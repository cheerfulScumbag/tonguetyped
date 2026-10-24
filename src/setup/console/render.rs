//! Drawing for the setup console. Everything renders into a `Buffer`, so the
//! same code paints the live terminal and the headless render tests.
//!
//! Each pane column is laid out at full height in an off-screen buffer and
//! the window around the focused row is copied onto the screen, so a terminal
//! too short for a tab scrolls instead of clipping it.

use super::{App, Control, DaemonState, Field, Kind, Pane, Row, Tab, Tone};
use crate::config::DECODE_KNOBS;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border::ROUNDED;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, LineGauge, Widget};
use ratatui::Frame;
use std::cell::Cell;

/// The narrowest body that still shows two pane columns side by side.
const WIDE_MIN: u16 = 76;
/// The header wordmark from widest to narrowest: the name in the brand style,
/// then an optional dimmer suffix.
const BRANDS: [(&str, &str); 3] = [("TONGUETYPED", " SETUP"), ("TONGUETYPED", ""), ("TT", "")];
/// The width every row reserves for the focus marker.
const MARKER: u16 = 2;
/// A blank column kept between row content and the pane's right border.
const GUTTER: u16 = 1;

impl App {
    pub(super) fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        self.render_into(area, frame.buffer_mut());
    }

    pub(super) fn render_into(&self, area: Rect, buf: &mut Buffer) {
        buf.set_style(area, self.theme.base());
        let status_lines = self.status_lines(area.width);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(status_lines.len() as u16),
                Constraint::Length(1),
            ])
            .split(area);
        self.render_header(chunks[0], buf);
        self.render_tabs(chunks[1], buf);
        self.render_body(chunks[2], buf);
        for (index, line) in status_lines.into_iter().enumerate() {
            let row = Rect::new(chunks[3].x, chunks[3].y + index as u16, chunks[3].width, 1);
            line.render(row, buf);
        }
        self.render_footer(chunks[4], buf);
    }

    // ---- header and tabs ------------------------------------------------

    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        let mut block = Block::bordered()
            .border_set(ROUNDED)
            .border_style(self.theme.border(false));
        if self.dirty() {
            block = block.title_bottom(
                Line::from(Span::styled(
                    " \u{270e} unsaved changes ",
                    self.theme.warn(),
                ))
                .right_aligned(),
            );
        }
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 || inner.width < 3 {
            return;
        }
        let inner = Rect::new(inner.x + 1, inner.y, inner.width - 2, 1);
        let width = usize::from(inner.width);
        let badges = self.daemon_badges();
        let mut chosen = None;
        'search: for brand in BRANDS {
            for badge in &badges {
                if brand_width(brand) + 2 + line_width(badge) <= width {
                    chosen = Some((brand, badge.clone()));
                    break 'search;
                }
            }
        }
        let ((name, suffix), badge) =
            chosen.unwrap_or((BRANDS[BRANDS.len() - 1], badges[badges.len() - 1].clone()));
        let gap = width.saturating_sub(brand_width((name, suffix)) + line_width(&badge));
        let mut spans = vec![
            Span::styled(name, self.theme.brand()),
            Span::styled(suffix, self.theme.dim()),
            Span::raw(" ".repeat(gap)),
        ];
        spans.extend(badge);
        fit(spans, inner.width).render(inner, buf);
    }

    /// The daemon badge from widest to narrowest, so the header can pick the
    /// widest that fits next to the brand.
    fn daemon_badges(&self) -> Vec<Vec<Span<'static>>> {
        let (state, tone) = super::daemon_state_label(self.daemon.state);
        let state = Span::styled(state, self.tone(tone));
        let backend = Span::styled(self.backend_badge(), self.theme.accent());
        let dim = |text: &'static str| Span::styled(text, self.theme.dim());
        vec![
            vec![
                dim("Daemon: "),
                state.clone(),
                dim(" | Backend: "),
                backend.clone(),
            ],
            vec![state.clone(), dim(" | "), backend],
            vec![state],
        ]
    }

    /// The configured backend preference; with `auto`, plus the backend the
    /// running daemon resolved it to.
    fn backend_badge(&self) -> String {
        let preference = self.config.model.preferred_backend.as_str();
        if preference != "auto" {
            return short_backend(preference).to_string();
        }
        let running = self
            .daemon
            .backend
            .as_deref()
            .filter(|_| {
                !matches!(
                    self.daemon.state,
                    DaemonState::Stopped | DaemonState::Unknown
                )
            })
            .and_then(|backend| backend.rsplit('/').next())
            .map(short_backend);
        match running {
            Some(backend) => format!("Auto ({backend})"),
            None => "Auto".to_string(),
        }
    }

    fn render_tabs(&self, area: Rect, buf: &mut Buffer) {
        let width = usize::from(area.width).saturating_sub(2);
        let label = |tab: Tab, full: bool| {
            let number = tab.index() + 1;
            let title = if full { tab.title() } else { tab.short_title() };
            format!(" [{number}] {title} ")
        };
        let separator = " | ";
        // Widest first: every title in full, then only the active one, then
        // all short, then bare numbers.
        let variants: [&dyn Fn(Tab) -> String; 4] = [
            &|tab| label(tab, true),
            &|tab| label(tab, tab == self.tab),
            &|tab| label(tab, false),
            &|tab| format!(" {} ", tab.index() + 1),
        ];
        let mut labels: Vec<String> = Vec::new();
        for variant in variants {
            labels = Tab::ALL.iter().map(|tab| variant(*tab)).collect();
            let total: usize = labels.iter().map(|label| text_width(label)).sum::<usize>()
                + separator.len() * (labels.len() - 1);
            if total <= width {
                break;
            }
        }
        let mut spans = vec![Span::raw(" ")];
        for (index, (tab, text)) in Tab::ALL.iter().zip(labels).enumerate() {
            if index > 0 {
                spans.push(Span::styled(separator, self.theme.dim()));
            }
            let style = if *tab == self.tab {
                self.theme.active_tab()
            } else {
                self.theme.text()
            };
            spans.push(Span::styled(text, style));
        }
        fit(spans, area.width).render(area, buf);
    }

    // ---- body -----------------------------------------------------------

    fn render_body(&self, area: Rect, buf: &mut Buffer) {
        let layout = self.layout();
        let wide = area.width >= WIDE_MIN;
        self.wide.set(wide);
        let focused = self.focused();
        if layout.table {
            if wide {
                let columns = split(area, Direction::Horizontal, 58);
                self.render_table(columns[0], buf);
                self.render_column(&layout.right, columns[1], buf, &self.scroll[1], focused);
            } else {
                let table_height = (area.height / 2).clamp(5, 9).min(area.height);
                let rows = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(table_height), Constraint::Min(0)])
                    .split(area);
                self.render_table(rows[0], buf);
                self.render_column(&layout.right, rows[1], buf, &self.scroll[1], focused);
            }
        } else if wide {
            let left = if self.tab == Tab::System { 55 } else { 50 };
            let columns = split(area, Direction::Horizontal, left);
            self.render_column(&layout.left, columns[0], buf, &self.scroll[0], focused);
            self.render_column(&layout.right, columns[1], buf, &self.scroll[1], focused);
        } else {
            let panes: Vec<Pane> = layout.left.into_iter().chain(layout.right).collect();
            self.render_column(&panes, area, buf, &self.scroll[0], focused);
        }
        if let Some(row) = self.drawer {
            self.render_drawer(area, buf, row);
        }
    }

    /// Lays `panes` out at full height and copies the window that keeps the
    /// focused row (and its pane title, when it is the first row) visible.
    /// When the panes are shorter than `area`, the last one grows to fill it.
    fn render_column(
        &self,
        panes: &[Pane],
        area: Rect,
        buf: &mut Buffer,
        scroll: &Cell<u16>,
        focused: Control,
    ) {
        if area.width < 6 || area.height == 0 || panes.is_empty() {
            return;
        }
        let inner_width = area.width - 2;
        let mut heights: Vec<u16> = panes
            .iter()
            .map(|pane| self.pane_content_height(pane, inner_width) + 2)
            .collect();
        let natural: u16 = heights.iter().sum();
        if natural < area.height {
            if let Some(last) = heights.last_mut() {
                *last += area.height - natural;
            }
        }
        let total: u16 = heights.iter().sum();
        let mut sheet = Buffer::empty(Rect::new(0, 0, area.width, total));
        sheet.set_style(sheet.area, self.theme.base());
        let mut focus_span = None;
        let mut top = 0;
        for (pane, height) in panes.iter().zip(&heights) {
            let rect = Rect::new(0, top, area.width, *height);
            if let Some((row_y, row_height)) = self.render_pane(pane, rect, &mut sheet, focused) {
                let first =
                    pane.rows.iter().position(
                        |row| matches!(row, Row::Control(control) if *control == focused),
                    ) == Some(0);
                focus_span = Some((if first { top } else { row_y }, row_y + row_height));
            }
            top += height;
        }

        let visible = area.height;
        let max_offset = total.saturating_sub(visible);
        let mut offset = scroll.get().min(max_offset);
        if let Some((start, end)) = focus_span {
            if start < offset {
                offset = start;
            }
            if end > offset + visible {
                offset = end.saturating_sub(visible);
            }
        }
        scroll.set(offset);
        for y in 0..visible.min(total.saturating_sub(offset)) {
            for x in 0..area.width {
                buf[(area.x + x, area.y + y)] = sheet[(x, offset + y)].clone();
            }
        }
        let marker_x = area.x + area.width.saturating_sub(3);
        if offset > 0 {
            buf.set_string(marker_x, area.y, "\u{25b2}", self.theme.accent());
        }
        if offset + visible < total {
            buf.set_string(
                marker_x,
                area.y + visible - 1,
                "\u{25bc}",
                self.theme.accent(),
            );
        }
    }

    fn pane_label_width(&self, pane: &Pane) -> usize {
        pane.rows
            .iter()
            .filter_map(|row| match row {
                Row::Control(control) => control_label(*control).map(text_width),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }

    fn pane_content_height(&self, pane: &Pane, inner_width: u16) -> u16 {
        let label_width = self.pane_label_width(pane);
        pane.rows
            .iter()
            .map(|row| self.row_lines(row, inner_width, label_width).len() as u16)
            .sum()
    }

    /// A row's content, one entry per screen line, before the focus marker.
    fn row_lines(
        &self,
        row: &Row,
        inner_width: u16,
        label_width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let width = inner_width.saturating_sub(MARKER + GUTTER);
        match row {
            Row::Control(control) => self.control_lines(*control, width, label_width),
            Row::Info(spans) => vec![spans
                .iter()
                .map(|(text, tone)| Span::styled(text.clone(), self.tone(*tone)))
                .collect()],
            Row::Wrapped(text, tone) => wrap(text, width)
                .into_iter()
                .map(|line| vec![Span::styled(line, self.tone(*tone))])
                .collect(),
            Row::Meter => vec![Vec::new()],
        }
    }

    /// Draws one pane and returns the y and height of its focused row, if it
    /// has it.
    fn render_pane(
        &self,
        pane: &Pane,
        area: Rect,
        buf: &mut Buffer,
        focused: Control,
    ) -> Option<(u16, u16)> {
        let has_focus = pane
            .rows
            .iter()
            .any(|row| matches!(row, Row::Control(control) if *control == focused));
        let block = self.pane_block(pane.title, has_focus);
        let inner = block.inner(area);
        block.render(area, buf);
        let label_width = self.pane_label_width(pane);
        let mut y = inner.y;
        let mut focus = None;
        for row in &pane.rows {
            let lines = self.row_lines(row, inner.width, label_width);
            let height = lines.len() as u16;
            let rect = Rect::new(inner.x, y, inner.width, height);
            if let Row::Meter = row {
                self.render_meter(rect, buf);
            } else {
                let is_focused = matches!(row, Row::Control(control) if *control == focused);
                if is_focused {
                    focus = Some((y, height));
                    if self.editing.is_none() || self.editing_control() == Some(focused) {
                        buf.set_style(rect, self.theme.focus());
                    }
                }
                for (index, spans) in lines.into_iter().enumerate() {
                    let marker = if is_focused && index == 0 {
                        "\u{25b8} "
                    } else {
                        "  "
                    };
                    let line_rect = Rect::new(inner.x, y + index as u16, inner.width, 1);
                    let mut line = vec![Span::raw(marker)];
                    line.extend(spans);
                    fit(line, inner.width).render(line_rect, buf);
                }
            }
            y += height;
        }
        focus
    }

    fn editing_control(&self) -> Option<Control> {
        self.editing
            .as_ref()
            .map(|(field, _)| Control::Field(*field))
    }

    fn pane_block(&self, title: &str, focused: bool) -> Block<'static> {
        let title = if focused {
            format!(" \u{25b8} {title} ")
        } else {
            format!(" {title} ")
        };
        Block::bordered()
            .border_set(ROUNDED)
            .border_style(self.theme.border(focused))
            .title(Span::styled(title, self.theme.heading()))
    }

    fn render_meter(&self, area: Rect, buf: &mut Buffer) {
        if let Some(error) = self.mic_monitor.error() {
            fit(
                vec![Span::styled(
                    format!("  Level  \u{2716} {error}"),
                    self.theme.error(),
                )],
                area.width,
            )
            .render(area, buf);
            return;
        }
        let ratio = crate::audio::level_to_ratio(self.mic_monitor.level());
        LineGauge::default()
            .ratio(ratio)
            .label(Span::styled(
                format!("  Level {:>3}%", (ratio * 100.0).round() as u32),
                self.theme.dim(),
            ))
            .filled_symbol("\u{2588}")
            .unfilled_symbol("\u{2591}")
            .filled_style(self.theme.gauge())
            .unfilled_style(self.theme.dim())
            .render(
                Rect::new(area.x, area.y, area.width.saturating_sub(1), 1),
                buf,
            );
    }

    /// One control's lines after its focus marker, laid out within `width`.
    fn control_lines(
        &self,
        control: Control,
        width: u16,
        label_width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let theme = &self.theme;
        let label = || match control_label(control) {
            Some(text) => Span::styled(format!("{text:<label_width$}  "), theme.text()),
            None => Span::raw(""),
        };
        let label_part = if control_label(control).is_some() {
            label_width + 2
        } else {
            0
        };
        let available = usize::from(width).saturating_sub(label_part);
        match control.kind() {
            Kind::Radio => {
                let Control::Microphone(index) = control else {
                    return vec![Vec::new()];
                };
                let selected = index == self.selected_microphone();
                let name = self
                    .capabilities
                    .microphones
                    .get(index)
                    .map(|(_, name)| name.clone())
                    .unwrap_or_default();
                let style = if selected {
                    theme.accent()
                } else {
                    theme.text()
                };
                vec![vec![
                    Span::styled(if selected { "\u{25cf} " } else { "\u{25cb} " }, style),
                    Span::styled(name, style),
                ]]
            }
            Kind::Check => {
                let checked = self.checked(control);
                vec![vec![
                    Span::styled(
                        if checked { "[x] " } else { "[ ] " },
                        if checked {
                            theme.accent()
                        } else {
                            theme.text()
                        },
                    ),
                    Span::styled(check_label(control), theme.text()),
                ]]
            }
            Kind::Choice => {
                let indent = Span::raw(" ".repeat(label_part));
                self.choice_lines(control, available)
                    .into_iter()
                    .enumerate()
                    .map(|(index, spans)| {
                        let mut line = vec![if index == 0 { label() } else { indent.clone() }];
                        line.extend(spans);
                        line
                    })
                    .collect()
            }
            Kind::Text => {
                let Control::Field(field) = control else {
                    return vec![Vec::new()];
                };
                let mut line = vec![label()];
                line.extend(self.field_spans(field, available));
                vec![line]
            }
            Kind::Action => vec![self.action_spans(control, label_width)],
            Kind::Table => vec![Vec::new()],
        }
    }

    /// A selector's options: inline, flowing onto more lines when they do not
    /// fit; steppers (and an option too long for any line) show `◂ value ▸`.
    fn choice_lines(&self, control: Control, available: usize) -> Vec<Vec<Span<'static>>> {
        let theme = &self.theme;
        let (options, current) = self.choice(control);
        let stepper = matches!(
            control,
            Control::OverlayEnabled
                | Control::OverlayPosition
                | Control::OverlayStyle
                | Control::OverlayLook
                | Control::OverlayShape
        );
        let tokens: Vec<Span<'static>> = options
            .iter()
            .enumerate()
            .map(|(index, (name, usable))| {
                let (mark, style) = if index == current {
                    ("\u{25cf} ", theme.accent())
                } else if *usable {
                    ("\u{25cb} ", theme.text())
                } else {
                    ("\u{00d7} ", theme.dim())
                };
                Span::styled(format!("{mark}{name}"), style)
            })
            .collect();
        if !stepper && tokens.iter().all(|token| token.width() <= available) {
            let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new()];
            let mut used = 0;
            for token in tokens {
                let line = lines.last_mut().expect("there is always a line");
                let gap = if line.is_empty() { 0 } else { 2 };
                if !line.is_empty() && used + gap + token.width() > available {
                    used = token.width();
                    lines.push(vec![token]);
                    continue;
                }
                if gap > 0 {
                    line.push(Span::raw("  "));
                }
                used += gap + token.width();
                line.push(token);
            }
            return lines;
        }
        let name = options
            .get(current)
            .map(|(name, _)| name.clone())
            .unwrap_or_default();
        let mut spans = vec![
            Span::styled("\u{25c2} ", theme.dim()),
            Span::styled(name, theme.accent()),
            Span::styled(" \u{25b8}", theme.dim()),
        ];
        let count = format!("  {}/{}", current + 1, options.len());
        if line_width(&spans) + text_width(&count) <= available {
            spans.push(Span::styled(count, theme.dim()));
        }
        vec![spans]
    }

    fn field_spans(&self, field: Field, available: usize) -> Vec<Span<'static>> {
        let theme = &self.theme;
        if let Some((editing, text)) = &self.editing {
            if *editing == field {
                let (before, cursor, after) = text.split_at_cursor();
                let cursor = cursor.map(String::from).unwrap_or_else(|| " ".into());
                // Keep the cursor in view: drop characters from the front of a
                // value longer than the field.
                let room = available.saturating_sub(1);
                let mut before: Vec<char> = before.chars().collect();
                while text_width(&before.iter().collect::<String>()) > room {
                    before.remove(0);
                }
                return vec![
                    Span::styled(before.into_iter().collect::<String>(), theme.text()),
                    Span::styled(cursor, theme.text().add_modifier(Modifier::REVERSED)),
                    Span::styled(after, theme.text()),
                ];
            }
        }
        let value = self.field_value(field);
        if value.trim().is_empty() {
            vec![Span::styled(field_placeholder(field), theme.dim())]
        } else {
            vec![Span::styled(value, theme.text())]
        }
    }

    fn action_spans(&self, control: Control, label_width: usize) -> Vec<Span<'static>> {
        let theme = &self.theme;
        let label = |text: &str| Span::styled(format!("{text:<label_width$}  "), theme.text());
        match control {
            Control::Dictation => {
                let text = if self.daemon.state == DaemonState::Recording {
                    "[ Stop Dictation ]"
                } else {
                    "[ Start Dictation ]"
                };
                vec![Span::styled(text, theme.accent())]
            }
            Control::Shortcut => vec![
                label("Shortcut"),
                Span::styled(
                    crate::activation::keybind_display(&self.config.activation.keybind),
                    theme.accent(),
                ),
            ],
            Control::Decoding => vec![
                label("Decoding"),
                Span::styled(self.config.transcription.decoding_summary(), theme.text()),
                Span::styled(" \u{25b8}", theme.dim()),
            ],
            Control::OverlayTest => vec![Span::styled(
                "[Enter / Space] Test Overlay on Desktop",
                theme.accent(),
            )],
            Control::DaemonStart => vec![Span::styled("[ Start daemon ]", theme.text())],
            Control::DaemonStop => vec![Span::styled("[ Stop daemon ]", theme.text())],
            Control::DaemonRestart => vec![Span::styled("[ Restart daemon ]", theme.text())],
            _ => Vec::new(),
        }
    }

    // ---- model catalog ----------------------------------------------------

    fn render_table(&self, area: Rect, buf: &mut Buffer) {
        let focused = self.focused() == Control::ModelTable && self.drawer.is_none();
        let block = self.pane_block("Speech models", focused);
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 || inner.width < 12 {
            return;
        }
        let (quant, size, live) = (7, 8, 5);
        let prefix = 3;
        let id_width = usize::from(inner.width)
            .saturating_sub(prefix + quant + size + live)
            .max(4);
        let header = format!(
            "{:prefix$}{:<id_width$}{:<quant$}{:>size$}{:>live$}",
            "", "ID", "Quant", "Size", "Live"
        );
        fit(
            vec![Span::styled(header, self.theme.heading())],
            inner.width,
        )
        .render(Rect::new(inner.x, inner.y, inner.width, 1), buf);

        let visible = usize::from(inner.height - 1);
        if visible == 0 {
            return;
        }
        let entries = crate::catalog::ENTRIES;
        let max_offset = entries.len().saturating_sub(visible);
        // The first frame centres the active model; later frames scroll only
        // as far as the cursor needs.
        let mut offset = self
            .table_scroll
            .get()
            .unwrap_or_else(|| self.model_cursor.saturating_sub(visible / 2))
            .min(max_offset);
        if self.model_cursor < offset {
            offset = self.model_cursor;
        } else if self.model_cursor >= offset + visible {
            offset = self.model_cursor + 1 - visible;
        }
        self.table_scroll.set(Some(offset));
        for (row, (index, entry)) in entries
            .iter()
            .enumerate()
            .skip(offset)
            .take(visible)
            .enumerate()
        {
            let rect = Rect::new(inner.x, inner.y + 1 + row as u16, inner.width, 1);
            let active = entry.id == self.config.model.active_model;
            let cursor = index == self.model_cursor;
            if cursor && focused {
                buf.set_style(rect, self.theme.focus());
            }
            let line = format!(
                "{:<id_width$}{:<quant$}{:>size$}{:>live$}",
                truncate_middle(short_id(entry), id_width.saturating_sub(1)),
                truncate(entry.quant, quant - 1),
                format_size(entry.size_bytes),
                if entry.info().supports_streaming() {
                    "live"
                } else {
                    "-"
                },
            );
            let marker = match (cursor, active) {
                (true, true) => "\u{25b8}\u{25cf} ",
                (true, false) => "\u{25b8}\u{25cb} ",
                (false, true) => " \u{25cf} ",
                (false, false) => " \u{25cb} ",
            };
            let style = if active {
                self.theme.accent()
            } else {
                self.theme.text()
            };
            fit(
                vec![Span::styled(marker, style), Span::styled(line, style)],
                inner.width,
            )
            .render(rect, buf);
        }
        let marker_x = area.x + area.width.saturating_sub(3);
        if offset > 0 {
            buf.set_string(marker_x, area.y, "\u{25b2}", self.theme.accent());
        }
        if offset + visible < entries.len() {
            buf.set_string(
                marker_x,
                area.y + area.height - 1,
                "\u{25bc}",
                self.theme.accent(),
            );
        }
    }

    // ---- decoding drawer ----------------------------------------------------

    fn render_drawer(&self, area: Rect, buf: &mut Buffer, row: usize) {
        let width = area.width.saturating_sub(4).min(72);
        let pane = Pane {
            title: "Whisper decoding",
            rows: (0..=DECODE_KNOBS.len())
                .map(|index| Row::Control(Control::Field(Field::Decode(index))))
                .chain(std::iter::once(Row::Wrapped(
                    crate::config::DECODING_HINT.to_string(),
                    Tone::Dim,
                )))
                .collect(),
        };
        let height =
            (self.pane_content_height(&pane, width.saturating_sub(2)) + 2).min(area.height);
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        Clear.render(rect, buf);
        buf.set_style(rect, self.theme.base());
        self.render_column(
            std::slice::from_ref(&pane),
            rect,
            buf,
            &self.drawer_scroll,
            Control::Field(Field::Decode(row)),
        );
    }

    // ---- status rows and footer ---------------------------------------------

    /// The overlay-test countdown banner and the current message, each only
    /// while there is one.
    fn status_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if let Some(seconds) = self.overlay_seconds_left() {
            let banner =
                format!(" [ \u{f0379} Flashing real overlay on desktop ({seconds}s)... ] ");
            let pad = usize::from(width).saturating_sub(text_width(&banner)) / 2;
            lines.push(fit(
                vec![
                    Span::raw(" ".repeat(pad)),
                    Span::styled(banner, self.theme.active_tab()),
                ],
                width,
            ));
        }
        if let Some(message) = &self.message {
            let prefix = match message.tone {
                Tone::Error => "\u{2716} ",
                Tone::Warn => "! ",
                Tone::Ok => "\u{2714} ",
                _ => "",
            };
            lines.extend(
                wrap(
                    &format!("{prefix}{}", message.text),
                    width.saturating_sub(2),
                )
                .into_iter()
                .take(2)
                .map(|line| {
                    fit(
                        vec![Span::raw(" "), Span::styled(line, self.tone(message.tone))],
                        width,
                    )
                }),
            );
        }
        lines
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        let mut spans = vec![Span::raw(" ")];
        let mut used = 1;
        for (key, action) in self.hints() {
            let width = text_width(key) + 1 + text_width(action) + 2;
            if used + width > usize::from(area.width) {
                continue;
            }
            used += width;
            spans.push(Span::styled(key, self.theme.key()));
            spans.push(Span::styled(format!(" {action}  "), self.theme.dim()));
        }
        fit(spans, area.width).render(area, buf);
    }

    /// The key hints for what has focus, most specific first; the footer
    /// drops whichever no longer fit.
    pub(super) fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.capture.is_some() {
            return vec![("Esc", "stop capturing"), ("^C", "discard & quit")];
        }
        if self.editing.is_some() {
            return vec![
                ("Enter", "apply"),
                ("Esc", "cancel"),
                ("\u{2190}/\u{2192}", "cursor"),
                ("Home/End", "jump"),
            ];
        }
        if self.shortcut_waiting {
            return vec![
                ("Enter/Esc", "keep current"),
                ("^R", "reopen"),
                ("q", "save & quit"),
            ];
        }
        if self.drawer.is_some() {
            return vec![
                ("Enter", "edit"),
                ("j/k", "move"),
                ("Esc", "close"),
                ("^S", "save"),
                ("1-4", "tab"),
            ];
        }
        let focused = self.focused();
        let mut hints = match focused.kind() {
            Kind::Radio => vec![("Space", "select")],
            Kind::Check => vec![("Space", "toggle")],
            Kind::Choice => vec![("h/l", "change"), ("Space", "next")],
            Kind::Text => vec![("Enter", "edit")],
            Kind::Table => vec![("Space", "select"), ("PgUp/Dn", "page")],
            Kind::Action => vec![(
                "Enter",
                match focused {
                    Control::Dictation => "start/stop",
                    Control::Shortcut => "open dialog",
                    Control::Decoding => "open",
                    Control::OverlayTest => "flash overlay",
                    _ => "run",
                },
            )],
        };
        hints.push(("j/k", "move"));
        hints.push(("q", "save & quit"));
        hints.push(("^S", "save"));
        hints.push(("1-4", "tab"));
        if focused.kind() != Kind::Choice {
            hints.push(("h/l", "pane"));
        }
        hints.push(("^R", "shortcut"));
        hints.push(("^C", "discard"));
        hints
    }

    fn tone(&self, tone: Tone) -> Style {
        match tone {
            Tone::Text => self.theme.text(),
            Tone::Dim => self.theme.dim(),
            Tone::Accent => self.theme.accent(),
            Tone::Ok => self.theme.ok(),
            Tone::Warn => self.theme.warn(),
            Tone::Error => self.theme.error(),
            Tone::Recording => self.theme.recording(),
        }
    }
}

