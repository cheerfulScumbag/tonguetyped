use crate::coordinator::Coordinator;
use anyhow::Context;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut, ShortcutsChanged};
use ashpd::desktop::Session;
use futures_util::{Stream, StreamExt};
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// The id the desktop stores this app's activation shortcut under. It is part
/// of the shortcut's *persisted identity*, not a value that is safe to reuse.
///
/// KDE's GlobalShortcuts portal keeps the `preferred_trigger` an action was
/// first registered with forever, as the action's "Default shortcut", keyed by
/// id. Its shortcut editor refuses to delete a default, so a user who changes
/// the key ends up with the default *plus* the new binding. TongueTyped no
/// longer sends a preferred trigger (see [`SHORTCUT_DESCRIPTION`]), but any
/// install that once did still has `Meta+O` stored as the default for the old
/// `"activation"` id.
///
/// Registering under a fresh id gives the desktop a brand-new action with no
/// default: a new install starts truly unbound, and changing the key later
/// replaces it instead of stacking. [`LEGACY_SHORTCUT_IDS`] names the ids older
/// releases used so [`purge_legacy_shortcuts`] can drop them once.
///
/// Never change this id again without extending the migration, or every upgrade
/// would leave another stale action behind that still grabs its old key.
const SHORTCUT_ID: &str = "start-stop-dictation";

/// Shortcut ids registered by earlier TongueTyped releases. The desktop stores
/// these as real actions (including any old `Meta+O` default);
/// [`purge_legacy_shortcuts`] removes them so nothing is imposed or lingering.
const LEGACY_SHORTCUT_IDS: &[&str] = &["activation"];

/// The human-readable action description the desktop shows in its shortcut
/// settings. Per the XDG GlobalShortcuts portal spec a `preferred_trigger`
/// is what the desktop stores as the action's "Default shortcut"; passing
/// one made KDE display an app-chosen default (`Meta+O`) beside the binding
/// the user actually set. TongueTyped therefore registers the shortcut with
/// this description and **no** preferred/default trigger, so the desktop
/// presents only the user's own binding.
const SHORTCUT_DESCRIPTION: &str = "Start or stop dictation";
// A press is a single keystroke - the user either does it within a few
// seconds or the configured combo doesn't reach this app at all (wrong
// combo, grabbed by something else, desktop shortcut portal unavailable).
const SHORTCUT_PRESS_TIMEOUT: Duration = Duration::from_secs(15);
// `ConfigureShortcuts` opens the desktop's own native dialog and hands
// control to the user for as long as it takes them to press the new combo -
// generous on purpose, unlike the single-keystroke timeout above. Hitting it
// is not a failure: a desktop that routes the call to its own settings window
// (KDE opens System Settings) can be closed without changing anything, and no
// `ShortcutsChanged` event ever arrives, so the wait settles on the unchanged
// binding instead of reporting an error (see `ReconfigureOutcome::Unchanged`).
const SHORTCUT_RECONFIGURE_TIMEOUT: Duration = Duration::from_secs(120);
/// How often the daemon re-asserts its own binding. On KDE the action's
/// presence is one shared flag per application id, and any second
/// GlobalShortcuts session for the same id that goes away clears it. Binding
/// again on the daemon's existing session is cheap (the action already
/// exists, so no dialog appears) and re-marks the action present, so a stray
/// helper session cannot silently deactivate the shortcut for longer than
/// this interval. The first re-assert also runs shortly after startup to
/// cover a manual-daemon handover whose old session closes after the new
/// daemon's first bind.
const SHORTCUT_REASSERT_INTERVAL: Duration = Duration::from_secs(30);
const SHORTCUT_STARTUP_REASSERT_DELAY: Duration = Duration::from_secs(2);
/// Reconnect backoff after the listener loses its session or cannot bind:
/// short enough to recover from a portal restart promptly, capped so a
/// machine with no portal does not spin.
const RECONNECT_BASE_DELAY: Duration = Duration::from_secs(1);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);
/// Consecutive connect/bind failures tolerated before the listener reports
/// `Failed` instead of `Initializing`. The supervisor keeps retrying either
/// way; this only changes what the status truthfully reports.
const CONSECUTIVE_FAILURES_BEFORE_FAILED: u32 = 2;
const INSTALLED_APPLICATION_ID: &str = "io.github.cheerfulScumbag.tonguetyped";
const DEVELOPMENT_APPLICATION_ID: &str = "io.github.cheerfulScumbag.tonguetyped.Devel";
const DEVELOPMENT_DESKTOP_ENTRY: &str = concat!(
    include_str!("../data/io.github.cheerfulScumbag.tonguetyped.desktop"),
    "NoDisplay=true\n"
);

/// The one runtime every one-shot portal call runs on, kept alive for the
/// life of the process.
///
/// [`ashpd`] caches its `zbus::Connection` in a process-global `OnceLock`, and
/// zbus's tokio backend spawns that connection's socket-reader task on
/// whichever runtime first opened it. Building a fresh runtime per call tears
/// that reader down the moment the call returns, so every later portal call -
/// reopening the shortcut dialog, re-running the test - blocks forever inside
/// [`GlobalShortcuts::new`] before it reaches D-Bus. Routing every such call
/// through this shared runtime keeps the cached connection usable.
pub(crate) fn portal_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("failed to build the shared portal runtime")
    })
}

fn application_id() -> anyhow::Result<&'static str> {
    if !cfg!(debug_assertions) {
        return Ok(INSTALLED_APPLICATION_ID);
    }

    let data_dir = directories::BaseDirs::new()
        .context("could not determine the user data directory")?
        .data_dir()
        .to_owned();
    install_development_desktop_entry(&data_dir)?;
    Ok(DEVELOPMENT_APPLICATION_ID)
}

fn install_development_desktop_entry(data_dir: &Path) -> anyhow::Result<()> {
    let applications_dir = data_dir.join("applications");
    std::fs::create_dir_all(&applications_dir).with_context(|| {
        format!(
            "could not create desktop application directory {}",
            applications_dir.display()
        )
    })?;
    let path = applications_dir.join(format!("{DEVELOPMENT_APPLICATION_ID}.desktop"));
    std::fs::write(&path, DEVELOPMENT_DESKTOP_ENTRY).with_context(|| {
        format!(
            "could not write development desktop entry {}",
            path.display()
        )
    })
}

/// Registers TongueTyped's desktop application identity so a portal request
/// (the activation shortcut, or the libei RemoteDesktop session in
/// [`crate::libei`]) is attributed to this app rather than a bare connection.
/// Shared by both portal users so they resolve the same application id.
///
/// [`ashpd`] caches one process-global D-Bus connection, and the portal accepts
/// only one application id per connection: once any caller has registered, a
/// second `Register` is rejected with "Connection already associated with an
/// application ID". That is not a failure - the connection already carries
/// exactly the identity we want - so it must never fail the caller. The libei
/// path ([`crate::libei`]) opens the RemoteDesktop portal after the shortcut
/// listener has already registered on the same connection, and letting the
/// rejection escape there dropped every dictation into the clipboard fallback.
///
/// Registration is therefore idempotent: the first success is recorded and
/// later calls short-circuit, and the portal's "already associated" reply is
/// treated as success too, so a connection associated by some other path (or a
/// race between two first callers) still resolves.
pub(crate) async fn register_host_app() -> anyhow::Result<()> {
    if REGISTERED.load(Ordering::Acquire) {
        return Ok(());
    }
    match ashpd::register_host_app(application_id()?.try_into()?).await {
        Ok(()) => {
            REGISTERED.store(true, Ordering::Release);
            Ok(())
        }
        Err(error) if is_already_associated(&error) => {
            REGISTERED.store(true, Ordering::Release);
            Ok(())
        }
        Err(error) => Err(error).context(
            "desktop application identity is unavailable; sign out and back in after installing TongueTyped",
        ),
    }
}

