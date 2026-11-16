//! A terminal in a tab: a shell, or a command, on a pty of its own.
//!
//! The keys go to the program, all of them but the platform's (cmd) and
//! `ctrl-\ ctrl-n`, which goes to normal mode as in vim: a copy of what
//! went by, in an editor, for the workspace to show instead. Text comes by
//! the input method, so that it can be composed.

use gpui::component::ActiveTheme as _;
use gpui::prelude::*;
use gpui::*;
use med_term::{CursorKind, Event, History, Modifiers, Palette, Rgb, Screen, SelectKind, Terminal};
use std::ops::Range;

/// What a terminal has to tell the workspace.
pub enum TermViewEvent {
    /// A key, before it went to the program: the pane it is in has the
    /// keys.
    KeyPress,
    /// `ctrl-\ ctrl-n`: to normal mode, in a copy of what went by.
    Normal,
    /// The program is done, and a key was pressed: the tab may go.
    Close,
    /// Its title, or whether it runs, is not what it was.
    Changed,
}

/// Where the cells are, as last drawn.
#[derive(Clone, Copy)]
struct Metrics {
    origin: Point<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
}

pub struct TermView {
    terminal: Terminal,
    focus_handle: FocusHandle,
    /// What the program called itself.
    title: Option<String>,
    /// Given what it was started to run, rather than the shell.
    command: Option<String>,
    /// Its exit code, once it is done (`Some(None)` if killed).
    exited: Option<Option<i32>>,
    /// `ctrl-\` was pressed, and the key after says what it is.
    escape_pending: bool,
    /// What the input method is composing, shown at the cursor.
    marked: Option<String>,
    metrics: Option<Metrics>,
    /// Of a line not yet scrolled by the wheel.
    scroll_rest: Pixels,
    /// The mouse is down, selecting.
    selecting: bool,
    _events: Task<()>,
}

impl EventEmitter<TermViewEvent> for TermView {}

