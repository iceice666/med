//! A terminal for med: a shell on a pty, and the screen it draws.
//!
//! alacritty's terminal does the work: it reads what the program prints
//! on a thread of its own and keeps the grid. Here is only what a view
//! needs of it: start one, give it keys and a size, and take a copy of
//! the screen to draw, in colors already worked out.

mod keys;

pub use keys::{Modifiers, encode_key};

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{self, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{self, CursorShape, NamedColor};
use futures::channel::mpsc;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// A color, as the screen has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Two thirds as bright, for faint text.
    fn dim(self) -> Self {
        let third = |c: u8| (c as u16 * 2 / 3) as u8;
        Self::new(third(self.r), third(self.g), third(self.b))
    }
}

impl From<ansi::Rgb> for Rgb {
    fn from(c: ansi::Rgb) -> Self {
        Self::new(c.r, c.g, c.b)
    }
}

/// The colors a program names rather than spells out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// Black, red, green, yellow, blue, magenta, cyan, white; then their
    /// bright kinds.
    pub ansi: [Rgb; 16],
}

impl Palette {
    /// xterm's colors, on a dark background.
    pub fn dark() -> Self {
        Self {
            foreground: Rgb::new(0xd4, 0xd4, 0xd4),
            background: Rgb::new(0x1e, 0x1e, 0x1e),
            cursor: Rgb::new(0xd4, 0xd4, 0xd4),
            ansi: [
                Rgb::new(0x00, 0x00, 0x00),
                Rgb::new(0xcd, 0x31, 0x31),
                Rgb::new(0x0d, 0xbc, 0x79),
                Rgb::new(0xe5, 0xe5, 0x10),
                Rgb::new(0x24, 0x72, 0xc8),
                Rgb::new(0xbc, 0x3f, 0xbc),
                Rgb::new(0x11, 0xa8, 0xcd),
                Rgb::new(0xe5, 0xe5, 0xe5),
                Rgb::new(0x66, 0x66, 0x66),
                Rgb::new(0xf1, 0x4c, 0x4c),
                Rgb::new(0x23, 0xd1, 0x8b),
                Rgb::new(0xf5, 0xf5, 0x43),
                Rgb::new(0x3b, 0x8e, 0xea),
                Rgb::new(0xd6, 0x70, 0xd6),
                Rgb::new(0x29, 0xb8, 0xdb),
                Rgb::new(0xff, 0xff, 0xff),
            ],
        }
    }

    /// The same, made to be read on a light background.
    pub fn light() -> Self {
        Self {
            foreground: Rgb::new(0x33, 0x33, 0x33),
            background: Rgb::new(0xff, 0xff, 0xff),
            cursor: Rgb::new(0x33, 0x33, 0x33),
            ansi: [
                Rgb::new(0x00, 0x00, 0x00),
                Rgb::new(0xcd, 0x31, 0x31),
                Rgb::new(0x00, 0xbc, 0x00),
                Rgb::new(0x94, 0x98, 0x00),
                Rgb::new(0x04, 0x51, 0xa5),
                Rgb::new(0xbc, 0x05, 0xbc),
                Rgb::new(0x05, 0x98, 0xbc),
                Rgb::new(0x55, 0x55, 0x55),
                Rgb::new(0x66, 0x66, 0x66),
                Rgb::new(0xcd, 0x31, 0x31),
                Rgb::new(0x14, 0xce, 0x14),
                Rgb::new(0xb5, 0xba, 0x00),
                Rgb::new(0x04, 0x51, 0xa5),
                Rgb::new(0xbc, 0x05, 0xbc),
                Rgb::new(0x05, 0x98, 0xbc),
                Rgb::new(0xa5, 0xa5, 0xa5),
            ],
        }
    }

    /// The color at `index` of the 256 a program may name by number: the
    /// sixteen, a 6×6×6 cube, then 24 greys.
    fn indexed(&self, index: u8) -> Rgb {
        match index {
            0..16 => self.ansi[index as usize],
            16..232 => {
                let level = |n: u8| if n == 0 { 0 } else { n * 40 + 55 };
                let n = index - 16;
                Rgb::new(level(n / 36), level(n / 6 % 6), level(n % 6))
            }
            _ => {
                let grey = (index - 232) * 10 + 8;
                Rgb::new(grey, grey, grey)
            }
        }
    }
}