/// Whether a `Register` call was rejected because the shared portal connection
/// already carries an application id.
///
/// The portal answers with `org.freedesktop.portal.Error.Failed` and the detail
/// "Could not register app ID: Connection already associated with an
/// application ID". Any other failure (a missing registry interface, a bus
/// error) is a real problem and stays an error, so this matches the error name
/// *and* the specific detail text.
fn is_already_associated(error: &ashpd::Error) -> bool {
    match error {
        ashpd::Error::Zbus(ashpd::zbus::Error::MethodError(name, detail, _)) => {
            already_associated_reply(name.as_str(), detail.as_deref())
        }
        _ => false,
    }
}

/// The pure predicate behind [`is_already_associated`], split out so the exact
/// error name and detail text are testable without a live portal.
fn already_associated_reply(name: &str, detail: Option<&str>) -> bool {
    name == "org.freedesktop.portal.Error.Failed"
        && detail.is_some_and(|detail| detail.contains("already associated with an application ID"))
}

/// Whether this process has already registered its application identity on the
/// shared portal connection. Only a successful registration (or the portal's
/// "already associated" reply) sets it, so a transient failure is retried.
static REGISTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The legacy shortcut ids the desktop currently knows about. Only ids the
/// desktop actually lists are returned, so a fresh install purges nothing and
/// never registers a legacy id just to remove it.
fn present_legacy_ids(known: &[String]) -> Vec<&'static str> {
    LEGACY_SHORTCUT_IDS
        .iter()
        .copied()
        .filter(|legacy| known.iter().any(|id| id == legacy))
        .collect()
}

/// Drops any activation shortcut left over from an older release.
///
/// The XDG GlobalShortcuts portal has no "unbind" call, but KDE's implementation
/// forgets every action of this app that a session knows about and then fails to
/// re-bind: binding a legacy id here marks it as ours, and the caller's
/// following bind of only [`SHORTCUT_ID`] leaves it out, so the portal removes
/// it (releasing its old key). Desktops whose implementation does not prune
/// un-rebound ids simply keep the legacy action as it already was, so this is
/// best-effort and never a reason to fail binding.
async fn purge_legacy_shortcuts<'a>(
    portal: &GlobalShortcuts<'a>,
    session: &Session<'a, GlobalShortcuts<'a>>,
) -> anyhow::Result<()> {
    let known: Vec<String> = portal
        .list_shortcuts(session)
        .await?
        .response()?
        .shortcuts()
        .iter()
        .map(|shortcut| shortcut.id().to_string())
        .collect();
    let legacy = present_legacy_ids(&known);
    if legacy.is_empty() {
        return Ok(());
    }
    tracing::info!("removing leftover activation shortcut ids {legacy:?}");
    let shortcuts: Vec<NewShortcut> = legacy
        .iter()
        .map(|id| NewShortcut::new(*id, SHORTCUT_DESCRIPTION))
        .collect();
    portal
        .bind_shortcuts(session, &shortcuts, None)
        .await?
        .response()?;
    Ok(())
}