fn split(area: Rect, direction: Direction, first_percent: u16) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(direction)
        .constraints([
            Constraint::Percentage(first_percent),
            Constraint::Percentage(100 - first_percent),
        ])
        .split(area)
}

pub(super) fn control_label(control: Control) -> Option<&'static str> {
    Some(match control {
        Control::ActivationMode => "Mode",
        Control::Shortcut => "Shortcut",
        Control::Decoding => "Decoding",
        Control::OutputMethod => "Method",
        Control::TypingBackend => "Typing via",
        Control::PasteKeys => "Paste keys",
        Control::OverlayEnabled => "Enabled",
        Control::OverlayPosition => "Position",
        Control::OverlayStyle => "Style",
        Control::OverlayLook => "Look",
        Control::OverlayShape => "Shape",
        Control::Field(field) => field_label(field),
        _ => return None,
    })
}

fn field_label(field: Field) -> &'static str {
    match field {
        Field::Replacements => "Replacements",
        Field::CleanupCommand => "Cleanup command",
        Field::CleanupTimeout => "Timeout (s)",
        Field::MaxEntries => "Max entries",
        Field::MaxAge => "Max age (days)",
        Field::TranscriptFolder => "Export folder",
        Field::Decode(0) => crate::config::INITIAL_PROMPT_LABEL,
        Field::Decode(index) => DECODE_KNOBS
            .get(index - 1)
            .map(|knob| knob.label)
            .unwrap_or(""),
    }
}