impl Focusable for TermView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TermView {
    /// A view of `terminal`, which says what is new on `events`.
    /// `command` is what it was given to run, if not the shell.
    pub fn new(
        terminal: Terminal,
        mut events: futures::channel::mpsc::UnboundedReceiver<Event>,
        command: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let task = cx.spawn(async move |this, cx| {
            while let Some(first) = next_event(&mut events, cx).await {
                // What came meanwhile, in one go: one redraw for them all.
                let mut batch = vec![first];
                while let Ok(event) = events.try_recv() {
                    batch.push(event);
                }
                let updated = this.update(cx, |this, cx| {
                    for event in batch {
                        this.on_event(event, cx);
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });
        Self {
            terminal,
            focus_handle: cx.focus_handle(),
            title: None,
            command,
            exited: None,
            escape_pending: false,
            marked: None,
            metrics: None,
            scroll_rest: px(0.),
            selecting: false,
            _events: task,
        }
    }

    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Wakeup | Event::Bell => {}
            Event::Title(title) => {
                self.title = Some(title).filter(|title| !title.is_empty());
                cx.emit(TermViewEvent::Changed);
            }
            Event::ResetTitle => {
                self.title = None;
                cx.emit(TermViewEvent::Changed);
            }
            Event::Exited(code) => {
                self.exited = Some(code);
                cx.emit(TermViewEvent::Changed);
            }
        }
    }

    /// What its tab is called.
    pub fn title(&self) -> String {
        let name = self
            .title
            .clone()
            .or_else(|| self.command.clone())
            .unwrap_or_else(|| "terminal".to_string());
        match self.exited {
            Some(_) => format!("{name} [exited]"),
            None => name,
        }
    }

    /// Whether the program is still running.
    pub fn is_running(&self) -> bool {
        self.exited.is_none()
    }

    /// Text from the clipboard, as the program asked to be given it.
    pub fn paste(&mut self, text: &str) {
        if self.is_running() {
            self.terminal.scroll_to_bottom();
            self.terminal.paste(text);
        }
    }

    /// What the mouse selected, to the clipboard. False if nothing is.
    pub fn copy(&mut self, cx: &mut Context<Self>) -> bool {
        match self.terminal.selection_text() {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
            None => false,
        }
    }

    /// Everything there is, as text, for normal mode.
    pub fn history(&self) -> History {
        self.terminal.history()
    }

    /// Hang up on the program, for its tab is going.
    pub fn close(&mut self) {
        self.terminal.shutdown();
    }

    /// The screen as it is, for a test to read.
    #[cfg(test)]
    pub fn screen(&self) -> Screen {
        self.terminal.screen(&Palette::dark())
    }

    fn write(&mut self, bytes: Vec<u8>) {
        self.terminal.clear_selection();
        self.terminal.scroll_to_bottom();
        self.terminal.write(bytes);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let m = &keystroke.modifiers;
        // The platform's keys are med's: copy, paste, quit.
        if m.platform || m.function {
            return;
        }
        cx.emit(TermViewEvent::KeyPress);
        let key = keystroke.key.as_str();
        let control = m.control && !m.alt && !m.shift;
        if self.escape_pending {
            self.escape_pending = false;
            if control && key == "n" {
                cx.stop_propagation();
                cx.emit(TermViewEvent::Normal);
                return;
            }
            // Not the way out: the program has the `ctrl-\` after all.
            if self.is_running() {
                self.write(vec![0x1c]);
            }
        } else if control && key == "\\" {
            cx.stop_propagation();
            self.escape_pending = true;
            return;
        }
        if !self.is_running() {
            cx.stop_propagation();
            cx.emit(TermViewEvent::Close);
            return;
        }
        // Back through what went by, a screenful at a time.
        if m.shift && !m.control && !m.alt && matches!(key, "pageup" | "pagedown") {
            cx.stop_propagation();
            self.terminal.scroll_page(key == "pageup");
            cx.notify();
            return;
        }
        let mods = Modifiers {
            control: m.control,
            alt: m.alt,
            shift: m.shift,
        };
        if let Some(bytes) = med_term::encode_key(key, mods, self.terminal.app_cursor()) {
            cx.stop_propagation();
            self.write(bytes);
        }
        // Anything else is text, which the input method gives us.
    }

    fn on_scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(metrics) = self.metrics else {
            return;
        };
        let delta = event.delta.pixel_delta(metrics.line_height).y + self.scroll_rest;
        let lines = (delta / metrics.line_height).trunc();
        self.scroll_rest = delta - metrics.line_height * lines;
        if lines == 0. {
            return;
        }
        if self.terminal.alternate_scroll() {
            // A program with the whole screen scrolls itself, by the
            // arrow keys.
            let key = if lines > 0. { "up" } else { "down" };
            let app_cursor = self.terminal.app_cursor();
            let bytes = med_term::encode_key(key, Modifiers::default(), app_cursor)
                .unwrap_or_default()
                .repeat(lines.abs() as usize);
            self.terminal.write(bytes);
        } else {
            self.terminal.scroll(lines as i32);
            cx.notify();
        }
    }

    /// The cell under `position`, as (row, column, on its right half).
    fn cell_at(&self, position: Point<Pixels>) -> Option<(usize, usize, bool)> {
        let metrics = self.metrics?;
        let x = (position.x - metrics.origin.x).max(px(0.)) / metrics.cell_width;
        let y = (position.y - metrics.origin.y).max(px(0.)) / metrics.line_height;
        Some((y as usize, x as usize, x.fract() >= 0.5))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        cx.emit(TermViewEvent::KeyPress);
        let Some((line, col, right)) = self.cell_at(event.position) else {
            return;
        };
        let kind = match event.click_count {
            2 => SelectKind::Word,
            3.. => SelectKind::Line,
            _ => SelectKind::Simple,
        };
        self.terminal.select_start(line, col, right, kind);
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if let Some((line, col, right)) = self.cell_at(event.position) {
            self.terminal.select_update(line, col, right);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        // A click, not a drag, selects nothing.
        if self.terminal.selection_text().is_none() {
            self.terminal.clear_selection();
        }
        cx.notify();
    }

    /// Size the terminal to `bounds` and lay out what it shows.
    fn layout_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Frame {
        let settings = cx.global::<crate::Settings>();
        let theme = cx.theme();
        let family = settings
            .font_family
            .clone()
            .unwrap_or_else(|| theme.mono_font_family.clone());
        let font_size = px(settings.font_size);
        let line_height = px((settings.font_size * 1.4).round());
        let base_font = font(family);
        let palette = palette(cx);
        let selection = rgb_of(theme.selection);
        let text_system = window.text_system().clone();
        let run = |len: usize, font: Font, color: Hsla| TextRun {
            len,
            font,
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let cell_width = text_system
            .shape_line(
                "0".into(),
                font_size,
                &[run(1, base_font.clone(), black())],
                None,
            )
            .width;

        let pad = px(6.);
        let origin = point(bounds.left() + pad, bounds.top() + pad / 2.);
        let cols = ((bounds.size.width - pad * 2.) / cell_width)
            .floor()
            .max(2.) as u16;
        let rows = ((bounds.size.height - pad) / line_height).floor().max(1.) as u16;
        self.terminal.resize(
            cols,
            rows,
            f32::from(cell_width).round() as u16,
            f32::from(line_height).round() as u16,
        );
        let metrics = Metrics {
            origin,
            cell_width,
            line_height,
        };
        self.metrics = Some(metrics);

        let screen = self.terminal.screen(&palette);
        let background = hsla(screen.background);
        let at = |line: usize, col: usize| {
            point(
                origin.x + cell_width * col as f32,
                origin.y + line_height * line as f32,
            )
        };
        let mut frame = Frame {
            bounds,
            background,
            line_height,
            back: Vec::new(),
            lines: Vec::new(),
            front: Vec::new(),
            front_lines: Vec::new(),
        };
        let style_font = |cell: &med_term::Cell| {
            let mut font = base_font.clone();
            if cell.bold {
                font.weight = FontWeight::BOLD;
            }
            if cell.italic {
                font.style = FontStyle::Italic;
            }
            font
        };
        let shape = |text: String, cell: &med_term::Cell, color: Hsla| {
            let len = text.len();
            let mut text_run = run(len, style_font(cell), color);
            if cell.underline {
                text_run.underline = Some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(color),
                    wavy: false,
                });
            }
            if cell.strikethrough {
                text_run.strikethrough = Some(StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(color),
                });
            }
            text_system.shape_line(text.into(), font_size, &[text_run], None)
        };

        for (line, cells) in screen.lines.iter().enumerate() {
            // Backgrounds, a stretch of one color at a time.
            let mut stretch: Option<(usize, usize, Rgb)> = None;
            for cell in cells {
                let width = if cell.wide { 2 } else { 1 };
                let colored = match cell.selected {
                    true => Some(selection),
                    false => (cell.bg != screen.background).then_some(cell.bg),
                };
                stretch = match (stretch, colored) {
                    (Some((from, to, color)), Some(bg)) if to == cell.col && color == bg => {
                        Some((from, cell.col + width, color))
                    }
                    (done, colored) => {
                        if let Some((from, to, color)) = done {
                            frame
                                .back
                                .push(cell_quad(at(line, from), to - from, metrics, color));
                        }
                        colored.map(|bg| (cell.col, cell.col + width, bg))
                    }
                };
            }
            if let Some((from, to, color)) = stretch {
                frame
                    .back
                    .push(cell_quad(at(line, from), to - from, metrics, color));
            }

            // Text, a run of one style at a time, each where its column
            // is: a wide character is as wide as the terminal says, not
            // as the font has it.
            let mut start = 0;
            while start < cells.len() {
                let first = &cells[start];
                let mut end = start + 1;
                if !first.wide {
                    while end < cells.len() {
                        let next = &cells[end];
                        let same = !next.wide
                            && next.col == first.col + (end - start)
                            && (next.fg, next.bold, next.italic, next.underline)
                                == (first.fg, first.bold, first.italic, first.underline)
                            && next.strikethrough == first.strikethrough;
                        if !same {
                            break;
                        }
                        end += 1;
                    }
                }
                let text: String = cells[start..end].iter().map(|cell| cell.c).collect();
                let blank = text.trim().is_empty() && !first.underline && !first.strikethrough;
                if !blank {
                    let shaped = shape(text, first, hsla(first.fg));
                    frame.lines.push((at(line, first.col), shaped));
                }
                start = end;
            }
        }

        let focused = self.focus_handle.is_focused(window);
        if let Some(cursor) = screen.cursor.filter(|_| self.is_running()) {
            let under = screen.lines[cursor.line]
                .iter()
                .find(|cell| cell.col == cursor.col);
            let width = cell_width
                * if under.is_some_and(|cell| cell.wide) {
                    2.
                } else {
                    1.
                };
            let origin = at(cursor.line, cursor.col);
            let color = hsla(screen.cursor_color);
            let cell = Bounds::new(origin, size(width, line_height));
            if let Some(marked) = self.marked.as_ref().filter(|marked| !marked.is_empty()) {
                // What is being composed covers the cursor, underlined.
                let mut text_run = run(marked.len(), base_font.clone(), hsla(palette.foreground));
                text_run.underline = Some(UnderlineStyle {
                    thickness: px(1.),
                    color: None,
                    wavy: false,
                });
                let shaped =
                    text_system.shape_line(marked.clone().into(), font_size, &[text_run], None);
                let covered = Bounds::new(origin, size(shaped.width, line_height));
                frame.front.push(fill(covered, background));
                frame.front_lines.push((origin, shaped));
            } else {
                match (cursor.shape, focused) {
                    (CursorKind::Block, true) => {
                        frame.back.push(fill(cell, color));
                        if let Some(under) = under.filter(|cell| cell.c != ' ') {
                            let shaped = shape(under.c.to_string(), under, background);
                            frame.front_lines.push((origin, shaped));
                        }
                    }
                    (CursorKind::Beam, _) => {
                        frame
                            .front
                            .push(fill(Bounds::new(origin, size(px(2.), line_height)), color));
                    }
                    (CursorKind::Underline, _) => {
                        let y = origin.y + line_height - px(2.);
                        frame.front.push(fill(
                            Bounds::new(point(origin.x, y), size(width, px(2.))),
                            color,
                        ));
                    }
                    (CursorKind::Block, false) => {
                        frame.front.push(outline(cell, color, BorderStyle::Solid));
                    }
                }
            }
        }
        if let Some(code) = self.exited {
            let said = match code {
                Some(code) => format!(
                    "[process exited {code}: a key closes the tab, ctrl-\\ ctrl-n reads it]"
                ),
                None => {
                    "[process killed: a key closes the tab, ctrl-\\ ctrl-n reads it]".to_string()
                }
            };
            let muted = cx.theme().muted_foreground;
            let shaped = text_system.shape_line(
                said.clone().into(),
                font_size,
                &[run(said.len(), base_font.clone(), muted)],
                None,
            );
            let line = last_used_line(&screen) + 1;
            frame
                .front_lines
                .push((at(line.min(screen.rows - 1), 0), shaped));
        }
        frame
    }
}

