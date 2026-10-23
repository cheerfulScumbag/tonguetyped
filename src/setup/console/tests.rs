use super::*;
use crate::inference::BackendChoice;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const SIZES: [(u16, u16); 2] = [(80, 24), (60, 20)];

fn capabilities() -> Capabilities {
    Capabilities {
        microphones: vec![
            ("default".into(), "System default microphone".into()),
            ("pipewire:usb".into(), "USB Microphone".into()),
        ],
        typing_backends: vec!["wtype".into(), "libei".into()],
        clipboard: true,
        inference_backends: vec![
            BackendChoice {
                name: "auto",
                unavailable: None,
            },
            BackendChoice {
                name: "cpu",
                unavailable: None,
            },
            BackendChoice {
                name: "vulkan",
                unavailable: None,
            },
            BackendChoice {
                name: "cuda",
                unavailable: Some("not compiled in".into()),
            },
        ],
    }
}

fn app() -> App {
    app_with(Config::default())
}

fn app_with(config: Config) -> App {
    TEST_DISK.with(|disk| *disk.borrow_mut() = config.clone());
    let mut app = App::new(config, capabilities(), Theme::new(false), false);
    app.daemon = DaemonSnapshot {
        state: DaemonState::Idle,
        backend: Some("transcribe.cpp/vulkan".into()),
        error: None,
    };
    app
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(key(code));
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn render(app: &App, width: u16, height: u16) -> Vec<String> {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    app.render_into(area, &mut buf);
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

fn screen(app: &App, width: u16, height: u16) -> String {
    render(app, width, height).join("\n")
}

fn focus(app: &mut App, control: Control) {
    let tab = Tab::ALL
        .into_iter()
        .find(|tab| {
            app.tab = *tab;
            app.layout().controls().contains(&control)
        })
        .unwrap_or_else(|| panic!("{control:?} is on no tab"));
    app.tab = tab;
    app.focus[tab.index()] = control;
}

/// The text a control's row must show in full, wherever it is focused.
fn expected_label(control: Control) -> String {
    match control {
        Control::Microphone(0) => "System default microphone".into(),
        Control::Microphone(_) => "USB Microphone".into(),
        Control::Dictation => "[ Start Dictation ]".into(),
        Control::OverlayTest => "[Enter / Space] Test Overlay on Desktop".into(),
        Control::DaemonStart => "[ Start daemon ]".into(),
        Control::DaemonStop => "[ Stop daemon ]".into(),
        Control::DaemonRestart => "[ Restart daemon ]".into(),
        Control::ModelTable => "Speech models".into(),
        Control::Backend => "\u{25cf} auto".into(),
        control => {
            let label =
                render::control_label(control).unwrap_or_else(|| render::check_label(control));
            assert!(!label.is_empty(), "{control:?} has no label");
            label.to_string()
        }
    }
}

#[test]
fn every_tab_renders_its_frame_at_both_sizes() {
    let mut app = app();
    for (width, height) in SIZES {
        for tab in Tab::ALL {
            app.tab = tab;
            let lines = render(&app, width, height);
            assert_eq!(lines.len(), usize::from(height));
            let text = lines.join("\n");
            assert!(
                lines[1].contains("VOX // "),
                "{tab:?} {width}x{height}:\n{text}"
            );
            assert!(
                lines[1].contains("ACTIVE"),
                "the header carries the daemon state:\n{text}"
            );
            for number in 1..=4 {
                assert!(
                    lines[3].contains(&format!("[{number}]")),
                    "tab {number} missing at {width}x{height}:\n{text}"
                );
            }
            let footer = &lines[usize::from(height) - 1];
            assert!(footer.contains("q save & quit"), "footer: {footer}");
            assert!(footer.contains("j/k move"), "footer: {footer}");
            assert!(!text.contains("████████╗"), "no block-letter logo");
        }
    }
}

#[test]
fn every_control_is_fully_visible_when_focused_at_both_sizes() {
    let mut app = app();
    app.config.history.enabled = true;
    app.config.output.method = OutputMethod::Paste;
    for (width, height) in SIZES {
        for tab in Tab::ALL {
            app.tab = tab;
            for control in app.layout().controls() {
                focus(&mut app, control);
                let text = screen(&app, width, height);
                let label = expected_label(control);
                assert!(
                    text.contains(&label),
                    "{control:?} label {label:?} not fully visible at {width}x{height}:\n{text}"
                );
                assert!(
                    text.contains('\u{25b8}'),
                    "the focus marker shows without colour at {width}x{height}:\n{text}"
                );
            }
        }
    }
}

#[test]
fn the_wide_layout_puts_panes_side_by_side_and_the_narrow_one_stacks_them() {
    let mut app = app();
    app.tab = Tab::Audio;
    let wide = render(&app, 80, 24);
    assert!(
        wide.iter()
            .any(|line| line.contains("Microphone") && line.contains("Live dictation")),
        "{}",
        wide.join("\n")
    );
    let narrow = render(&app, 60, 20);
    assert!(
        !narrow
            .iter()
            .any(|line| line.contains("Microphone") && line.contains("Live dictation")),
        "{}",
        narrow.join("\n")
    );
}

#[test]
fn tab_titles_shorten_rather_than_overflow() {
    let mut app = app();
    app.tab = Tab::System;
    let wide = render(&app, 120, 30);
    assert!(wide[3].contains("[1] Audio & Capture"), "{}", wide[3]);
    assert!(wide[3].contains("[4] Interface & System"), "{}", wide[3]);
    let medium = render(&app, 80, 24);
    assert!(
        medium[3].contains("[4] Interface & System"),
        "{}",
        medium[3]
    );
    assert!(medium[3].contains("[1] Audio "), "{}", medium[3]);
    let narrow = render(&app, 60, 20);
    assert!(narrow[3].contains("[4] System"), "{}", narrow[3]);
}

#[test]
fn the_unsaved_marker_appears_after_an_edit() {
    let mut config = Config::default();
    config.output.method = OutputMethod::Clipboard;
    let mut app = app_with(config);
    focus(&mut app, Control::PauseMedia);
    assert!(!screen(&app, 80, 24).contains("unsaved changes"));
    press(&mut app, KeyCode::Char(' '));
    assert!(screen(&app, 80, 24).contains("unsaved changes"));
}

#[test]
fn number_keys_brackets_and_tab_switch_tabs() {
    let mut app = app();
    for (digit, tab) in ['1', '2', '3', '4'].into_iter().zip(Tab::ALL) {
        press(&mut app, KeyCode::Char(digit));
        assert_eq!(app.tab, tab);
    }
    press(&mut app, KeyCode::Char(']'));
    assert_eq!(app.tab, Tab::Audio, "] wraps past the last tab");
    press(&mut app, KeyCode::Char('['));
    assert_eq!(app.tab, Tab::System, "[ wraps before the first tab");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.tab, Tab::Audio);
    press(&mut app, KeyCode::BackTab);
    assert_eq!(app.tab, Tab::System);
}

#[test]
fn each_tab_remembers_its_own_focus() {
    let mut app = app();
    press(&mut app, KeyCode::Char('j'));
    let audio_focus = app.focused();
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('1'));
    assert_eq!(app.focused(), audio_focus);
}