fn brand_width((name, suffix): (&str, &str)) -> usize {
    text_width(name) + text_width(suffix)
}

fn field_placeholder(field: Field) -> String {
    match field {
        Field::Replacements => "none (e.g. alot => a lot)".into(),
        Field::CleanupCommand | Field::TranscriptFolder => "none".into(),
        Field::Decode(index) => crate::config::TranscriptionConfig::decoding_form_placeholders()
            .get(index)
            .cloned()
            .unwrap_or_default(),
        _ => String::new(),
    }
}

pub(super) fn check_label(control: Control) -> &'static str {
    match control {
        Control::PauseMedia => "Pause media while dictating",
        Control::FeedbackSounds => "Feedback sounds",
        Control::Translate => "Translate to English",
        Control::AutoSubmit => "Press Enter after the text",
        Control::SpokenPunctuation => "Spoken punctuation",
        Control::RemoveFillers => "Remove filler words",
        Control::HistoryEnabled => "Save transcript history",
        Control::Autostart => "Start when you sign in",
        _ => "",
    }
}

fn short_backend(name: &str) -> &str {
    match name {
        "cpu" => "CPU",
        "vulkan" => "Vulkan",
        "cuda" => "CUDA",
        "rocm" => "ROCm",
        "metal" => "Metal",
        other => other,
    }
}