/// What a terminal has to tell its view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// There is something new on the screen.
    Wakeup,
    /// The program gave the terminal a title.
    Title(String),
    /// The program took its title back.
    ResetTitle,
    Bell,
    /// The program is done, with its exit code if it exited rather than
    /// being killed.
    Exited(Option<i32>),
}

/// How to start a terminal.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The program to run; the user's login shell when `None`.
    pub program: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Set for the program, over what med itself has.
    pub env: HashMap<String, String>,
    pub cols: u16,
    pub rows: u16,
}

/// What alacritty says, passed on to the view; and what it asks to have
/// written back to the program, written. The screen has one and the loop
/// reading from the program another, both to the same view.
#[derive(Clone)]
struct Listener {
    events: mpsc::UnboundedSender<Event>,
    pty: Arc<OnceLock<EventLoopSender>>,
    /// Told by the loop when the program exits, before the screen says so.
    exit_code: Arc<Mutex<Option<i32>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let event = match event {
            TermEvent::Wakeup => Event::Wakeup,
            TermEvent::Title(title) => Event::Title(title),
            TermEvent::ResetTitle => Event::ResetTitle,
            TermEvent::Bell => Event::Bell,
            TermEvent::PtyWrite(text) => {
                if let Some(pty) = self.pty.get() {
                    let _ = pty.send(Msg::Input(text.into_bytes().into()));
                }
                return;
            }
            TermEvent::ChildExit(status) => {
                *self.exit_code.lock().expect("not poisoned") = status.code();
                return;
            }
            TermEvent::Exit => Event::Exited(*self.exit_code.lock().expect("not poisoned")),
            _ => return,
        };
        let _ = self.events.unbounded_send(event);
    }
}

/// A size in cells, as alacritty measures a screen.
#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// A program running on a pty, and its screen.
pub struct Terminal {
    term: Arc<FairMutex<Term<Listener>>>,
    pty: EventLoopSender,
    cols: u16,
    rows: u16,
}