#[test]
fn j_and_k_walk_every_control_in_order() {
    let mut app = app();
    app.tab = Tab::Audio;
    let controls = app.layout().controls();
    app.focus[0] = controls[0];
    for expected in &controls[1..] {
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.focused(), *expected);
    }
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(
        app.focused(),
        *controls.last().unwrap(),
        "j stops at the end"
    );
    press(&mut app, KeyCode::Up);
    assert_eq!(app.focused(), controls[controls.len() - 2]);
}

#[test]
fn h_and_l_move_between_panes_when_the_control_is_not_a_choice() {
    let mut app = app();
    render(&app, 80, 24);
    app.tab = Tab::Audio;
    app.focus[0] = Control::Microphone(0);
    press(&mut app, KeyCode::Char('l'));
    assert_eq!(app.focused(), Control::Dictation);
    press(&mut app, KeyCode::Char('h'));
    assert_eq!(app.focused(), Control::Microphone(0));
}

#[test]
fn space_toggles_checkboxes() {
    let mut app = app();
    for control in [
        Control::PauseMedia,
        Control::FeedbackSounds,
        Control::Translate,
        Control::SpokenPunctuation,
        Control::RemoveFillers,
        Control::HistoryEnabled,
        Control::Autostart,
    ] {
        focus(&mut app, control);
        let before = app.checked(control);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.checked(control), !before, "{control:?}");
    }
}

