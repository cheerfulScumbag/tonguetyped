use crate::coordinator::Coordinator;
use anyhow::Context;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut, ShortcutsChanged};
use ashpd::desktop::Session;
use futures_util::{Stream, StreamExt};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

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
/// whichever runtime first opened it. Building a fresh runtime per call (as
/// the wizard's and dashboard's one-shot shortcut threads used to) tears that
/// reader down the moment the call returns, so every later portal call -
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
/// unbound, with no leftover default. Shared by `listen()`,
/// `test_shortcut_binding()`, and `reconfigure_shortcut()`, all three of which
/// need a bound shortcut before they can listen for presses or open the native
/// reconfigure dialog.
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

pub enum ShortcutTestOutcome {
    /// The configured shortcut was actually pressed and observed via the
    /// portal's `Activated` signal - not just accepted by `BindShortcuts`.
    Pressed,
    TimedOut,
}

/// Binds the shortcut and then actually listens for it to be pressed (via
/// the same `receive_activated()` stream `listen()` uses), instead of only
/// checking that `bind_shortcuts` accepted the registration. Tests whatever
/// trigger the desktop currently has bound for the action; the user sets
/// that trigger through the desktop's own dialog (`reconfigure_shortcut`).
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

pub async fn listen(
    coordinator: Arc<Coordinator>,
    ready: tokio::sync::oneshot::Sender<Result<(), String>>,
) -> anyhow::Result<()> {
    if let Err(error) = register_host_app().await {
        let message = format!("failed to register desktop application identity: {error}");
        let _ = ready.send(Err(message.clone()));
        anyhow::bail!(message);
    }
    let portal = match GlobalShortcuts::new().await {
        Ok(portal) => portal,
        Err(error) => {
            let message = format!("global shortcuts portal unavailable: {error}");
            let _ = ready.send(Err(message.clone()));
            anyhow::bail!(message);
        }
    };
    let session = portal.create_session().await?;
    let reported = match bind_activation_shortcut(&portal, &session).await {
        Ok(reported) => reported,
        Err(error) => {
            let message = error.to_string();
            let _ = ready.send(Err(message.clone()));
            anyhow::bail!(message);
        }
    };
    // The registration above is the app's only source of truth for what is
    // actually bound: persist whatever the desktop reports so `config` never
    // claims a key TongueTyped chose.
    coordinator.record_activation_binding(reported);

    let mut activated = portal.receive_activated().await?;
    let mut deactivated = portal.receive_deactivated().await?;
    let _ = ready.send(Ok(()));
    let mut event_order = EventOrder::default();
    loop {
        let event = tokio::select! {
            event = activated.next() => event.map(|event| (event.timestamp(), event.shortcut_id() == SHORTCUT_ID, true)),
            event = deactivated.next() => event.map(|event| (event.timestamp(), event.shortcut_id() == SHORTCUT_ID, false)),
        };
        let Some(event) = event else {
            anyhow::bail!("global shortcuts event stream closed");
        };
        let (timestamp, is_activation, pressed) = event;
        if is_activation {
            for pressed in event_order.push(timestamp, pressed) {
                if let Err(error) = coordinator.handle_activation(pressed).await {
                    tracing::error!("activation command failed: {error}");
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
}