/// Binds the activation shortcut on an already-created session with its id and
/// human description only - no `preferred_trigger`. The return value is
/// whatever trigger the desktop already has bound for the action (empty when
/// none), never an app-chosen key. Any stale shortcut from an older release is
/// purged first (see [`purge_legacy_shortcuts`]) so a user gets one action,
/// unbound, with no leftover default. Shared by the daemon supervisor and the
/// standalone `test_shortcut_binding()`/`reconfigure_shortcut()` paths, which
/// only run when no daemon owns the session.
async fn bind_activation_shortcut<'a>(
    portal: &GlobalShortcuts<'a>,
    session: &Session<'a, GlobalShortcuts<'a>>,
) -> anyhow::Result<String> {
    if let Err(error) = purge_legacy_shortcuts(portal, session).await {
        tracing::warn!("could not clear a leftover activation shortcut: {error}");
    }
    let shortcut = NewShortcut::new(SHORTCUT_ID, SHORTCUT_DESCRIPTION);
    let response = portal
        .bind_shortcuts(session, &[shortcut], None)
        .await?
        .response()?;
    response
        .shortcuts()
        .iter()
        .find(|shortcut| shortcut.id() == SHORTCUT_ID)
        .map(|shortcut| shortcut.trigger_description().to_string())
        .ok_or_else(|| anyhow::anyhow!("global shortcuts portal did not bind activation key"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutTestOutcome {
    /// The configured shortcut was actually pressed and observed via the
    /// portal's `Activated` signal - not just accepted by `BindShortcuts`.
    Pressed,
    TimedOut,
}

/// Standalone fallback: binds the shortcut on its own session and listens for
/// a press. Only used when no daemon is running - a daemon owns the session
/// otherwise, and this helper session's teardown would deactivate the shared
/// desktop action (see [`ActivationHandle`]).
///
/// Tests whatever trigger the desktop currently has bound for the action; the
/// user sets that trigger through the desktop's own dialog
/// (`reconfigure_shortcut`).
pub async fn test_shortcut_binding() -> Result<ShortcutTestOutcome, String> {
    test_shortcut_binding_inner()
        .await
        .map_err(|error| error.to_string())
}

async fn test_shortcut_binding_inner() -> anyhow::Result<ShortcutTestOutcome> {
    register_host_app().await?;
    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session().await?;
    bind_activation_shortcut(&portal, &session).await?;

    let mut activated = portal.receive_activated().await?;
    let wait_for_press = async {
        loop {
            let Some(event) = activated.next().await else {
                anyhow::bail!("global shortcuts event stream closed");
            };
            if event.shortcut_id() == SHORTCUT_ID {
                return Ok(());
            }
        }
    };
    match tokio::time::timeout(SHORTCUT_PRESS_TIMEOUT, wait_for_press).await {
        Ok(result) => result.map(|()| ShortcutTestOutcome::Pressed),
        Err(_) => Ok(ShortcutTestOutcome::TimedOut),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconfigureOutcome {
    /// The desktop reported a new trigger for the activation action.
    Changed { trigger_description: String },
    /// No change was observed: the user kept the current binding, closed the
    /// desktop's settings window without applying one, or the wait elapsed
    /// first. `trigger_description` is whatever the desktop still reports, and
    /// the caller must treat this as "nothing to apply", not a failure.
    ///
    /// KDE routes `ConfigureShortcuts` to System Settings rather than an
    /// inline dialog and sends no signal when that window is closed, so this
    /// is the only honest outcome for "closed without changing anything" -
    /// distinct from a real error (portal unavailable, event stream closed).
    Unchanged { trigger_description: String },
}

/// Opens the desktop's own native "press your new shortcut" dialog
/// (`GlobalShortcuts::configure_shortcuts`) so the user can set or change
/// the activation shortcut's trigger, then reports back whatever trigger is
/// really bound afterwards. This is the mechanism for setting a key now that
/// the registration ships no preferred/default trigger, and it works for the
/// first-ever bind exactly as it does for a rebind.
///
/// `cancel` lets the caller stop waiting early - the caller's UI settles on
/// the unchanged binding instead of the user being trapped until
/// [`SHORTCUT_RECONFIGURE_TIMEOUT`].
pub async fn reconfigure_shortcut(
    cancel: tokio::sync::oneshot::Receiver<()>,
) -> Result<ReconfigureOutcome, String> {
    reconfigure_shortcut_inner(cancel)
        .await
        .map_err(|error| error.to_string())
}

async fn reconfigure_shortcut_inner(
    cancel: tokio::sync::oneshot::Receiver<()>,
) -> anyhow::Result<ReconfigureOutcome> {
    register_host_app().await?;
    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session().await?;
    // `ConfigureShortcuts` requires a session that has already bound at
    // least one shortcut - this is that bind. It carries no preferred
    // trigger, so the dialog below is the only thing that sets the trigger.
    let bound = bind_activation_shortcut(&portal, &session).await?;

    // Subscribe before opening the dialog, not after: the dialog can close
    // (and emit ShortcutsChanged) at any point once ConfigureShortcuts is
    // called, and that call itself returns immediately without waiting for
    // the dialog - subscribing afterwards could miss the signal entirely.
    let mut changed = portal.receive_shortcuts_changed().await?;
    portal.configure_shortcuts(&session, None, None).await?;

    let observed = await_shortcut_trigger(
        &mut changed,
        cancel,
        SHORTCUT_RECONFIGURE_TIMEOUT,
        shortcut_trigger,
    )
    .await?;

    Ok(match observed {
        Some(trigger_description) => ReconfigureOutcome::Changed {
            trigger_description,
        },
        // No change was seen: report the freshest binding the desktop has, so
        // the UI settles on reality rather than a stale baseline. Falling back
        // to `bound` only if the read fails.
        None => ReconfigureOutcome::Unchanged {
            trigger_description: current_trigger(&portal, &session).await.unwrap_or(bound),
        },
    })
}

/// The activation action's new trigger from a `ShortcutsChanged` event, if the
/// event names it. Events for other ids are ignored.
fn shortcut_trigger(event: &ShortcutsChanged) -> Option<String> {
    event
        .shortcuts()
        .iter()
        .find(|shortcut| shortcut.id() == SHORTCUT_ID)
        .map(|shortcut| shortcut.trigger_description().to_string())
}

/// Reads the trigger the desktop currently has bound for [`SHORTCUT_ID`],
/// empty when it has none. Used to settle on the real binding when no
/// `ShortcutsChanged` event was observed.
async fn current_trigger<'a>(
    portal: &GlobalShortcuts<'a>,
    session: &Session<'a, GlobalShortcuts<'a>>,
) -> anyhow::Result<String> {
    let response = portal.list_shortcuts(session).await?.response()?;
    Ok(response
        .shortcuts()
        .iter()
        .find(|shortcut| shortcut.id() == SHORTCUT_ID)
        .map(|shortcut| shortcut.trigger_description().to_string())
        .unwrap_or_default())
}

/// Waits for a `ShortcutsChanged` event naming [`SHORTCUT_ID`] and returns its
/// new trigger. Returns `Ok(None)` when `cancel` fires or `timeout` elapses
/// with no change - both are "nothing to apply", never an error, so the caller
/// can settle on the unchanged binding instead of reporting a failure.
///
/// Generic over the stream item and a `trigger` extractor so the wait/settle
/// logic is unit-testable without a live portal.
async fn await_shortcut_trigger<S, T, F>(
    events: &mut S,
    cancel: tokio::sync::oneshot::Receiver<()>,
    timeout: Duration,
    trigger: F,
) -> anyhow::Result<Option<String>>
where
    S: Stream<Item = T> + Unpin,
    F: Fn(&T) -> Option<String>,
{
    let wait_for_change = async {
        loop {
            let Some(event) = events.next().await else {
                anyhow::bail!("global shortcuts event stream closed");
            };
            if let Some(trigger) = trigger(&event) {
                return Ok(trigger);
            }
        }
    };
    tokio::select! {
        result = tokio::time::timeout(timeout, wait_for_change) => match result {
            Ok(inner) => inner.map(Some),
            Err(_) => Ok(None),
        },
        _ = cancel => Ok(None),
    }
}

/// Rewrites the portal's trigger description into TongueTyped's own wording
/// for display: KDE calls the Super key `Meta`, so `Meta+O` becomes
/// `Super+O`. The stored value is only ever what the desktop reports is
/// bound - this is pure display normalization, never something registered
/// back to the desktop.
pub fn keybind_label(reported: &str) -> String {
    reported.replace("Meta", "Super")
}

/// The user-facing rendering of whatever the desktop reports is bound:
/// `keybind_label`, or a friendly placeholder when nothing is bound yet.
pub fn keybind_display(reported: &str) -> String {
    if reported.trim().is_empty() {
        "(none set yet)".to_string()
    } else {
        keybind_label(reported)
    }
}

/// A command the daemon's IPC dispatcher can send to the one task that owns
/// the GlobalShortcuts session.
pub enum ActivationCommand {
    Test {
        reply: oneshot::Sender<Result<ShortcutTestOutcome, String>>,
    },
    Configure {
        reply: oneshot::Sender<Result<ReconfigureOutcome, String>>,
    },
    CancelConfigure,
}

/// The daemon's handle to its activation supervisor.
///
/// Shortcut testing and reconfiguring go through this instead of creating a
/// second GlobalShortcuts session. On KDE a session's application id is the
/// KGlobalAccel component, and the component's action has one shared
/// "present" flag: destroying any helper session's QAction marks the action
/// inactive and unregisters the key grab, while the daemon's still-connected
/// listener sees nothing. Making the daemon the only owner removes that
/// competing session entirely.
#[derive(Clone)]
pub struct ActivationHandle {
    commands: mpsc::Sender<ActivationCommand>,
}

impl ActivationHandle {
    pub async fn test_shortcut(&self) -> Result<ShortcutTestOutcome, String> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(ActivationCommand::Test { reply })
            .await
            .map_err(|_| "the daemon's shortcut listener is not running".to_string())?;
        result
            .await
            .map_err(|_| "the daemon's shortcut listener stopped before replying".to_string())?
    }

    pub async fn configure_shortcut(&self) -> Result<ReconfigureOutcome, String> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(ActivationCommand::Configure { reply })
            .await
            .map_err(|_| "the daemon's shortcut listener is not running".to_string())?;
        result
            .await
            .map_err(|_| "the daemon's shortcut listener stopped before replying".to_string())?
    }

    pub async fn cancel_configure(&self) {
        let _ = self.commands.send(ActivationCommand::CancelConfigure).await;
    }

    #[cfg(test)]
    pub(crate) fn for_test(commands: mpsc::Sender<ActivationCommand>) -> Self {
        Self { commands }
    }
}

/// Starts the daemon's activation supervisor and returns its control handle.
///
/// The supervisor owns the process's only GlobalShortcuts session for as long
/// as it can hold one. It rebinds on session close or a failed bind, retries
/// with bounded backoff, and periodically re-asserts its binding so a stray
/// helper session (another build, a leftover process) cannot leave the shared
/// desktop action inactive. This mirrors `libei`'s "one session owned by the
/// process, reconnect on death" pattern.
pub fn spawn(coordinator: Arc<Coordinator>) -> ActivationHandle {
    let (commands, receiver) = mpsc::channel(8);
    tokio::spawn(supervise(
        PortalConnector,
        coordinator,
        receiver,
        ReassertSchedule::default(),
    ));
    ActivationHandle { commands }
}

/// Bounded exponential reconnect backoff, split out so the policy is testable
/// without a portal.
#[derive(Debug, Default)]
pub struct RetryBackoff {
    attempt: u32,
}

impl RetryBackoff {
    fn reset(&mut self) {
        self.attempt = 0;
    }

    fn next_delay(&mut self) -> Duration {
        // 1s, 2s, 4s, 8s, 16s, then the 30s cap.
        let shift = self.attempt.min(5);
        let delay = RECONNECT_BASE_DELAY
            .saturating_mul(1u32 << shift)
            .min(RECONNECT_MAX_DELAY);
        self.attempt = self.attempt.saturating_add(1);
        delay
    }
}

/// A session with a key bound is `Available`; one the desktop reports with no
/// key is `Unbound`, never a silent "available".
fn bound_status(trigger: &str) -> crate::ipc::ShortcutStatus {
    if trigger.trim().is_empty() {
        crate::ipc::ShortcutStatus::Unbound
    } else {
        crate::ipc::ShortcutStatus::Available
    }
}

/// Every signal the supervisor needs from the desktop, merged into one stream
/// so the listener (and a headless test) has a single source of events.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionEvent {
    Activated {
        shortcut_id: String,
        timestamp: Duration,
    },
    Deactivated {
        shortcut_id: String,
        timestamp: Duration,
    },
    ShortcutChanged {
        shortcut_id: String,
        trigger_description: String,
    },
    SessionClosed,
    /// The underlying signal stream ended. Any one of them ending means the
    /// session can no longer be trusted, so the supervisor reconnects.
    StreamEnded,
}

type EventStream = Pin<Box<dyn Stream<Item = SessionEvent> + Send>>;

/// Appends [`SessionEvent::StreamEnded`] to a signal stream, so `select_all`
/// reports a single stream's end as a reconnect rather than waiting for all of
/// them to end.
fn end_marked<S>(stream: S) -> EventStream
where
    S: Stream<Item = SessionEvent> + Send + 'static,
{
    Box::pin(stream.chain(futures_util::stream::once(async {
        SessionEvent::StreamEnded
    })))
}

type SessionFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// The supervisor's only view of a live portal session. Implemented by the real
/// [`Binding`] and by headless fakes in the tests, so reconnection, periodic
/// re-assertion, and portal-restart recovery can be exercised without a portal.
trait ShortcutSession: Send {
    fn events(&mut self) -> &mut EventStream;
    fn trigger(&self) -> &str;
    fn set_trigger(&mut self, trigger: String);
    fn reassert(&mut self) -> SessionFuture<'_, anyhow::Result<String>>;
    fn open_configure(&mut self) -> SessionFuture<'_, anyhow::Result<()>>;
}

/// Creates sessions on demand. The supervisor calls this again after every
/// loss, which is exactly the reconnect path.
trait SessionConnector: Send + 'static {
    type Session: ShortcutSession + 'static;
    fn connect(&mut self) -> SessionFuture<'static, anyhow::Result<Self::Session>>;
}