/// The next thing the terminal has to say; `None` once it has no more.
async fn next_event(
    events: &mut futures::channel::mpsc::UnboundedReceiver<Event>,
    cx: &AsyncApp,
) -> Option<Event> {
    #[cfg(not(test))]
    {
        let _ = cx;
        futures::StreamExt::next(events).await
    }
    // The test executor will not be woken from the thread that reads from
    // the program: it looks, on its own clock.
    #[cfg(test)]
    loop {
        use futures::channel::mpsc::TryRecvError;
        cx.background_executor()
            .timer(std::time::Duration::from_millis(10))
            .await;
        match events.try_recv() {
            Ok(event) => return Some(event),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Closed) => return None,
        }
    }
}

/// The last row anything is on.
fn last_used_line(screen: &Screen) -> usize {
    (0..screen.rows)
        .rev()
        .find(|line| !screen.line_text(*line).is_empty())
        .unwrap_or(0)
}

/// `cells` cells from `origin`, filled with `color`.
fn cell_quad(origin: Point<Pixels>, cells: usize, metrics: Metrics, color: Rgb) -> PaintQuad {
    let width = metrics.cell_width * cells as f32;
    fill(
        Bounds::new(origin, size(width, metrics.line_height)),
        hsla(color),
    )
}

fn hsla(color: Rgb) -> Hsla {
    Rgba {
        r: color.r as f32 / 255.,
        g: color.g as f32 / 255.,
        b: color.b as f32 / 255.,
        a: 1.,
    }
    .into()
}