#[test]
fn space_selects_a_microphone_and_the_mode_radio_flips() {
    let mut app = app();
    focus(&mut app, Control::Microphone(1));
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.config.audio.microphone, "pipewire:usb");
    focus(&mut app, Control::ActivationMode);
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.config.activation.mode, ActivationMode::Toggle);
    press(&mut app, KeyCode::Char('h'));
    assert_eq!(app.config.activation.mode, ActivationMode::Hold);
}

#[test]
fn steppers_move_both_ways_and_wrap() {
    let mut app = app();
    focus(&mut app, Control::OverlayPosition);
    let start = app.config.overlay.position.clone();
    let index = POSITION_VALUES
        .iter()
        .position(|value| *value == start)
        .unwrap();
    press(&mut app, KeyCode::Char('l'));
    assert_eq!(
        app.config.overlay.position,
        POSITION_VALUES[(index + 1) % POSITION_VALUES.len()]
    );
    press(&mut app, KeyCode::Left);
    assert_eq!(app.config.overlay.position, start);
    for _ in 0..POSITION_VALUES.len() {
        press(&mut app, KeyCode::Char(' '));
    }
    assert_eq!(app.config.overlay.position, start, "Space cycles and wraps");
}

#[test]
fn changing_the_style_keeps_an_offered_look_and_resets_the_shape() {
    let mut app = app();
    app.config.overlay.style = "long-pill".into();
    app.config.overlay.waveform_shape = "bars".into();
    focus(&mut app, Control::OverlayStyle);
    let index = STYLE_VALUES
        .iter()
        .position(|value| *value == "long-pill")
        .unwrap();
    app.set_choice(Control::OverlayStyle, index);
    assert_eq!(
        app.config.overlay.waveform_shape,
        default_shape("long-pill"),
        "a style change starts that style's own shape"
    );
    assert_eq!(
        app.config.overlay.look,
        clamp_look_value("long-pill", &app.config.overlay.look)
    );
}

#[test]
fn look_and_shape_steppers_follow_the_style() {
    let mut app = app();
    for style in STYLE_VALUES {
        app.config.overlay.style = style.to_string();
        app.config.overlay.look = clamp_look_value(style, "waveform");
        app.tab = Tab::System;
        let controls = app.layout().controls();
        assert_eq!(
            controls.contains(&Control::OverlayLook),
            has_look_choice(style),
            "{style}"
        );
        assert_eq!(
            controls.contains(&Control::OverlayShape),
            shows_shape_choice(style, &app.config.overlay.look),
            "{style}"
        );
    }
}

#[test]
fn the_overlay_test_flashes_the_configured_overlay_then_hides_it() {
    let mut app = app();
    focus(&mut app, Control::OverlayTest);
    press(&mut app, KeyCode::Enter);
    let effects = app.take_effects();
    assert!(
        matches!(&effects[..], [Effect::OverlayShow(config)] if config.style == app.config.overlay.style),
        "{effects:?}"
    );
    let text = screen(&app, 80, 24);
    assert!(
        text.contains("Flashing real overlay on desktop (4s)..."),
        "{text}"
    );

    // Restyling mid-flash re-sends the overlay with the new look.
    focus(&mut app, Control::OverlayPosition);
    press(&mut app, KeyCode::Char('l'));
    app.tick(Instant::now());
    assert!(matches!(&app.take_effects()[..], [Effect::OverlayShow(_)]));

    app.tick(Instant::now() + OVERLAY_TEST_DURATION);
    assert!(matches!(&app.take_effects()[..], [Effect::OverlayHide]));
    assert!(app.overlay_test.is_none());
    assert!(!screen(&app, 80, 24).contains("Flashing"));
}