impl Terminal {
    /// Start `options.program` on a new pty. What it has to say comes on
    /// the receiver, until it exits.
    pub fn spawn(options: Options) -> anyhow::Result<(Self, mpsc::UnboundedReceiver<Event>)> {
        let (cols, rows) = (options.cols.max(1), options.rows.max(1));
        let (tx, rx) = mpsc::unbounded();
        let pty_cell = Arc::new(OnceLock::new());
        let listener = Listener {
            events: tx,
            pty: pty_cell.clone(),
            exit_code: Arc::default(),
        };
        let size = Size {
            cols: cols as usize,
            rows: rows as usize,
        };
        let term = Arc::new(FairMutex::new(Term::new(
            term::Config::default(),
            &size,
            listener.clone(),
        )));
        // Not `tty::setup_env`, which sets them for med and so for every
        // program it starts.
        let mut env = options.env;
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        let tty_options = tty::Options {
            shell: options
                .program
                .map(|program| tty::Shell::new(program, options.args)),
            working_directory: options.cwd,
            drain_on_exit: true,
            env,
            #[cfg(windows)]
            escape_args: false,
        };
        let window_size = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: 1,
            cell_height: 1,
        };
        let pty = tty::new(&tty_options, window_size, 0)?;
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let sender = event_loop.channel();
        let _ = pty_cell.set(sender.clone());
        event_loop.spawn();
        Ok((
            Self {
                term,
                pty: sender,
                cols,
                rows,
            },
            rx,
        ))
    }

    /// Send `bytes` to the program, as if typed.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.pty.send(Msg::Input(bytes.into()));
    }

    /// Text pasted in: marked as such if the program asked for that, so
    /// that a shell does not run it line by line.
    pub fn paste(&self, text: &str) {
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        if bracketed {
            // What would end the paste early is left out.
            let text = text.replace("\x1b[201~", "");
            self.write(format!("\x1b[200~{text}\x1b[201~").into_bytes());
        } else {
            self.write(text.replace("\r\n", "\r").replace('\n', "\r").into_bytes());
        }
    }

    /// Whether the arrow keys are to be sent in the application's form.
    pub fn app_cursor(&self) -> bool {
        self.term.lock().mode().contains(TermMode::APP_CURSOR)
    }

    /// Whether the wheel is to be arrow keys: a program that has the
    /// whole screen (an editor, a pager) has no scrollback to scroll.
    pub fn alternate_scroll(&self) -> bool {
        let term = self.term.lock();
        let mode = term.mode();
        mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL)
    }

    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Make the screen `cols` by `rows` cells, each `cell_width` by
    /// `cell_height` pixels. False if it was that already.
    pub fn resize(&mut self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> bool {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) {
            return false;
        }
        self.cols = cols;
        self.rows = rows;
        // The pty, for the program to know; the grid, for us.
        let _ = self.pty.send(Msg::Resize(WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width,
            cell_height,
        }));
        self.term.lock().resize(Size {
            cols: cols as usize,
            rows: rows as usize,
        });
        true
    }

    /// Scroll back through what went by, `lines` up (or down, if fewer
    /// than none).
    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    /// Back to the bottom, where the program writes.
    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    /// A screenful back through what went by, or forward.
    pub fn scroll_page(&self, up: bool) {
        let page = if up { Scroll::PageUp } else { Scroll::PageDown };
        self.term.lock().scroll_display(page);
    }

    /// Start selecting with the mouse at row `line` of the screen as it is
    /// shown, column `col`; `right` if on the right half of the cell.
    pub fn select_start(&self, line: usize, col: usize, right: bool, kind: SelectKind) {
        let mut term = self.term.lock();
        let point = shown_point(&term, line, col);
        let ty = match kind {
            SelectKind::Simple => SelectionType::Simple,
            SelectKind::Word => SelectionType::Semantic,
            SelectKind::Line => SelectionType::Lines,
        };
        term.selection = Some(Selection::new(ty, point, side(right)));
    }

    /// The mouse, still down, is at `line`, `col` now.
    pub fn select_update(&self, line: usize, col: usize, right: bool) {
        let mut term = self.term.lock();
        let point = shown_point(&term, line, col);
        if let Some(selection) = &mut term.selection {
            selection.update(point, side(right));
        }
    }

    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    /// The text selected with the mouse, if any.
    pub fn selection_text(&self) -> Option<String> {
        let term = self.term.lock();
        let text = term.selection_to_string()?;
        (!text.is_empty()).then_some(text)
    }

    /// Everything there is, what went by and the screen, as text: a line
    /// for each row, a wide character once. With it, where a cursor goes:
    /// where the program's is, or if what went by is being looked at, the
    /// top of what is shown.
    pub fn history(&self) -> History {
        let term = self.term.lock();
        let grid = term.grid();
        let top = grid.topmost_line().0;
        let bottom = grid.bottommost_line().0;
        let row_text = |line: i32, upto: Option<usize>| -> (String, usize) {
            let row = &grid[Line(line)];
            let mut text = String::new();
            let mut chars_before = 0;
            for col in 0..grid.columns() {
                let cell = &row[Column(col)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                if upto.is_some_and(|upto| col < upto) {
                    chars_before += 1;
                }
                text.push(cell.c);
                if let Some(zerowidth) = cell.zerowidth() {
                    text.extend(zerowidth);
                }
            }
            (text.trim_end().to_string(), chars_before)
        };
        let offset = grid.display_offset() as i32;
        let (at_line, at_col) = match offset {
            0 => {
                let cursor = grid.cursor.point;
                (cursor.line.0, cursor.column.0)
            }
            _ => (-offset, 0),
        };
        let mut lines = Vec::new();
        let mut col = 0;
        for line in top..=bottom {
            let upto = (line == at_line).then_some(at_col);
            let (text, before) = row_text(line, upto);
            if line == at_line {
                col = before;
            }
            lines.push(text);
        }
        let cursor_line = (at_line - top) as usize;
        // Blank rows below the cursor are only the screen not yet used.
        while lines.len() > cursor_line + 1 && lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        History {
            text: lines.join("\n"),
            line: cursor_line,
            col,
        }
    }

    /// A copy of the screen as it is now, in `palette`'s colors.
    pub fn screen(&self, palette: &Palette) -> Screen {
        let term = self.term.lock();
        let content = term.renderable_content();
        let colors = content.colors;
        let resolve = |color: ansi::Color| -> Rgb {
            match color {
                ansi::Color::Spec(rgb) => rgb.into(),
                ansi::Color::Indexed(index) => colors[index as usize]
                    .map(Rgb::from)
                    .unwrap_or_else(|| palette.indexed(index)),
                ansi::Color::Named(named) => colors[named]
                    .map(Rgb::from)
                    .unwrap_or_else(|| named_color(named, palette)),
            }
        };
        let selection = content.selection;
        let (cols, rows) = (term.columns(), term.screen_lines());
        let offset = content.display_offset as i32;
        let mut lines: Vec<Vec<Cell>> = (0..rows).map(|_| Vec::new()).collect();
        for indexed in content.display_iter {
            let cell = &indexed.cell;
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let row = indexed.point.line.0 + offset;
            let Some(line) = usize::try_from(row).ok().and_then(|row| lines.get_mut(row)) else {
                continue;
            };
            let flags = cell.flags;
            let (mut fg, mut bg) = (resolve(cell.fg), resolve(cell.bg));
            if flags.contains(Flags::DIM) {
                fg = fg.dim();
            }
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            line.push(Cell {
                col: indexed.point.column.0,
                c: cell.c,
                fg,
                bg,
                bold: flags.contains(Flags::BOLD),
                italic: flags.contains(Flags::ITALIC),
                underline: flags.intersects(Flags::ALL_UNDERLINES),
                strikethrough: flags.contains(Flags::STRIKEOUT),
                wide: flags.contains(Flags::WIDE_CHAR),
                selected: selection.is_some_and(|range| range.contains(indexed.point)),
            });
        }
        let cursor_line = content.cursor.point.line.0 + offset;
        let cursor = match content.cursor.shape {
            CursorShape::Hidden => None,
            shape => usize::try_from(cursor_line)
                .ok()
                .filter(|line| *line < rows)
                .map(|line| Cursor {
                    line,
                    col: content.cursor.point.column.0,
                    shape: match shape {
                        CursorShape::Beam => CursorKind::Beam,
                        CursorShape::Underline => CursorKind::Underline,
                        _ => CursorKind::Block,
                    },
                }),
        };
        Screen {
            cols,
            rows,
            lines,
            cursor,
            cursor_color: colors[NamedColor::Cursor]
                .map(Rgb::from)
                .unwrap_or(palette.cursor),
            background: resolve(ansi::Color::Named(NamedColor::Background)),
            scrolled: content.display_offset > 0,
        }
    }

    /// Stop reading from the program, which closes the pty: the program
    /// is hung up on.
    pub fn shutdown(&self) {
        let _ = self.pty.send(Msg::Shutdown);
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The point of the grid that row `line` of the screen, as it is shown,
/// has at `col`.
fn shown_point<T>(term: &Term<T>, line: usize, col: usize) -> Point {
    let grid = term.grid();
    let line =
        (line.min(grid.screen_lines().saturating_sub(1)) as i32) - grid.display_offset() as i32;
    let col = col.min(grid.columns().saturating_sub(1));
    Point::new(Line(line), Column(col))
}

fn side(right: bool) -> Side {
    if right { Side::Right } else { Side::Left }
}

/// What a mouse selection takes in as it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectKind {
    /// Cell by cell.
    Simple,
    /// Word by word: a double click.
    Word,
    /// Line by line: a triple click.
    Line,
}

/// The terminal's text: see [`Terminal::history`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct History {
    pub text: String,
    /// Where the cursor goes: the line, from 0, and the character in it
    /// (not the column: a wide character is one).
    pub line: usize,
    pub col: usize,
}