struct PortalConnector;

impl SessionConnector for PortalConnector {
    type Session = Binding;

    fn connect(&mut self) -> SessionFuture<'static, anyhow::Result<Binding>> {
        Box::pin(connect())
    }
}

/// The effects the supervisor reports. Implemented by [`Coordinator`] in
/// production, so the loop can run headlessly against a recording fake.
trait SupervisorSink: Send + Sync {
    fn binding_reported(&self, trigger: String);
    fn shortcut_state(&self, status: crate::ipc::ShortcutStatus, error: Option<String>);
    fn shortcut_failed(&self, error: String);
    fn activation(&self, pressed: bool) -> SessionFuture<'_, ()>;
}

impl SupervisorSink for Coordinator {
    fn binding_reported(&self, trigger: String) {
        self.record_activation_binding(trigger);
    }

    fn shortcut_state(&self, status: crate::ipc::ShortcutStatus, error: Option<String>) {
        self.set_shortcut_state(status, error);
    }

    fn shortcut_failed(&self, error: String) {
        self.set_runtime_error(error);
    }

    fn activation(&self, pressed: bool) -> SessionFuture<'_, ()> {
        Box::pin(async move {
            if let Err(error) = self.handle_activation(pressed).await {
                tracing::error!("activation command failed: {error}");
            }
        })
    }
}

/// When to re-assert the binding. Bounds are injectable so the recovery loop is
/// testable without waiting through the production 30-second interval.
#[derive(Debug, Clone, Copy)]
struct ReassertSchedule {
    startup_delay: Duration,
    interval: Duration,
}

impl Default for ReassertSchedule {
    fn default() -> Self {
        Self {
            startup_delay: SHORTCUT_STARTUP_REASSERT_DELAY,
            interval: SHORTCUT_REASSERT_INTERVAL,
        }
    }
}

struct Binding {
    portal: GlobalShortcuts<'static>,
    session: Session<'static, GlobalShortcuts<'static>>,
    events: EventStream,
    /// The trigger the desktop last reported, in the desktop's own wording.
    trigger: String,
}

impl ShortcutSession for Binding {
    fn events(&mut self) -> &mut EventStream {
        &mut self.events
    }

    fn trigger(&self) -> &str {
        &self.trigger
    }

    fn set_trigger(&mut self, trigger: String) {
        self.trigger = trigger;
    }

    fn reassert(&mut self) -> SessionFuture<'_, anyhow::Result<String>> {
        Box::pin(async move { bind_activation_shortcut(&self.portal, &self.session).await })
    }

    fn open_configure(&mut self) -> SessionFuture<'_, anyhow::Result<()>> {
        Box::pin(async move {
            self.portal
                .configure_shortcuts(&self.session, None, None)
                .await?;
            Ok(())
        })
    }
}

async fn connect() -> anyhow::Result<Binding> {
    register_host_app()
        .await
        .context("could not register the desktop application identity")?;
    let portal = GlobalShortcuts::new()
        .await
        .context("global shortcuts portal unavailable")?;
    let session = portal
        .create_session()
        .await
        .context("could not create a global shortcuts session")?;
    let trigger = bind_activation_shortcut(&portal, &session).await?;

    let activated = portal
        .receive_activated()
        .await
        .context("could not subscribe to shortcut activations")?
        .map(|event| SessionEvent::Activated {
            shortcut_id: event.shortcut_id().to_string(),
            timestamp: event.timestamp(),
        });
    let deactivated = portal
        .receive_deactivated()
        .await
        .context("could not subscribe to shortcut deactivations")?
        .map(|event| SessionEvent::Deactivated {
            shortcut_id: event.shortcut_id().to_string(),
            timestamp: event.timestamp(),
        });
    let changed = portal
        .receive_shortcuts_changed()
        .await
        .context("could not subscribe to shortcut changes")?
        .flat_map(|event| {
            let changes: Vec<SessionEvent> = event
                .shortcuts()
                .iter()
                .map(|shortcut| SessionEvent::ShortcutChanged {
                    shortcut_id: shortcut.id().to_string(),
                    trigger_description: shortcut.trigger_description().to_string(),
                })
                .collect();
            futures_util::stream::iter(changes)
        });
    let closed = session
        .receive_closed()
        .await
        .context("could not watch the shortcut session")?
        .map(|()| SessionEvent::SessionClosed);
    let events = futures_util::stream::select_all(vec![
        end_marked(activated),
        end_marked(deactivated),
        end_marked(changed),
        end_marked(closed),
    ]);
    Ok(Binding {
        portal,
        session,
        events: Box::pin(events),
        trigger,
    })
}