#[test]
fn space_also_runs_the_overlay_test() {
    let mut app = app();
    focus(&mut app, Control::OverlayTest);
    press(&mut app, KeyCode::Char(' '));
    assert!(matches!(&app.take_effects()[..], [Effect::OverlayShow(_)]));
}

#[test]
fn an_overlay_the_desktop_cannot_show_is_reported_in_the_ui() {
    let mut app = app();
    focus(&mut app, Control::OverlayTest);
    press(&mut app, KeyCode::Enter);
    app.apply(TaskResult::OverlayShown(false));
    assert!(app.overlay_test.is_none());
    let text = screen(&app, 80, 24);
    assert!(text.contains("no layer-shell overlay"), "{text}");
}

#[test]
fn quitting_during_the_overlay_test_hides_the_overlay() {
    let mut app = app();
    focus(&mut app, Control::OverlayTest);
    press(&mut app, KeyCode::Enter);
    app.take_effects();
    press(&mut app, KeyCode::Char('q'));
    assert!(app
        .take_effects()
        .iter()
        .any(|effect| matches!(effect, Effect::OverlayHide)));
}

#[test]
fn dictation_starts_and_stops_through_the_daemon() {
    let mut app = app();
    focus(&mut app, Control::Dictation);
    assert!(screen(&app, 80, 24).contains("[ IDLE ]"));
    press(&mut app, KeyCode::Enter);
    assert!(matches!(&app.take_effects()[..], [Effect::Dictation(true)]));
    app.apply(TaskResult::Dictation(Ok(())));
    app.apply(TaskResult::Status(DaemonSnapshot {
        state: DaemonState::Recording,
        backend: None,
        error: None,
    }));
    let text = screen(&app, 80, 24);
    assert!(text.contains("RECORDING..."), "{text}");
    assert!(text.contains("[ Stop Dictation ]"), "{text}");

    press(&mut app, KeyCode::Enter);
    assert!(matches!(
        &app.take_effects()[..],
        [Effect::Dictation(false)]
    ));
    assert!(
        app.capture.is_some(),
        "stopping captures the typed transcript"
    );
}

#[test]
fn typed_transcript_is_captured_instead_of_running_commands() {
    let mut app = app();
    app.apply(TaskResult::Status(DaemonSnapshot {
        state: DaemonState::Processing,
        backend: None,
        error: None,
    }));
    assert!(app.capture.is_some());
    type_text(&mut app, "q 1 hello");
    press(&mut app, KeyCode::Enter);
    assert!(app.exit.is_none(), "a typed q must not quit");
    assert_eq!(app.tab, Tab::Audio, "a typed digit must not switch tabs");
    app.apply(TaskResult::Status(DaemonSnapshot {
        state: DaemonState::Idle,
        backend: None,
        error: None,
    }));
    app.tick(Instant::now() + CAPTURE_GRACE);
    assert!(app.capture.is_none());
    assert_eq!(app.last_result.as_deref(), Some("q 1 hello"));
    assert!(matches!(&app.take_effects()[..], [Effect::FetchLastResult]));
    assert!(screen(&app, 80, 24).contains("Last result: q 1 hello"));
}

#[test]
fn an_older_transcript_is_never_shown_as_this_dictations_result() {
    let mut app = app();
    let status = |state| DaemonSnapshot {
        state,
        backend: None,
        error: None,
    };
    app.apply(TaskResult::Status(status(DaemonState::Recording)));
    app.apply(TaskResult::Status(status(DaemonState::Idle)));
    assert!(matches!(&app.take_effects()[..], [Effect::FetchLastResult]));
    app.apply(TaskResult::LastResult(Some(("from yesterday".into(), 1))));
    assert_eq!(app.last_result.as_deref(), Some(NO_SPEECH));

    app.apply(TaskResult::Status(status(DaemonState::Recording)));
    app.apply(TaskResult::Status(status(DaemonState::Idle)));
    app.apply(TaskResult::LastResult(Some((
        "hello there".into(),
        unix_seconds(),
    ))));
    assert_eq!(app.last_result.as_deref(), Some("hello there"));
}