fn named_color(named: NamedColor, palette: &Palette) -> Rgb {
    let index = named as usize;
    match named {
        _ if index < 16 => palette.ansi[index],
        NamedColor::Foreground | NamedColor::BrightForeground => palette.foreground,
        NamedColor::Background => palette.background,
        NamedColor::Cursor => palette.cursor,
        NamedColor::DimForeground => palette.foreground.dim(),
        // The dim eight, after `DimBlack`.
        _ => {
            let dim = index - NamedColor::DimBlack as usize;
            palette.ansi[dim.min(7)].dim()
        }
    }
}

/// The screen, to be drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    /// For each row, its cells from the left, each saying which column it
    /// is in: a wide character takes two, and the second is not here.
    pub lines: Vec<Vec<Cell>>,
    pub cursor: Option<Cursor>,
    pub cursor_color: Rgb,
    /// The program's background, where it draws none of its own.
    pub background: Rgb,
    /// Whether this is not the bottom but what went by before.
    pub scrolled: bool,
}

impl Screen {
    /// The text of row `line`, the end of it trimmed.
    pub fn line_text(&self, line: usize) -> String {
        let text: String = self.lines[line].iter().map(|cell| cell.c).collect();
        text.trim_end().to_string()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub col: usize,
    pub c: char,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    /// Two columns wide.
    pub wide: bool,
    /// In what is selected with the mouse.
    pub selected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub line: usize,
    pub col: usize,
    pub shape: CursorKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorKind {
    Block,
    Beam,
    Underline,
}

#[cfg(test)]
mod tests;