fn rgb_of(color: Hsla) -> Rgb {
    let c = color.to_rgb();
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    Rgb::new(byte(c.r), byte(c.g), byte(c.b))
}

/// The terminal's colors, after the theme's.
fn palette(cx: &App) -> Palette {
    let theme = cx.theme();
    let mut palette = match theme.is_dark() {
        true => Palette::dark(),
        false => Palette::light(),
    };
    palette.foreground = rgb_of(theme.foreground);
    palette.background = rgb_of(theme.background);
    palette.cursor = rgb_of(theme.foreground);
    palette
}

/// What the terminal draws, worked out before it is drawn.
struct Frame {
    bounds: Bounds<Pixels>,
    background: Hsla,
    line_height: Pixels,
    /// Behind the text: colored cells, the block cursor.
    back: Vec<PaintQuad>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    /// Over it: a bar or outline cursor, what is being composed.
    front: Vec<PaintQuad>,
    front_lines: Vec<(Point<Pixels>, ShapedLine)>,
}

impl Frame {
    fn paint(self, window: &mut Window, cx: &mut App) {
        let line_height = self.line_height;
        window.with_content_mask(
            Some(ContentMask {
                bounds: self.bounds,
            }),
            |window| {
                window.paint_quad(fill(self.bounds, self.background));
                for quad in self.back {
                    window.paint_quad(quad);
                }
                for (origin, line) in &self.lines {
                    let _ = line.paint(*origin, line_height, TextAlign::Left, None, window, cx);
                }
                for quad in self.front {
                    window.paint_quad(quad);
                }
                for (origin, line) in &self.front_lines {
                    let _ = line.paint(*origin, line_height, TextAlign::Left, None, window, cx);
                }
            },
        );
    }
}