async fn supervise<K, S, C>(
    mut connector: K,
    coordinator: Arc<C>,
    mut commands: mpsc::Receiver<ActivationCommand>,
    schedule: ReassertSchedule,
) where
    K: SessionConnector<Session = S>,
    S: ShortcutSession + 'static,
    C: SupervisorSink + 'static,
{
    let mut backoff = RetryBackoff::default();
    let mut consecutive_failures: u32 = 0;
    loop {
        match connector.connect().await {
            Ok(mut session) => {
                backoff.reset();
                consecutive_failures = 0;
                // The bind above is the app's only source of truth for what is
                // actually bound: persist whatever the desktop reports so
                // `config` never claims a key TongueTyped chose.
                coordinator.binding_reported(session.trigger().to_string());
                coordinator.shortcut_state(bound_status(session.trigger()), None);
                match run_bound(&*coordinator, &mut session, &mut commands, schedule).await {
                    SessionEnd::CommandsClosed => return,
                    SessionEnd::Lost(error) => {
                        let message =
                            format!("activation listener lost its session: {error}; reconnecting");
                        tracing::warn!("{message}");
                        coordinator.shortcut_state(
                            crate::ipc::ShortcutStatus::Initializing,
                            Some(message),
                        );
                    }
                }
            }
            Err(error) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                let message = format!("activation listener failed: {error}; retrying");
                tracing::warn!("{message}");
                if consecutive_failures >= CONSECUTIVE_FAILURES_BEFORE_FAILED {
                    coordinator.shortcut_failed(message);
                } else {
                    coordinator
                        .shortcut_state(crate::ipc::ShortcutStatus::Initializing, Some(message));
                }
            }
        }

        let delay = backoff.next_delay();
        let wait = tokio::time::sleep(delay);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                _ = &mut wait => break,
                command = commands.recv() => match command {
                    Some(command) => reply_while_reconnecting(command),
                    None => return,
                },
            }
        }
    }
}

fn reply_while_reconnecting(command: ActivationCommand) {
    let message = "the shortcut listener is reconnecting; try again in a moment".to_string();
    match command {
        ActivationCommand::Test { reply } => {
            let _ = reply.send(Err(message));
        }
        ActivationCommand::Configure { reply } => {
            let _ = reply.send(Err(message));
        }
        ActivationCommand::CancelConfigure => {}
    }
}

enum SessionEnd {
    CommandsClosed,
    Lost(String),
}

/// Why `configure_on_session` returned. A lost session must end the whole
/// listener so the supervisor reconnects, not just fail the dialog request.
enum ConfigureEnd {
    Finished(Result<ReconfigureOutcome, String>),
    SessionLost(String),
}

/// One wake-up from the `run_bound` select. Handled after the borrow of the
/// session's event stream ends, so the session can be used mutably again.
enum Wake {
    Event(Option<SessionEvent>),
    Command(Option<ActivationCommand>),
    TestTimeout,
    Reassert,
}

enum ConfigureWake {
    Event(Option<SessionEvent>),
    Command(Option<ActivationCommand>),
    Deadline,
    Reassert,
}

struct TestWait {
    reply: oneshot::Sender<Result<ShortcutTestOutcome, String>>,
    deadline: tokio::time::Instant,
}

/// Re-asserts the binding on the current session and reports the trigger the
/// desktop reports. A failure means the session is no longer usable (a portal
/// restart invalidates it without emitting `Session.Closed`), so the caller
/// must treat it as session loss and reconnect.
async fn reassert_binding<S, C>(session: &mut S, coordinator: &C) -> Result<(), String>
where
    S: ShortcutSession,
    C: SupervisorSink + ?Sized,
{
    let trigger = session
        .reassert()
        .await
        .map_err(|error| format!("shortcut rebind failed: {error}"))?;
    session.set_trigger(trigger.clone());
    coordinator.binding_reported(trigger.clone());
    coordinator.shortcut_state(bound_status(&trigger), None);
    Ok(())
}

async fn run_bound<S, C>(
    coordinator: &C,
    session: &mut S,
    commands: &mut mpsc::Receiver<ActivationCommand>,
    schedule: ReassertSchedule,
) -> SessionEnd
where
    S: ShortcutSession,
    C: SupervisorSink + ?Sized,
{
    let mut event_order = EventOrder::default();
    let mut test_wait: Option<TestWait> = None;
    // A test press is consumed by the test rather than toggling dictation;
    // its matching release is swallowed too so the hold-release ordering does
    // not see a release with no press.
    let mut swallow_release = false;
    let mut reassert = tokio::time::interval_at(
        tokio::time::Instant::now() + schedule.startup_delay,
        schedule.interval,
    );
    reassert.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Events are filtered by shortcut id, not by originating session:
    // `ashpd`'s `Session::path()` is `pub(crate)`, so the app cannot compare
    // `Activated::session_handle()` with its own session. The single-owner
    // rule is the defense - the daemon holds the process's only session, so
    // there is no other TongueTyped session whose presses could fan out here.
    loop {
        let test_timer = sleep_until_optional(test_wait.as_ref().map(|wait| wait.deadline));
        let wake = {
            let events = session.events();
            tokio::select! {
                event = events.next() => Wake::Event(event),
                command = commands.recv() => Wake::Command(command),
                _ = test_timer => Wake::TestTimeout,
                _ = reassert.tick() => Wake::Reassert,
            }
        };
        match wake {
            Wake::Event(None) | Wake::Event(Some(SessionEvent::StreamEnded)) => {
                return SessionEnd::Lost("the global shortcuts event stream ended".to_string());
            }
            Wake::Event(Some(SessionEvent::SessionClosed)) => {
                return SessionEnd::Lost(
                    "the desktop closed the global shortcuts session".to_string(),
                );
            }
            Wake::Event(Some(SessionEvent::Activated {
                shortcut_id,
                timestamp,
            })) => {
                if shortcut_id != SHORTCUT_ID {
                    continue;
                }
                if let Some(wait) = test_wait.take() {
                    swallow_release = true;
                    let _ = wait.reply.send(Ok(ShortcutTestOutcome::Pressed));
                    continue;
                }
                for pressed in event_order.push(timestamp, true) {
                    coordinator.activation(pressed).await;
                }
            }
            Wake::Event(Some(SessionEvent::Deactivated {
                shortcut_id,
                timestamp,
            })) => {
                if shortcut_id != SHORTCUT_ID {
                    continue;
                }
                if swallow_release {
                    swallow_release = false;
                    continue;
                }
                for pressed in event_order.push(timestamp, false) {
                    coordinator.activation(pressed).await;
                }
            }
            // A change outside the dialog is not a new binding; the desktop
            // only emits it for the editor this app opens.
            Wake::Event(Some(SessionEvent::ShortcutChanged { .. })) => {}
            Wake::Command(None) => return SessionEnd::CommandsClosed,
            Wake::Command(Some(ActivationCommand::Test { reply })) => {
                if test_wait.is_some() {
                    let _ = reply.send(Err("a shortcut test is already running".to_string()));
                } else {
                    test_wait = Some(TestWait {
                        reply,
                        deadline: tokio::time::Instant::now() + SHORTCUT_PRESS_TIMEOUT,
                    });
                }
            }
            Wake::Command(Some(ActivationCommand::Configure { reply })) => {
                if test_wait.is_some() {
                    // Let the test finish rather than silently swallowing the
                    // press the user is about to make.
                    let _ = reply.send(Err(
                        "finish the running shortcut test before configuring".to_string()
                    ));
                } else {
                    match configure_on_session(
                        session,
                        commands,
                        coordinator,
                        &mut event_order,
                        &mut swallow_release,
                        &mut reassert,
                    )
                    .await
                    {
                        ConfigureEnd::Finished(result) => {
                            let _ = reply.send(result);
                        }
                        ConfigureEnd::SessionLost(error) => {
                            let _ = reply.send(Err(error.clone()));
                            return SessionEnd::Lost(error);
                        }
                    }
                }
            }
            Wake::Command(Some(ActivationCommand::CancelConfigure)) => {}
            Wake::TestTimeout => {
                if let Some(wait) = test_wait.take() {
                    let _ = wait.reply.send(Ok(ShortcutTestOutcome::TimedOut));
                }
            }
            Wake::Reassert => {
                if let Err(error) = reassert_binding(session, coordinator).await {
                    return SessionEnd::Lost(error);
                }
            }
        }
    }
}