#[test]
fn dictation_with_the_daemon_stopped_says_how_to_start_it() {
    let mut app = app();
    app.daemon.state = DaemonState::Stopped;
    focus(&mut app, Control::Dictation);
    press(&mut app, KeyCode::Enter);
    assert!(app.take_effects().is_empty());
    assert!(screen(&app, 80, 24).contains("The daemon is not running"));
}

#[test]
fn the_header_badge_names_the_state_and_the_backend() {
    let mut app = app();
    let text = screen(&app, 80, 24);
    assert!(
        text.contains("Daemon: \u{25cf} ACTIVE | Backend: Auto (Vulkan)"),
        "{text}"
    );
    app.config.model.preferred_backend = "cpu".into();
    app.daemon.state = DaemonState::Stopped;
    let text = screen(&app, 80, 24);
    assert!(text.contains("\u{25cb} STOPPED"), "{text}");
    assert!(text.contains("CPU"), "{text}");
}

#[test]
fn text_fields_edit_validate_and_cancel() {
    let mut app = app();
    focus(&mut app, Control::Field(Field::CleanupTimeout));
    press(&mut app, KeyCode::Enter);
    assert!(app.editing.is_some());
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Backspace);
    type_text(&mut app, "0");
    press(&mut app, KeyCode::Enter);
    assert!(
        app.editing.is_some(),
        "an invalid value keeps the field open"
    );
    assert!(screen(&app, 80, 24).contains("at least 1"));
    press(&mut app, KeyCode::Backspace);
    type_text(&mut app, "25");
    press(&mut app, KeyCode::Enter);
    assert!(app.editing.is_none());
    assert_eq!(app.config.text.post_process_timeout_seconds, 25);

    press(&mut app, KeyCode::Enter);
    type_text(&mut app, "9");
    press(&mut app, KeyCode::Esc);
    assert!(app.editing.is_none(), "Esc leaves the field");
    assert!(app.exit.is_none(), "Esc in a field never leaves setup");
    assert_eq!(app.config.text.post_process_timeout_seconds, 25);
}

#[test]
fn keys_type_into_a_field_instead_of_navigating() {
    let mut app = app();
    focus(&mut app, Control::Field(Field::CleanupCommand));
    press(&mut app, KeyCode::Enter);
    type_text(&mut app, "q1jh[");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.config.text.post_process_command, "q1jh[");
    assert_eq!(app.tab, Tab::Output);
    assert!(app.exit.is_none());
}

#[test]
fn the_decoding_drawer_opens_edits_and_closes_with_esc() {
    let mut app = app();
    focus(&mut app, Control::Decoding);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.drawer, Some(0));
    for (width, height) in SIZES {
        let text = screen(&app, width, height);
        assert!(text.contains("Whisper decoding"), "{text}");
        assert!(text.contains("Vocabulary prompt"), "{text}");
    }
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);
    type_text(&mut app, "0.4");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.config.transcription.temperature, Some(0.4));
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.drawer, None);
    assert!(app.exit.is_none(), "Esc closes the drawer, not setup");
}

#[test]
fn the_delivery_method_shows_only_its_own_settings() {
    let mut app = app();
    app.config.output.method = OutputMethod::Paste;
    app.tab = Tab::Output;
    let controls = app.layout().controls();
    assert!(controls.contains(&Control::PasteKeys));
    assert!(controls.contains(&Control::AutoSubmit));
    assert!(controls.contains(&Control::TypingBackend));

    app.config.output.auto_submit = true;
    app.config.output.typing_backend = "wtype".into();
    let clipboard = DELIVERY_METHODS
        .iter()
        .position(|method| *method == OutputMethod::Clipboard)
        .unwrap();
    app.set_choice(Control::OutputMethod, clipboard);
    assert!(
        !app.config.output.auto_submit,
        "Enter-after only applies when inserting"
    );
    assert_eq!(app.config.output.typing_backend, "auto");
    let controls = app.layout().controls();
    assert!(!controls.contains(&Control::PasteKeys));
    assert!(!controls.contains(&Control::AutoSubmit));
}