pub(super) fn text_width(text: &str) -> usize {
    Span::raw(text).width()
}

fn line_width(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// Cuts `text` to `width` columns, marking a cut with an ellipsis.
fn truncate(text: &str, width: usize) -> String {
    if text_width(text) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    for c in text.chars() {
        if text_width(&out) + text_width(&c.to_string()) + 1 > width {
            break;
        }
        out.push(c);
    }
    out.push('\u{2026}');
    out
}

/// A catalog id without its quantization suffix, which the table shows in
/// its own column: `parakeet-tdt-0.6b-v2-f16` becomes `parakeet-tdt-0.6b-v2`.
fn short_id(entry: &crate::catalog::CatalogModel) -> &'static str {
    let suffix = format!("-{}", entry.quant.to_ascii_lowercase());
    entry.id.strip_suffix(suffix.as_str()).unwrap_or(entry.id)
}

/// Cuts the middle out of `text` to fit `width` columns, keeping both ends,
/// so ids that differ only near the end (`-v2`, `-v3`) stay distinguishable.
fn truncate_middle(text: &str, width: usize) -> String {
    if text_width(text) <= width || width < 5 {
        return truncate(text, width);
    }
    let chars: Vec<char> = text.chars().collect();
    let tail = (width - 1) / 2;
    let head = width - 1 - tail;
    let start: String = chars[..head].iter().collect();
    let end: String = chars[chars.len() - tail..].iter().collect();
    format!("{start}\u{2026}{end}")
}

/// Lays `spans` on one line of `width` columns: a span that would run past
/// the edge is cut and ends in an ellipsis, so nothing overflows or wraps.
fn fit(spans: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let width = usize::from(width);
    if line_width(&spans) <= width {
        return Line::from(spans);
    }
    let mut out = Vec::new();
    let mut used = 0;
    for span in spans {
        let span_width = span.width();
        if used + span_width < width {
            used += span_width;
            out.push(span);
            continue;
        }
        let room = width.saturating_sub(used);
        out.push(Span::styled(truncate(&span.content, room), span.style));
        break;
    }
    Line::from(out)
}

/// Greedy word wrap to `width` columns; a word longer than the line is cut.
fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width).max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if text_width(&candidate) <= width {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        current = if text_width(word) > width {
            truncate(word, width)
        } else {
            word.to_string()
        };
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

fn format_size(bytes: u64) -> String {
    let megabytes = bytes as f64 / 1_000_000.0;
    if megabytes >= 1000.0 {
        format!("{:.1} GB", megabytes / 1000.0)
    } else {
        format!("{megabytes:.0} MB")
    }
}