async fn sleep_until_optional(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Runs the desktop's shortcut editor on the daemon's own session and waits
/// for the reported trigger. Kept inline in the listener because a session is
/// not cloneable: the alternative would be a second session, which is the bug.
/// Presses still reach dictation while the editor is open, and the periodic
/// re-assert keeps running. A lost session or a failed re-assert ends the
/// listener so the supervisor reconnects instead of settling on a dead one.
async fn configure_on_session<S, C>(
    session: &mut S,
    commands: &mut mpsc::Receiver<ActivationCommand>,
    coordinator: &C,
    event_order: &mut EventOrder,
    swallow_release: &mut bool,
    reassert: &mut tokio::time::Interval,
) -> ConfigureEnd
where
    S: ShortcutSession,
    C: SupervisorSink + ?Sized,
{
    if let Err(error) = session.open_configure().await {
        return ConfigureEnd::Finished(Err(format!(
            "could not open the desktop shortcut editor: {error}"
        )));
    }

    let deadline = tokio::time::Instant::now() + SHORTCUT_RECONFIGURE_TIMEOUT;
    loop {
        let wake = {
            let events = session.events();
            tokio::select! {
                event = events.next() => ConfigureWake::Event(event),
                _ = tokio::time::sleep_until(deadline) => ConfigureWake::Deadline,
                command = commands.recv() => ConfigureWake::Command(command),
                _ = reassert.tick() => ConfigureWake::Reassert,
            }
        };
        match wake {
            ConfigureWake::Event(None) | ConfigureWake::Event(Some(SessionEvent::StreamEnded)) => {
                return ConfigureEnd::SessionLost(
                    "the global shortcuts event stream ended while configuring".to_string(),
                );
            }
            ConfigureWake::Event(Some(SessionEvent::SessionClosed)) => {
                return ConfigureEnd::SessionLost(
                    "the desktop closed the global shortcuts session while configuring".to_string(),
                );
            }
            ConfigureWake::Event(Some(SessionEvent::ShortcutChanged {
                shortcut_id,
                trigger_description,
            })) => {
                if shortcut_id != SHORTCUT_ID {
                    continue;
                }
                session.set_trigger(trigger_description.clone());
                coordinator.binding_reported(trigger_description.clone());
                coordinator.shortcut_state(bound_status(&trigger_description), None);
                return ConfigureEnd::Finished(Ok(ReconfigureOutcome::Changed {
                    trigger_description,
                }));
            }
            ConfigureWake::Event(Some(SessionEvent::Activated {
                shortcut_id,
                timestamp,
            })) => {
                if shortcut_id == SHORTCUT_ID {
                    for pressed in event_order.push(timestamp, true) {
                        coordinator.activation(pressed).await;
                    }
                }
            }
            ConfigureWake::Event(Some(SessionEvent::Deactivated {
                shortcut_id,
                timestamp,
            })) => {
                if shortcut_id == SHORTCUT_ID {
                    if *swallow_release {
                        *swallow_release = false;
                    } else {
                        for pressed in event_order.push(timestamp, false) {
                            coordinator.activation(pressed).await;
                        }
                    }
                }
            }
            ConfigureWake::Command(None) => {
                return ConfigureEnd::Finished(Err("the daemon is shutting down".to_string()));
            }
            ConfigureWake::Command(Some(ActivationCommand::CancelConfigure)) => {
                return ConfigureEnd::Finished(Ok(ReconfigureOutcome::Unchanged {
                    trigger_description: session.trigger().to_string(),
                }));
            }
            ConfigureWake::Command(Some(ActivationCommand::Test { reply })) => {
                let _ = reply.send(Err("a shortcut configuration dialog is open".to_string()));
            }
            ConfigureWake::Command(Some(ActivationCommand::Configure { reply })) => {
                let _ = reply.send(Err(
                    "a shortcut configuration dialog is already open".to_string()
                ));
            }
            ConfigureWake::Deadline => {
                return ConfigureEnd::Finished(Ok(ReconfigureOutcome::Unchanged {
                    trigger_description: session.trigger().to_string(),
                }));
            }
            ConfigureWake::Reassert => {
                if let Err(error) = reassert_binding(session, coordinator).await {
                    return ConfigureEnd::SessionLost(error);
                }
            }
        }
    }
}

#[derive(Default)]
struct EventOrder {
    press_timestamp: Option<std::time::Duration>,
    pending_release: Option<std::time::Duration>,
}

impl EventOrder {
    fn push(&mut self, timestamp: std::time::Duration, pressed: bool) -> Vec<bool> {
        if pressed {
            self.press_timestamp = Some(timestamp);
            let mut events = vec![true];
            if self
                .pending_release
                .take()
                .is_some_and(|release| timestamp <= release)
            {
                self.press_timestamp = None;
                events.push(false);
            }
            events
        } else if let Some(press_timestamp) = self.press_timestamp {
            if timestamp < press_timestamp {
                Vec::new()
            } else {
                self.press_timestamp = None;
                vec![false]
            }
        } else {
            self.pending_release = Some(timestamp);
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    #[test]
    fn installs_discoverable_development_application_identity() {
        let data_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("activation-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);

        install_development_desktop_entry(&data_dir).unwrap();

        let entry = data_dir
            .join("applications")
            .join(format!("{DEVELOPMENT_APPLICATION_ID}.desktop"));
        assert_eq!(
            std::fs::read_to_string(entry).unwrap(),
            DEVELOPMENT_DESKTOP_ENTRY
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn reports_kde_wording_as_the_apps_own_super_modifier() {
        assert_eq!(keybind_label("Meta+O"), "Super+O");
        assert_eq!(keybind_label("Ctrl+Meta+Space"), "Ctrl+Super+Space");
        assert_eq!(keybind_label("Ctrl+Shift+Space"), "Ctrl+Shift+Space");
        assert_eq!(keybind_label(""), "");
    }

    #[test]
    fn activation_shortcut_does_not_reuse_a_legacy_id() {
        // A reused id would inherit KDE's persisted "default shortcut" and make
        // a rebind stack old + new (see the `SHORTCUT_ID` doc comment).
        assert!(!LEGACY_SHORTCUT_IDS.contains(&SHORTCUT_ID));
        assert!(!SHORTCUT_ID.is_empty());
    }

    #[test]
    fn only_legacy_ids_the_desktop_reports_are_purged() {
        let known = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        assert_eq!(present_legacy_ids(&known(&[])), Vec::<&str>::new());
        assert_eq!(
            present_legacy_ids(&known(&[SHORTCUT_ID])),
            Vec::<&str>::new()
        );
        assert_eq!(
            present_legacy_ids(&known(&["activation"])),
            vec!["activation"]
        );
        assert_eq!(
            present_legacy_ids(&known(&["activation", SHORTCUT_ID])),
            vec!["activation"]
        );
    }

    #[test]
    fn orders_release_that_arrives_before_earlier_press() {
        let mut order = EventOrder::default();
        assert!(order
            .push(std::time::Duration::from_millis(20), false)
            .is_empty());
        assert_eq!(
            order.push(std::time::Duration::from_millis(10), true),
            [true, false]
        );
    }

    #[test]
    fn ignores_late_delivery_of_release_before_active_press() {
        let mut order = EventOrder::default();
        assert_eq!(
            order.push(std::time::Duration::from_millis(20), true),
            [true]
        );
        assert!(order
            .push(std::time::Duration::from_millis(10), false)
            .is_empty());
        assert_eq!(
            order.push(std::time::Duration::from_millis(30), false),
            [false]
        );
    }

    /// A change event for another action is ignored; the one for this action
    /// wins.
    fn trigger_of(event: &Vec<(&'static str, &'static str)>) -> Option<String> {
        event
            .iter()
            .find(|(id, _)| *id == SHORTCUT_ID)
            .map(|(_, trigger)| trigger.to_string())
    }

    #[tokio::test]
    async fn shortcut_change_reports_the_new_trigger() {
        let mut events = futures_util::stream::iter(vec![
            vec![("some-other-action", "Ctrl+B")],
            vec![(SHORTCUT_ID, "Ctrl+Meta+K")],
        ]);
        let (_sender, cancel) = tokio::sync::oneshot::channel();

        let observed =
            await_shortcut_trigger(&mut events, cancel, Duration::from_secs(5), trigger_of)
                .await
                .unwrap();

        assert_eq!(observed.as_deref(), Some("Ctrl+Meta+K"));
    }

    #[tokio::test]
    async fn cancelled_shortcut_wait_settles_with_no_change() {
        let mut events = futures_util::stream::pending::<Vec<(&'static str, &'static str)>>();
        let (sender, cancel) = tokio::sync::oneshot::channel();
        sender.send(()).unwrap();

        let observed =
            await_shortcut_trigger(&mut events, cancel, Duration::from_secs(5), trigger_of)
                .await
                .unwrap();

        assert_eq!(observed, None);
    }

    #[tokio::test]
    async fn timed_out_shortcut_wait_settles_with_no_change() {
        let mut events = futures_util::stream::pending::<Vec<(&'static str, &'static str)>>();
        let (_sender, cancel) = tokio::sync::oneshot::channel();

        let observed =
            await_shortcut_trigger(&mut events, cancel, Duration::from_millis(20), trigger_of)
                .await
                .unwrap();

        assert_eq!(observed, None);
    }

    #[tokio::test]
    async fn closed_shortcut_event_stream_is_an_error() {
        let mut events =
            futures_util::stream::iter(Vec::<Vec<(&'static str, &'static str)>>::new());
        let (_sender, cancel) = tokio::sync::oneshot::channel();

        let result =
            await_shortcut_trigger(&mut events, cancel, Duration::from_secs(5), trigger_of).await;

        assert!(
            result.is_err(),
            "a closed stream is a real failure, not a no-change settle"
        );
    }

    /// Live check against the running desktop portal: registering twice must
    /// succeed. The portal rejects a second `Register` on the same connection
    /// with "already associated with an application ID" - the exact error that
    /// dropped the libei typing path into the clipboard fallback, because the
    /// shortcut listener registers first and the RemoteDesktop portal then
    /// registers again on the same shared connection. Ignored by default: it
    /// needs a live `xdg-desktop-portal` exposing the host registry interface
    /// (run with `cargo test -- --ignored a_second_registration`).
    #[tokio::test]
    #[ignore = "needs a live xdg-desktop-portal with the host registry interface"]
    async fn a_second_registration_on_the_shared_connection_is_tolerated() {
        // The shortcut listener and the libei RemoteDesktop session share one
        // process-global portal connection, which accepts only one application
        // id. Register the identity, then force a second registration onto the
        // portal (rather than letting the memo short-circuit it) and prove the
        // wrapper turns the portal's rejection into success.
        REGISTERED.store(false, Ordering::Release);
        register_host_app().await.expect("first registration");

        // The shared connection now carries the identity, so the portal rejects
        // a second raw `Register` with the exact reply the daemon log reported.
        // Asserting this proves the test is exercising the already-associated
        // branch rather than passing trivially on an idempotent portal.
        REGISTERED.store(false, Ordering::Release);
        let app_id: ashpd::AppID = application_id()
            .expect("application id")
            .try_into()
            .expect("valid application id");
        let raw = ashpd::register_host_app(app_id).await;
        assert!(
            matches!(&raw, Err(error) if is_already_associated(error)),
            "a second registration on the shared connection must be rejected as \
             already associated: {raw:?}"
        );

        // The wrapper must treat that rejection as success.
        REGISTERED.store(false, Ordering::Release);
        register_host_app()
            .await
            .expect("a second registration on the shared connection must be tolerated");
    }

    #[test]
    fn only_the_already_associated_reply_is_tolerated() {
        // The exact reply KDE sends on the second registration over the shared
        // portal connection (the daemon log's anchor). It means the connection
        // already has the identity we want, so it must be treated as success.
        assert!(already_associated_reply(
            "org.freedesktop.portal.Error.Failed",
            Some("Could not register app ID: Connection already associated with an application ID"),
        ));
    }

    #[test]
    fn other_registration_failures_stay_errors() {
        // A different detail under the same portal error name is a real failure.
        assert!(!already_associated_reply(
            "org.freedesktop.portal.Error.Failed",
            Some("Could not register app ID: Invalid application ID"),
        ));
        // The right detail under a different error name is not the reply we mean.
        assert!(!already_associated_reply(
            "org.freedesktop.DBus.Error.Failed",
            Some("Connection already associated with an application ID"),
        ));
        // A failure with no detail cannot be recognized as the tolerated reply.
        assert!(!already_associated_reply(
            "org.freedesktop.portal.Error.Failed",
            None,
        ));
    }

    #[test]
    fn reconfigure_outcome_distinguishes_changed_from_unchanged() {
        let changed = ReconfigureOutcome::Changed {
            trigger_description: "Ctrl+A".to_string(),
        };
        let unchanged = ReconfigureOutcome::Unchanged {
            trigger_description: "Ctrl+A".to_string(),
        };
        assert!(matches!(changed, ReconfigureOutcome::Changed { .. }));
        assert!(matches!(unchanged, ReconfigureOutcome::Unchanged { .. }));
        assert_ne!(changed, unchanged);
    }

    #[test]
    fn reconnect_backoff_grows_then_stays_bounded() {
        let mut backoff = RetryBackoff::default();
        let delays: Vec<Duration> = (0..8).map(|_| backoff.next_delay()).collect();
        assert_eq!(delays[0], Duration::from_secs(1));
        assert_eq!(delays[1], Duration::from_secs(2));
        assert_eq!(delays[2], Duration::from_secs(4));
        assert_eq!(delays[3], Duration::from_secs(8));
        assert_eq!(delays[4], Duration::from_secs(16));
        assert_eq!(delays[5], RECONNECT_MAX_DELAY);
        assert_eq!(delays[7], RECONNECT_MAX_DELAY);
        assert!(delays.iter().all(|delay| *delay <= RECONNECT_MAX_DELAY));

        // A successful bind resets the sequence to the first short delay.
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    }

    #[test]
    fn a_bound_action_with_no_key_is_unbound_not_available() {
        assert_eq!(
            bound_status("Super+O"),
            crate::ipc::ShortcutStatus::Available
        );
        assert_eq!(bound_status(""), crate::ipc::ShortcutStatus::Unbound);
        assert_eq!(bound_status("   "), crate::ipc::ShortcutStatus::Unbound);
    }
    #[derive(Default)]
    struct FakeSink {
        bindings: Mutex<Vec<String>>,
        states: Mutex<Vec<(crate::ipc::ShortcutStatus, Option<String>)>>,
        activations: Mutex<Vec<bool>>,
    }

    impl SupervisorSink for FakeSink {
        fn binding_reported(&self, trigger: String) {
            self.bindings.lock().unwrap().push(trigger);
        }

        fn shortcut_state(&self, status: crate::ipc::ShortcutStatus, error: Option<String>) {
            self.states.lock().unwrap().push((status, error));
        }

        fn shortcut_failed(&self, error: String) {
            self.states
                .lock()
                .unwrap()
                .push((crate::ipc::ShortcutStatus::Failed, Some(error)));
        }

        fn activation(&self, pressed: bool) -> SessionFuture<'_, ()> {
            Box::pin(async move {
                self.activations.lock().unwrap().push(pressed);
            })
        }
    }

    struct FakeSession {
        events: EventStream,
        trigger: String,
        reasserts: Arc<Mutex<VecDeque<Result<String, String>>>>,
        reassert_count: Arc<AtomicUsize>,
    }

    impl ShortcutSession for FakeSession {
        fn events(&mut self) -> &mut EventStream {
            &mut self.events
        }

        fn trigger(&self) -> &str {
            &self.trigger
        }

        fn set_trigger(&mut self, trigger: String) {
            self.trigger = trigger;
        }

        fn reassert(&mut self) -> SessionFuture<'_, anyhow::Result<String>> {
            let result = self.reasserts.lock().unwrap().pop_front();
            let fallback = self.trigger.clone();
            self.reassert_count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                match result {
                    Some(Ok(trigger)) => Ok(trigger),
                    Some(Err(error)) => Err(anyhow::anyhow!(error)),
                    None => Ok(fallback),
                }
            })
        }

        fn open_configure(&mut self) -> SessionFuture<'_, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn fake_session(trigger: &str) -> (mpsc::UnboundedSender<SessionEvent>, FakeSession) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let events = futures_util::stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|event| (event, receiver))
        });
        (
            sender,
            FakeSession {
                events: Box::pin(events),
                trigger: trigger.to_string(),
                reasserts: Arc::new(Mutex::new(VecDeque::new())),
                reassert_count: Arc::new(AtomicUsize::new(0)),
            },
        )
    }

    struct FakeConnector {
        sessions: Arc<Mutex<VecDeque<FakeSession>>>,
        connects: Arc<AtomicUsize>,
    }

    impl FakeConnector {
        fn new(sessions: Vec<FakeSession>) -> (Self, Arc<AtomicUsize>) {
            let connects = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    sessions: Arc::new(Mutex::new(VecDeque::from(sessions))),
                    connects: connects.clone(),
                },
                connects,
            )
        }
    }

    impl SessionConnector for FakeConnector {
        type Session = FakeSession;

        fn connect(&mut self) -> SessionFuture<'static, anyhow::Result<FakeSession>> {
            let session = self.sessions.lock().unwrap().pop_front();
            let connects = self.connects.clone();
            Box::pin(async move {
                match session {
                    Some(session) => {
                        connects.fetch_add(1, Ordering::SeqCst);
                        Ok(session)
                    }
                    None => Err(anyhow::anyhow!("portal unavailable")),
                }
            })
        }
    }

    fn fast_schedule() -> ReassertSchedule {
        ReassertSchedule {
            startup_delay: Duration::from_millis(5),
            interval: Duration::from_millis(10),
        }
    }

    async fn wait_until(condition: impl Fn() -> bool, what: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    #[tokio::test]
    async fn reconnects_after_the_desktop_closes_the_session() {
        let (sender, first) = fake_session("Super+O");
        let (_second_sender, second) = fake_session("Super+O");
        let (connector, connects) = FakeConnector::new(vec![first, second]);
        let (_commands, command_rx) = mpsc::channel(8);
        let task = tokio::spawn(supervise(
            connector,
            Arc::new(FakeSink::default()),
            command_rx,
            fast_schedule(),
        ));

        wait_until(|| connects.load(Ordering::SeqCst) == 1, "the first session").await;
        sender.send(SessionEvent::SessionClosed).unwrap();
        wait_until(
            || connects.load(Ordering::SeqCst) == 2,
            "a reconnect after the desktop closed the session",
        )
        .await;

        task.abort();
    }

    #[tokio::test]
    async fn reconnects_after_the_event_stream_ends() {
        let (sender, first) = fake_session("Super+O");
        let (_second_sender, second) = fake_session("Super+O");
        let (connector, connects) = FakeConnector::new(vec![first, second]);
        let (_commands, command_rx) = mpsc::channel(8);
        let task = tokio::spawn(supervise(
            connector,
            Arc::new(FakeSink::default()),
            command_rx,
            fast_schedule(),
        ));

        wait_until(|| connects.load(Ordering::SeqCst) == 1, "the first session").await;
        drop(sender);
        wait_until(
            || connects.load(Ordering::SeqCst) == 2,
            "a reconnect after the event stream ended",
        )
        .await;

        task.abort();
    }

    #[tokio::test]
    async fn reconnects_when_a_periodic_rebind_fails() {
        let (sender, first) = fake_session("Super+O");
        first
            .reasserts
            .lock()
            .unwrap()
            .push_back(Err("AccessDenied: Invalid session".to_string()));
        let first_reasserts = first.reassert_count.clone();
        let (_second_sender, second) = fake_session("Super+O");
        let (connector, connects) = FakeConnector::new(vec![first, second]);
        let (_commands, command_rx) = mpsc::channel(8);
        let task = tokio::spawn(supervise(
            connector,
            Arc::new(FakeSink::default()),
            command_rx,
            fast_schedule(),
        ));

        wait_until(
            || connects.load(Ordering::SeqCst) == 2,
            "a reconnect after a failed periodic rebind",
        )
        .await;
        assert_eq!(
            first_reasserts.load(Ordering::SeqCst),
            1,
            "the dead session must not keep being rebound"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            first_reasserts.load(Ordering::SeqCst),
            1,
            "the supervisor must reconnect instead of retrying the stale session"
        );

        drop(sender);
        task.abort();
    }

    #[tokio::test]
    async fn persistent_bind_failure_reports_failed_instead_of_reconnecting() {
        let (connector, _connects) = FakeConnector::new(Vec::new());
        let sink = Arc::new(FakeSink::default());
        let (_commands, command_rx) = mpsc::channel(8);
        let task = tokio::spawn(supervise(
            connector,
            sink.clone(),
            command_rx,
            fast_schedule(),
        ));

        wait_until(
            || {
                sink.states.lock().unwrap().iter().any(|(status, error)| {
                    *status == crate::ipc::ShortcutStatus::Failed
                        && error
                            .as_deref()
                            .is_some_and(|text| text.contains("portal unavailable"))
                })
            },
            "a persistent bind failure to be reported as failed",
        )
        .await;
        assert!(
            sink.states
                .lock()
                .unwrap()
                .iter()
                .take_while(|(status, _)| *status != crate::ipc::ShortcutStatus::Failed)
                .all(|(status, _)| *status == crate::ipc::ShortcutStatus::Initializing),
            "earlier retries must stay initializing until the failure is persistent"
        );

        task.abort();
    }

    #[tokio::test]
    async fn periodically_reasserts_the_binding() {
        let (_sender, session) = fake_session("");
        session
            .reasserts
            .lock()
            .unwrap()
            .push_back(Ok("Ctrl+B".to_string()));
        let reasserts = session.reassert_count.clone();
        let (connector, _connects) = FakeConnector::new(vec![session]);
        let sink = Arc::new(FakeSink::default());
        let (_commands, command_rx) = mpsc::channel(8);
        let task = tokio::spawn(supervise(
            connector,
            sink.clone(),
            command_rx,
            fast_schedule(),
        ));

        wait_until(
            || reasserts.load(Ordering::SeqCst) >= 2,
            "repeated periodic re-asserts",
        )
        .await;
        assert!(
            sink.bindings
                .lock()
                .unwrap()
                .iter()
                .any(|binding| binding == "Ctrl+B"),
            "the re-assert must record the trigger the desktop reported"
        );

        task.abort();
    }
}
