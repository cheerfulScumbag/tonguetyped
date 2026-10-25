use super::*;
use crate::inference::BackendChoice;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

const SIZES: [(u16, u16); 2] = [(80, 24), (60, 20)];
/// The two colours the wordmark must paint, straight from the theme.
const WORDMARK_GREEN: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
const WORDMARK_TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);

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
        typing_warning: None,
    }
}

fn app() -> App {
    app_with(Config::default())
}

fn app_with(config: Config) -> App {
    app_with_theme(config, Theme::new(false))
}

fn app_with_theme(config: Config, theme: Theme) -> App {
    TEST_DISK.with(|disk| *disk.borrow_mut() = config.clone());
    let mut app = App::new(config, capabilities(), theme, false);
    app.daemon = DaemonSnapshot {
        state: DaemonState::Idle,
        backend: Some("transcribe.cpp/vulkan".into()),
        error: None,
    };
    app
}

/// A console that paints colours, so the header's wordmark can be inspected.
fn colored_app() -> App {
    app_with_theme(Config::default(), Theme::new(true))
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

fn render_buffer(app: &App, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    app.render_into(area, &mut buf);
    buf
}

fn render(app: &App, width: u16, height: u16) -> Vec<String> {
    let buf = render_buffer(app, width, height);
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

/// The header content row as glyphs paired with the colour each was painted.
fn header_cells(app: &App, width: u16) -> Vec<(String, Color)> {
    let buf = render_buffer(app, width, 10);
    (0..width)
        .map(|x| (buf[(x, 1)].symbol().to_string(), buf[(x, 1)].fg))
        .collect()
}

fn header_text(cells: &[(String, Color)]) -> String {
    cells.iter().map(|(symbol, _)| symbol.as_str()).collect()
}

/// The cell index where `needle` starts, so multi-byte borders cannot shift it.
fn find_cells(cells: &[(String, Color)], needle: &str) -> Option<usize> {
    let needle: Vec<String> = needle.chars().map(|c| c.to_string()).collect();
    cells.windows(needle.len()).position(|window| {
        window
            .iter()
            .zip(&needle)
            .all(|((glyph, _), wanted)| glyph == wanted)
    })
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
fn the_header_wordmark_degrades_without_clipping_the_badge() {
    let app = app();
    assert!(render(&app, 100, 10)[1].contains("tonguetyped"));
    let mut seen = Vec::new();
    for width in (24..=100).rev() {
        let header = render(&app, width, 10)[1].trim_end().to_string();
        let wordmark = ["tonguetyped", "tt"]
            .into_iter()
            .find(|wordmark| header.starts_with(&format!("\u{2502} {wordmark}")))
            .unwrap_or_else(|| panic!("{width} cols has no wordmark: {header}"));
        assert!(header.contains("ACTIVE"), "{width} cols: {header}");
        assert!(header.ends_with('\u{2502}'), "{width} cols: {header}");
        assert!(!header.contains('\u{1F399}'), "no emoji: {header}");
        if seen.last() != Some(&wordmark) {
            seen.push(wordmark);
        }
    }
    assert_eq!(seen, ["tonguetyped", "tt"]);
}

#[test]
fn the_header_wordmark_paints_exactly_two_green_t_glyphs() {
    let app = colored_app();
    let cells = header_cells(&app, 100);
    let text = header_text(&cells);
    let start = find_cells(&cells, "tonguetyped").unwrap_or_else(|| panic!("no wordmark: {text}"));
    for (offset, letter) in "tonguetyped".chars().enumerate() {
        let (glyph, color) = &cells[start + offset];
        let expected = if letter == 't' {
            WORDMARK_GREEN
        } else {
            WORDMARK_TEXT
        };
        assert_eq!(glyph, &letter.to_string(), "wordmark letter {offset}");
        assert_eq!(
            *color, expected,
            "wordmark letter {offset} ({letter:?}) is painted {color:?}"
        );
    }
    let green_ts = cells
        .iter()
        .filter(|(glyph, color)| glyph == "t" && *color == WORDMARK_GREEN)
        .count();
    assert_eq!(green_ts, 2, "exactly the two t glyphs are green:\n{text}");
    assert!(text.contains("Daemon: "), "the daemon badge stays:\n{text}");
    assert!(text.contains("Vulkan"), "the backend badge stays:\n{text}");
}

#[test]
fn the_header_wordmark_falls_back_to_tt_when_narrow() {
    let app = colored_app();
    let cells = header_cells(&app, 22);
    let text = header_text(&cells);
    assert!(!text.contains("tonguetyped"), "too narrow for it:\n{text}");
    let start = find_cells(&cells, "tt").unwrap_or_else(|| panic!("no tt fallback: {text}"));
    for offset in 0..2 {
        assert_eq!(cells[start + offset], ("t".to_string(), WORDMARK_GREEN));
    }
    assert!(text.contains("ACTIVE"), "the badges stay:\n{text}");
}

#[test]
fn the_replacements_placeholder_names_no_other_app() {
    let mut app = app();
    focus(&mut app, Control::Field(Field::Replacements));
    let text = screen(&app, 100, 40);
    let line = text
        .lines()
        .find(|line| line.contains("none (e.g."))
        .unwrap_or_else(|| panic!("no Replacements placeholder:\n{text}"));
    assert!(line.contains("alot => a lot"), "{line}");
    for name in ["vox", "Vox", "VOX"] {
        assert!(!line.contains(name), "{line}");
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
                lines[1].contains("tonguetyped"),
                "{tab:?} {width}x{height}:\n{text}"
            );
            assert!(!text.contains("VOX"), "no other app's name:\n{text}");
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
fn enabling_feedback_sounds_previews_the_success_sound() {
    let mut app = app();
    app.take_effects();
    focus(&mut app, Control::FeedbackSounds);
    assert!(!app.checked(Control::FeedbackSounds));

    press(&mut app, KeyCode::Char(' '));
    assert!(app.checked(Control::FeedbackSounds));
    let effects = app.take_effects();
    assert!(
        matches!(
            &effects[..],
            [Effect::FeedbackSound {
                event: FeedbackEvent::Success,
                ..
            }]
        ),
        "turning sounds on plays success once: {effects:?}"
    );

    press(&mut app, KeyCode::Char(' '));
    assert!(!app.checked(Control::FeedbackSounds));
    assert!(
        app.take_effects().is_empty(),
        "turning sounds off does not play anything"
    );
}

#[test]
fn feedback_sound_failure_is_shown_with_the_real_reason() {
    let mut app = app();
    app.apply(TaskResult::FeedbackSound(Err(
        "no audio output device".to_string()
    )));
    let message = app.message.expect("the console shows the failure");
    assert!(
        message.text.contains("no audio output device"),
        "{message:?}"
    );
    assert_eq!(message.tone, Tone::Error);
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
    caps.typing_warning = Some("No typing helper found - install dotool.");
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

/// The catalog index of the default active model.
fn active_index() -> usize {
    crate::catalog::ENTRIES
        .iter()
        .position(|entry| entry.id == crate::catalog::DEFAULT_MODEL_ID)
        .expect("the default active model is in the catalog")
}

#[test]
fn the_table_shows_the_entry_count_and_the_filter_keys() {
    let mut app = app();
    focus(&mut app, Control::ModelTable);
    let text = screen(&app, 80, 24);
    assert!(
        text.contains(&format!("Speech models {}", crate::catalog::ENTRIES.len())),
        "{text}"
    );
    for key in ["f family", "t quant", "i installed", "s sort"] {
        assert!(text.contains(key), "missing {key:?}:\n{text}");
    }
}

#[test]
fn installed_family_and_quant_filters_are_reachable_and_clearable_from_the_table() {
    let mut app = app();
    focus(&mut app, Control::ModelTable);
    app.installed
        .iter_mut()
        .for_each(|installed| *installed = false);

    // Any -> installed: nothing is installed, so the table keeps its count
    // and says how to get the rows back.
    press(&mut app, KeyCode::Char('i'));
    assert_eq!(app.model_rows(), Vec::<usize>::new());
    let text = screen(&app, 80, 24);
    assert!(text.contains(&format!(
        "Speech models 0/{}",
        crate::catalog::ENTRIES.len()
    )));
    assert!(text.contains("installed"), "{text}");
    assert!(text.contains("c clear"), "{text}");
    assert!(text.contains("No models match"), "{text}");

    // Installed -> not installed: every row is back.
    press(&mut app, KeyCode::Char('i'));
    assert_eq!(app.model_rows().len(), crate::catalog::ENTRIES.len());
    assert!(screen(&app, 80, 24).contains("not installed"));

    // Installed -> any, then the family cycle starts at the first family in
    // catalog order and every kept row really is that family.
    press(&mut app, KeyCode::Char('i'));
    press(&mut app, KeyCode::Char('f'));
    let family = crate::catalog::ENTRIES[0].info().family_label();
    assert_eq!(app.model_filter.family, Some(family));
    let rows = app.model_rows();
    assert!(!rows.is_empty());
    assert!(rows
        .iter()
        .all(|index| crate::catalog::ENTRIES[*index].info().family_label() == family));
    assert!(screen(&app, 80, 24).contains(family));

    // The quantization cycle starts at the first quant in catalog order.
    press(&mut app, KeyCode::Char('t'));
    let quant = crate::catalog::ENTRIES[0].quant;
    assert_eq!(app.model_filter.quant, Some(quant));
    let rows = app.model_rows();
    assert!(!rows.is_empty());
    assert!(rows
        .iter()
        .all(|index| crate::catalog::ENTRIES[*index].quant == quant));

    // c restores the whole catalog but leaves the sort alone.
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Char('c'));
    assert!(!app.model_filter.is_active());
    assert_eq!(app.model_rows().len(), crate::catalog::ENTRIES.len());
    assert_ne!(
        app.model_sort,
        ModelSort::Catalog,
        "clear only clears filters"
    );
    let text = screen(&app, 80, 24);
    assert!(text.contains(&format!("Speech models {}", crate::catalog::ENTRIES.len())));
    assert!(text.contains("f family  t quant  i installed  s size \u{2191}"));
}

#[test]
fn filter_keys_only_work_while_the_table_has_focus() {
    let mut app = app();
    app.tab = Tab::Audio;
    for key in [
        KeyCode::Char('i'),
        KeyCode::Char('f'),
        KeyCode::Char('t'),
        KeyCode::Char('s'),
        KeyCode::Char('c'),
    ] {
        press(&mut app, key);
    }
    assert!(!app.model_filter.is_active());
    assert_eq!(app.model_sort, ModelSort::Catalog);
}

#[test]
fn filtering_and_sorting_never_edit_the_configuration() {
    let mut app = app();
    focus(&mut app, Control::ModelTable);
    for key in [
        KeyCode::Char('i'),
        KeyCode::Char('f'),
        KeyCode::Char('t'),
        KeyCode::Char('s'),
    ] {
        press(&mut app, key);
    }
    assert!(app.model_filter.is_active());
    assert_eq!(app.model_sort, ModelSort::SizeAsc);
    assert!(!app.dirty(), "the table view is session state, not an edit");
    assert!(app.exit.is_none());
}

#[test]
fn a_filter_that_hides_rows_clamps_the_cursor_to_a_visible_one() {
    let mut app = app();
    focus(&mut app, Control::ModelTable);
    let kept = 60;
    app.installed
        .iter_mut()
        .for_each(|installed| *installed = false);
    app.installed[kept] = true;
    app.model_cursor = 0;

    press(&mut app, KeyCode::Char('i'));
    assert_eq!(app.model_rows(), vec![kept]);
    assert_eq!(
        app.model_cursor, kept,
        "the cursor lands on the only row left"
    );
    assert_eq!(app.focused(), Control::ModelTable);

    // The single remaining row is both the first and the last, so k stays on
    // it rather than stepping into a hidden row.
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.model_cursor, kept);
    assert_eq!(app.focused(), Control::ModelTable);

    // Clearing brings back every row without moving the cursor off its model.
    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.model_cursor, kept);
    assert_eq!(app.model_rows().len(), crate::catalog::ENTRIES.len());
}

#[test]
fn paging_stays_inside_the_filtered_rows() {
    let mut app = app();
    focus(&mut app, Control::ModelTable);
    let (first, last) = (3, 7);
    app.installed
        .iter_mut()
        .for_each(|installed| *installed = false);
    app.installed[first] = true;
    app.installed[last] = true;
    app.model_cursor = 0;
    press(&mut app, KeyCode::Char('i'));
    assert_eq!(app.model_rows(), vec![first, last]);
    assert_eq!(app.model_cursor, first);
    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.model_cursor, last);
    press(&mut app, KeyCode::PageDown);
    assert_eq!(
        app.model_cursor, last,
        "paging stops at the last visible row"
    );
    press(&mut app, KeyCode::PageUp);
    assert_eq!(app.model_cursor, first);
}

#[test]
fn the_active_sort_is_named_in_the_header_and_the_status_line() {
    let mut app = app();
    focus(&mut app, Control::ModelTable);
    press(&mut app, KeyCode::Char('s'));
    assert_eq!(app.model_sort, ModelSort::SizeAsc);
    let text = screen(&app, 80, 24);
    assert!(text.contains("Size\u{2191}"), "{text}");
    assert!(text.contains("s size \u{2191}"), "{text}");

    press(&mut app, KeyCode::Char('s'));
    assert_eq!(app.model_sort, ModelSort::SizeDesc);
    let text = screen(&app, 80, 24);
    assert!(text.contains("Size\u{2193}"), "{text}");

    // A full cycle returns to catalog order and drops the marker.
    for _ in 0..4 {
        press(&mut app, KeyCode::Char('s'));
    }
    assert_eq!(app.model_sort, ModelSort::Catalog);
    let text = screen(&app, 80, 24);
    assert!(!text.contains("Size\u{2191}") && !text.contains("Size\u{2193}"));
}

#[test]
fn sorting_keeps_the_cursor_on_its_model_and_the_active_model_selectable() {
    let mut app = app();
    focus(&mut app, Control::ModelTable);
    app.installed
        .iter_mut()
        .for_each(|installed| *installed = true);
    let active = active_index();
    app.model_cursor = active;

    for _ in 0..6 {
        // cycle every order once; the cursor must stay on the same model
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.model_cursor, active);
        assert_eq!(
            app.config.model.active_model,
            crate::catalog::DEFAULT_MODEL_ID
        );
    }
    assert_eq!(app.model_sort, ModelSort::Catalog);

    press(&mut app, KeyCode::Char('s'));
    let family = crate::catalog::ENTRIES[active].info().family_label();
    while app.model_filter.family != Some(family) {
        press(&mut app, KeyCode::Char('f'));
    }
    assert!(app.model_rows().contains(&active));
    assert_eq!(
        app.model_cursor, active,
        "the filtered-out cursor is kept visible"
    );

    press(&mut app, KeyCode::Char(' '));
    assert_eq!(
        app.config.model.active_model,
        crate::catalog::DEFAULT_MODEL_ID,
        "the active row can still be selected without re-downloading"
    );
    assert!(app.take_effects().is_empty());
}