impl Render for TermView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prepaint_entity = cx.entity();
        let paint_entity = cx.entity();
        let focus_handle = self.focus_handle.clone();
        div()
            .id("terminal")
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll(cx.listener(Self::on_scroll))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        prepaint_entity.update(cx, |view, cx| view.layout_frame(bounds, window, cx))
                    },
                    move |bounds, frame, window, cx| {
                        frame.paint(window, cx);
                        window.handle_input(
                            &focus_handle,
                            ElementInputHandler::new(bounds, paint_entity.clone()),
                            cx,
                        );
                    },
                )
                .size_full(),
            )
    }
}

// ---- input method support ------------------------------------------------

impl EntityInputHandler for TermView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let at = self
            .marked
            .as_ref()
            .map_or(0, |marked| marked.encode_utf16().count());
        Some(UTF16Selection {
            range: at..at,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let marked = self.marked.as_ref()?;
        Some(0..marked.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = None;
        if self.is_running() && !text.is_empty() {
            self.write(text.as_bytes().to_vec());
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = Some(new_text.to_string()).filter(|text| !text.is_empty());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // Where the cursor is, for the input method's window to be by it.
        let metrics = self.metrics?;
        let screen = self.terminal.screen(&Palette::dark());
        let cursor = screen.cursor?;
        Some(Bounds::new(
            point(
                metrics.origin.x + metrics.cell_width * cursor.col as f32,
                metrics.origin.y + metrics.line_height * cursor.line as f32,
            ),
            size(metrics.cell_width, metrics.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.is_running()
    }
}