#[test]
fn a_delivery_method_that_cannot_work_here_is_skipped() {
    let mut caps = capabilities();
    caps.typing_backends.clear();
    let mut app = App::new(Config::default(), caps, Theme::new(false), false);
    app.config.output.method = OutputMethod::Clipboard;
    focus(&mut app, Control::OutputMethod);
    press(&mut app, KeyCode::Char('h'));
    assert_eq!(
        app.config.output.method,
        OutputMethod::None,
        "Type and Paste need a typing helper, so h skips them and wraps"
    );
    press(&mut app, KeyCode::Char('l'));
    assert_eq!(app.config.output.method, OutputMethod::Clipboard);
}

#[test]
fn a_fresh_install_opens_on_typing_like_the_old_wizard() {
    let app = App::new(Config::default(), capabilities(), Theme::new(false), true);
    assert_eq!(app.config.output.method, OutputMethod::Type);
    assert!(app.dirty(), "the suggestion is an unsaved edit");
}

#[test]
fn an_existing_no_delivery_choice_is_left_alone() {
    let mut app = app();
    assert_eq!(app.config.output.method, OutputMethod::None);
    assert!(!app.dirty());
    press(&mut app, KeyCode::Char('q'));
    assert!(matches!(app.exit, Some(SetupOutcome::Unchanged)));
}

#[test]
fn an_unavailable_backend_is_marked_and_skipped() {
    let mut app = app();
    focus(&mut app, Control::Backend);
    let text = screen(&app, 80, 24);
    assert!(text.contains("unavailable here: cuda"), "{text}");
    app.config.model.preferred_backend = "vulkan".into();
    press(&mut app, KeyCode::Char('l'));
    assert_eq!(
        app.config.model.preferred_backend, "auto",
        "cuda is skipped"
    );
}

#[test]
fn selecting_a_missing_model_downloads_it() {
    let mut app = app();
    focus(&mut app, Control::ModelTable);
    app.installed
        .iter_mut()
        .for_each(|installed| *installed = false);
    let target = crate::catalog::ENTRIES
        .iter()
        .position(|entry| {
            let mut config = app.config.clone();
            config.model.active_model = entry.id.to_string();
            entry.id != app.config.model.active_model && config.validate().is_ok()
        })
        .unwrap();
    app.model_cursor = target;
    press(&mut app, KeyCode::Char(' '));
    let id = crate::catalog::ENTRIES[target].id;
    assert_eq!(app.config.model.active_model, id);
    assert!(matches!(&app.take_effects()[..], [Effect::Download(wanted)] if wanted == id));
    app.apply(TaskResult::DownloadProgress(50, 100));
    assert!(screen(&app, 80, 24).contains("Downloading 50%"));
    app.apply(TaskResult::Download(Ok(
        crate::model::DownloadOutcome::Fresh,
    )));
    assert!(app.installed[target]);
}

#[test]
fn the_table_cursor_moves_and_pages() {
    let mut app = app();
    focus(&mut app, Control::ModelTable);
    app.model_cursor = 0;
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.model_cursor, 0);
    assert_eq!(app.focused(), Control::ModelTable);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.model_cursor, 1);
    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.model_cursor, 11);
    let text = screen(&app, 60, 20);
    assert!(text.contains(crate::catalog::ENTRIES[11].id) || text.contains('\u{2026}'));
    press(&mut app, KeyCode::Char('l'));
    assert_ne!(app.focused(), Control::ModelTable, "l leaves the table");
}

#[test]
fn ctrl_r_opens_the_shortcut_dialog_and_esc_keeps_the_binding() {
    let mut app = app();
    app.config.activation.keybind = "Meta+Space".into();
    app.handle_key(ctrl('r'));
    assert!(matches!(
        &app.take_effects()[..],
        [Effect::OpenShortcutDialog]
    ));
    assert!(screen(&app, 80, 24).contains("shortcut window is open"));
    press(&mut app, KeyCode::Esc);
    assert!(app.exit.is_none(), "Esc backs out of the dialog wait only");
    assert!(matches!(
        &app.take_effects()[..],
        [Effect::CancelShortcutDialog]
    ));
    assert!(!app.shortcut_waiting);
}