#[test]
fn selecting_a_filtered_and_sorted_row_selects_that_catalog_model() {
    let mut app = app();
    focus(&mut app, Control::ModelTable);
    app.installed
        .iter_mut()
        .for_each(|installed| *installed = false);
    while app.model_filter.quant != Some("F16") {
        press(&mut app, KeyCode::Char('t'));
    }
    press(&mut app, KeyCode::Char('s'));
    let target = app
        .model_rows()
        .into_iter()
        .find(|index| {
            let mut config = app.config.clone();
            config.model.active_model = crate::catalog::ENTRIES[*index].id.to_string();
            config.validate().is_ok()
        })
        .expect("an F16 entry the default recording limit accepts");
    app.model_cursor = target;
    let text = screen(&app, 80, 24);
    assert!(
        text.contains("F16"),
        "the row being selected is on screen:\n{text}"
    );
    press(&mut app, KeyCode::Char(' '));
    let id = crate::catalog::ENTRIES[target].id;
    assert_eq!(app.config.model.active_model, id);
    assert!(
        matches!(&app.take_effects()[..], [Effect::Download(wanted)] if wanted == id),
        "the selected row's own model downloads"
    );
}

#[test]
fn a_filter_that_hides_the_active_model_names_it() {
    let mut app = app();
    focus(&mut app, Control::ModelTable);
    let active = &crate::catalog::ENTRIES[active_index()];
    // The first family in catalog order is Whisper, which is the default
    // active model's own family; the second is not.
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Char('f'));
    assert_ne!(app.model_filter.family, Some(active.info().family_label()));
    assert!(!app.model_rows().contains(&active_index()));
    let text = screen(&app, 80, 24);
    assert!(
        text.contains(&format!(
            "! Active {} hidden by filters",
            crate::catalog::ENTRIES[active_index()]
                .id
                .replace(&format!("-{}", active.quant.to_ascii_lowercase()), "")
        )),
        "{text}"
    );
    assert!(text.contains(&format!(
        "Speech models {}/{}",
        app.model_rows().len(),
        crate::catalog::ENTRIES.len()
    )));
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
fn ctrl_r_keeps_the_old_wait_until_it_settles() {
    let mut app = app();
    app.handle_key(ctrl('r'));
    app.take_effects();
    app.handle_key(ctrl('r'));
    assert!(
        matches!(&app.take_effects()[..], [Effect::OpenShortcutDialog]),
        "a reopen must not cancel ahead of the executor"
    );

    let mut executor = Executor {
        slots: Vec::new(),
        status: Arc::new(Mutex::new(None)),
        stop: Arc::new(AtomicBool::new(false)),
        overlay: std::sync::mpsc::channel().0,
        shortcut: Some(ReconfigureHandle::for_test(Arc::new(Mutex::new(None)))),
        shortcut_reopen: false,
        download: None,
        download_progress: (0, 0),
    };
    executor.run(Effect::OpenShortcutDialog);
    assert!(
        executor.shortcut_reopen,
        "the reopen must wait for the old wait to settle"
    );
    assert!(
        executor.shortcut.is_some(),
        "the old wait must stay polled instead of starting a second configure"
    );

    executor.run(Effect::CancelShortcutDialog);
    assert!(
        !executor.shortcut_reopen,
        "keeping the current binding must cancel the scheduled reopen"
    );
    assert!(executor.shortcut.is_none());
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