#[test]
fn a_changed_shortcut_is_stored_with_the_super_wording() {
    let mut app = app();
    app.handle_key(ctrl('r'));
    app.apply(TaskResult::Shortcut(Ok(ReconfigureOutcome::Changed {
        trigger_description: "Meta+D".into(),
    })));
    assert_eq!(app.config.activation.keybind, "Super+D");
    assert!(screen(&app, 80, 24).contains("Bound: Super+D"));
}

#[test]
fn a_failed_shortcut_change_is_reported_and_leaves_the_binding() {
    let mut app = app();
    app.config.activation.keybind = "Super+D".into();
    app.handle_key(ctrl('r'));
    app.apply(TaskResult::Shortcut(Err("portal unavailable".into())));
    assert_eq!(app.config.activation.keybind, "Super+D");
    assert!(screen(&app, 80, 24).contains("portal unavailable"));
}

#[test]
fn daemon_actions_run_one_at_a_time_and_report_back() {
    let mut app = app();
    focus(&mut app, Control::DaemonRestart);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    assert!(matches!(
        &app.take_effects()[..],
        [Effect::Daemon(DaemonAction::Restart)]
    ));
    assert!(screen(&app, 80, 24).contains("Restarting the daemon..."));
    app.apply(TaskResult::Daemon(Err(
        "The daemon stopped but could not start again: boom".into(),
    )));
    assert!(app.daemon_busy.is_none());
    assert!(screen(&app, 80, 24).contains("could not start again"));
}

#[test]
fn ctrl_s_saves_only_changed_fields_onto_the_current_file() {
    let mut app = app_with(Config::default());
    // Another terminal changes the file after setup opened.
    TEST_DISK.with(|disk| disk.borrow_mut().audio.feedback_volume = 0.3);
    focus(&mut app, Control::PauseMedia);
    press(&mut app, KeyCode::Char(' '));
    app.handle_key(ctrl('s'));
    let disk = TEST_DISK.with(|disk| disk.borrow().clone());
    assert_eq!(disk.audio.pause_media, app.config.audio.pause_media);
    assert_eq!(disk.audio.feedback_volume, 0.3, "the other edit survives");
    assert_eq!(app.config.audio.feedback_volume, 0.3, "setup adopts it");
    assert!(!app.dirty());
    assert!(screen(&app, 80, 24).contains("Saved"));
}

#[test]
fn q_and_esc_at_the_root_save_and_quit() {
    for code in [KeyCode::Char('q'), KeyCode::Esc] {
        let mut app = app();
        focus(&mut app, Control::Autostart);
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, code);
        assert!(matches!(app.exit, Some(SetupOutcome::Saved)), "{code:?}");
        assert!(TEST_DISK.with(|disk| disk.borrow().startup.autostart));
    }
}

#[test]
fn quitting_with_nothing_changed_writes_nothing() {
    let mut config = Config::default();
    config.output.method = OutputMethod::Clipboard;
    let mut app = app_with(config);
    press(&mut app, KeyCode::Char('q'));
    assert!(matches!(app.exit, Some(SetupOutcome::Unchanged)));
}

#[test]
fn a_save_that_fails_validation_stays_open_with_the_error() {
    let mut app = app();
    app.config.output.method = OutputMethod::Clipboard;
    app.config.output.auto_submit = true;
    press(&mut app, KeyCode::Char('q'));
    assert!(app.exit.is_none());
    let text = screen(&app, 80, 24);
    assert!(text.contains("Not saved"), "{text}");
    app.handle_key(ctrl('c'));
    assert!(matches!(app.exit, Some(SetupOutcome::Cancelled)));
}

#[test]
fn merge_changes_applies_edits_and_removals_only() {
    let base: toml::Value = toml::from_str("a = 1\nb = 2\n[t]\nx = 1\ny = 2\n").unwrap();
    let edited: toml::Value = toml::from_str("a = 5\nb = 2\n[t]\nx = 1\n").unwrap();
    let mut target: toml::Value =
        toml::from_str("a = 1\nb = 9\nc = 3\n[t]\nx = 7\ny = 2\nz = 4\n").unwrap();
    merge_changes(&base, &edited, &mut target);
    let expected: toml::Value = toml::from_str("a = 5\nb = 9\nc = 3\n[t]\nx = 7\nz = 4\n").unwrap();
    assert_eq!(target, expected);
}

#[test]
fn a_saved_config_round_trips_through_the_file_format() {
    let mut app = app();
    app.config.transcription.temperature = Some(0.2);
    app.config
        .text
        .replacements
        .insert("tee vee".into(), "TV".into());
    app.config.overlay.style = "long-pill".into();
    app.handle_key(ctrl('s'));
    let disk = TEST_DISK.with(|disk| disk.borrow().clone());
    let text = toml::to_string_pretty(&disk).unwrap();
    let parsed: Config = toml::from_str(&text).unwrap();
    parsed.validate().unwrap();
    assert_eq!(parsed.transcription.temperature, Some(0.2));
    assert_eq!(
        parsed.text.replacements.get("tee vee").map(String::as_str),
        Some("TV")
    );
    assert_eq!(parsed.overlay.style, "long-pill");
}

#[test]
fn colour_mode_paints_the_slate_background() {
    let mut app = app();
    app.theme = Theme::new(true);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    app.render_into(area, &mut buf);
    assert_eq!(
        buf[(40, 12)].bg,
        ratatui::style::Color::Rgb(0x18, 0x18, 0x25),
        "the theme fills its own background, so a light terminal stays readable"
    );
}

/// Writes every tab at both sizes to `$TONGUETYPED_RENDER_DUMP` for review.
#[test]
fn dump_renders_when_asked() {
    let Some(dir) = std::env::var_os("TONGUETYPED_RENDER_DUMP") else {
        return;
    };
    let mut app = app();
    for (width, height) in SIZES {
        for tab in Tab::ALL {
            app.tab = tab;
            let path = std::path::Path::new(&dir).join(format!("{tab:?}-{width}x{height}.txt"));
            std::fs::write(path, screen(&app, width, height)).unwrap();
        }
    }
}

fn three_mic_app() -> App {
    let mut caps = capabilities();
    caps.microphones = vec![
        ("test-mic-1".into(), "Test microphone 1".into()),
        ("test-mic-2".into(), "Test microphone 2".into()),
        ("test-mic-3".into(), "Test microphone 3".into()),
    ];
    let mut app = App::new(Config::default(), caps, Theme::new(false), false);
    app.tab = Tab::Audio;
    app.focus[0] = Control::Microphone(0);
    app.mic_monitor.simulate_active_preview(0);
    app
}

#[test]
fn rapid_mic_navigation_defers_the_preview_restart_until_focus_settles() {
    let mut app = three_mic_app();
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.focused(), Control::Microphone(2));
    assert_eq!(app.mic_monitor.active_index(), None);
    assert!(app.mic_monitor.restart_pending());
    app.refresh_mic();
    assert!(
        app.mic_monitor.restart_pending(),
        "no device opens before the settle delay"
    );
}

#[test]
fn an_async_stream_error_lands_in_the_level_row() {
    let mut app = three_mic_app();
    // What AudioRecorder's error callback does from its audio thread when a
    // device disconnects mid-stream.
    app.mic_monitor.inject_stream_error("Device disconnected");
    app.refresh_mic();
    assert_eq!(app.mic_monitor.error(), Some("Device disconnected"));
    assert!(!app.mic_monitor.has_open_stream());
    assert!(!app.mic_monitor.has_pending_stream_error());
    assert!(screen(&app, 80, 24).contains("Level  \u{2716} Device disconnected"));
}

#[test]
fn moving_to_another_mic_discards_the_previous_devices_error() {
    let mut app = three_mic_app();
    app.mic_monitor.inject_stream_error("Device 0 disconnected");
    press(&mut app, KeyCode::Char('j'));
    assert!(!app.mic_monitor.has_pending_stream_error());
    app.refresh_mic();
    assert_eq!(app.mic_monitor.error(), None);
}
