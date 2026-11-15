//! The modal editing engine: a key-driven state machine over a [`Buffer`].
//!
//! The engine is UI-agnostic. The host feeds it [`Key`]s and applies the
//! returned [`Effect`]s for anything that reaches outside the buffer (ex
//! commands such as `:w`, LSP requests, scrolling).
//!
//! Which key does what is data: the [`Keymap`] in [`Shared`] binds key
//! sequences to named commands. The grammar those commands plug into —
//! counts, registers, operator + motion — lives here.

use crate::command::{Action, Await, Builtin, Cmd, InsertCmd};
use crate::ex::{self, RangeCtx};
use crate::flash::{Flash, FlashMatch};
use crate::keymap::{Keymap, Lookup, MapMode, format_keys, parse_keys};
use crate::motion::{self, Find, WS, class, first_non_blank, leading_ws};
use crate::textobj::{self, ObjRange};
use med_core::Buffer;
use regex::Regex;
use std::collections::{HashMap, VecDeque};

/// Keys run for one key press before giving up on a mapping as endless.
const MAX_MAPPED_KEYS: usize = 1000;

/// Keys one `@` runs before a macro that calls itself is given up on.
const MAX_MACRO_KEYS: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    /// Literal text for insert mode or the command line (a paste). Unlike
    /// `Char` it gets no auto-indent treatment.
    Text(String),
    Ctrl(char),
    Esc,
    Enter,
    Backspace,
    Delete,
    Tab,
    /// Shift-Tab.
    BackTab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

impl Key {
    fn is_escape(&self) -> bool {
        matches!(self, Key::Esc | Key::Ctrl('[') | Key::Ctrl('c'))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Visual,
    VisualLine,
    /// A rectangle of text: `ctrl-v`.
    VisualBlock,
    /// Editing the `:` / `/` / `?` command line.
    Command,
}

impl Mode {
    pub fn is_visual(self) -> bool {
        matches!(self, Mode::Visual | Mode::VisualLine | Mode::VisualBlock)
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
            Mode::VisualBlock => "V-BLOCK",
            Mode::Command => "COMMAND",
        }
    }
}

/// The lines currently on screen, for `H`/`M`/`L` and page scrolling.
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub top: usize,
    pub height: usize,
}

/// Per-language editing settings.
#[derive(Clone, Debug)]
pub struct Lang {
    pub tab_width: usize,
    pub expand_tab: bool,
    pub line_comment: Option<String>,
    /// What is closed as it is opened, while [`Shared::auto_pairs`] is on.
    pub pairs: Vec<(char, char)>,
}

/// The brackets and quotes most languages close.
pub const PAIRS: &[(char, char)] = &[
    ('(', ')'),
    ('[', ']'),
    ('{', '}'),
    ('"', '"'),
    ('\'', '\''),
    ('`', '`'),
];

impl Default for Lang {
    fn default() -> Self {
        Self {
            tab_width: 4,
            expand_tab: true,
            line_comment: None,
            pairs: PAIRS.to_vec(),
        }
    }
}

impl Lang {
    pub fn indent_unit(&self) -> String {
        if self.expand_tab {
            " ".repeat(self.tab_width.max(1))
        } else {
            "\t".to_string()
        }
    }
}

/// System clipboard access, supplied by the host.
pub trait Clipboard {
    fn get(&mut self) -> Option<String>;
    fn set(&mut self, text: &str);
}

/// An in-memory clipboard for tests and headless use.
#[derive(Default)]
pub struct MemClipboard(pub Option<String>);

impl Clipboard for MemClipboard {
    fn get(&mut self) -> Option<String> {
        self.0.clone()
    }
    fn set(&mut self, text: &str) {
        self.0 = Some(text.to_string());
    }
}

/// What the host knows of the text's structure, for the text objects and
/// motions that go by it (`if`, `]f`, expanding the selection). An engine
/// with no host to ask does without them.
pub trait SyntaxTree {
    /// The text object `kind` at `pos`, as `start..end` in chars: `f` a
    /// function, `t` a type, `a` an argument, `c` a comment.
    fn object(
        &mut self,
        buf: &Buffer,
        pos: usize,
        kind: char,
        around: bool,
    ) -> Option<(usize, usize)>;

    /// The smallest node that holds `start..end` and more.
    fn enclosing(&mut self, buf: &Buffer, start: usize, end: usize) -> Option<(usize, usize)>;

    /// Where the next (or the previous) object of `kind` starts.
    fn neighbor(&mut self, buf: &Buffer, pos: usize, kind: char, forward: bool) -> Option<usize>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Register {
    pub text: String,
    pub linewise: bool,
    /// A rectangle, one line of it to a line of `text`.
    pub blockwise: bool,
}

pub struct Search {
    pub pattern: String,
    pub regex: Regex,
    pub forward: bool,
}

/// State shared by every editor: the key bindings, registers, the last
/// search, the last repeatable change.
#[derive(Default)]
pub struct Shared {
    pub keymap: Keymap,
    registers: HashMap<char, Register>,
    pub search: Option<Search>,
    pub hlsearch: bool,
    /// Close brackets and quotes as they are typed.
    pub auto_pairs: bool,
    last_find: Option<Find>,
    last_change: Vec<Key>,
    /// The keys recorded into a register, with the text they were stored
    /// as: while the register still holds that text, these are run rather
    /// than the text read back, which cannot say everything (a paste).
    macros: HashMap<char, (String, Vec<Key>)>,
    last_macro: Option<char>,
}

impl Shared {
    fn write_register(
        &mut self,
        name: Option<char>,
        reg: Register,
        yank: bool,
        clipboard: &mut dyn Clipboard,
    ) {
        match name {
            Some('_') => return,
            Some(c) if c.is_ascii_uppercase() => {
                let entry = self.registers.entry(c.to_ascii_lowercase()).or_default();
                entry.text.push_str(&reg.text);
                entry.linewise |= reg.linewise;
            }
            Some(c) if c.is_ascii_lowercase() => {
                self.registers.insert(c, reg.clone());
            }
            // The unnamed register is kept in sync with the system clipboard.
            _ => clipboard.set(&reg.text),
        }
        if yank {
            self.registers.insert('0', reg.clone());
        }
        self.registers.insert('"', reg);
    }

    fn read_register(
        &mut self,
        name: Option<char>,
        clipboard: &mut dyn Clipboard,
    ) -> Option<Register> {
        match name {
            None | Some('"') | Some('+') | Some('*') => {
                let unnamed = self.registers.get(&'"');
                match clipboard.get() {
                    // Text copied elsewhere wins; our own yank keeps its
                    // linewise flag.
                    Some(clip) if unnamed.is_none_or(|r| r.text != clip) => Some(Register {
                        linewise: clip.ends_with('\n'),
                        text: clip,
                        blockwise: false,
                    }),
                    _ => unnamed.cloned(),
                }
            }
            Some(c) => self.registers.get(&c.to_ascii_lowercase()).cloned(),
        }
    }

    pub fn register(&self, name: char) -> Option<&Register> {
        self.registers.get(&name)
    }

    #[cfg(test)]
    pub(crate) fn registers_insert_for_test(&mut self, name: char, text: &str) {
        let register = Register {
            text: text.to_string(),
            ..Register::default()
        };
        self.registers.insert(name, register);
    }
}

pub struct Ctx<'a> {
    pub buf: &'a mut Buffer,
    pub shared: &'a mut Shared,
    pub clipboard: &'a mut dyn Clipboard,
    pub viewport: Viewport,
    pub lang: &'a Lang,
    pub syntax: Option<&'a mut dyn SyntaxTree>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scroll {
    /// `zz`
    Center,
    /// `zt`
    Top,
    /// `zb`
    Bottom,
    /// Scroll the view by this many lines (positive = down).
    Lines(isize),
}

/// Something the host has to do on the engine's behalf.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// An ex command the engine does not handle itself (`w`, `q`, `e foo`…).
    Ex(String),
    Scroll(Scroll),
    /// The cursor is about to make a long jump from this offset.
    Jump {
        from: usize,
    },
    /// A key bound to a command that is not one of the engine's own
    /// (see [`command_names`](crate::command_names)): the host runs it.
    Command {
        name: String,
        count: Option<usize>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operator {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
    Lower,
    Upper,
    ToggleCase,
    Comment,
    /// Put a delimiter, asked for next, around the target.
    Surround,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordFwd(bool),
    WordBack(bool),
    WordEnd(bool),
    WordEndBack(bool),
    LineStart,
    FirstNonBlank,
    LineEnd,
    /// `G` (defaults to the last line) / `gg` (defaults to the first).
    Line {
        default_last: bool,
    },
    Find(Find),
    RepeatFind {
        reverse: bool,
    },
    MatchPair,
    ParaFwd,
    ParaBack,
    ScreenTop,
    ScreenMid,
    ScreenBot,
    Page {
        down: bool,
        half: bool,
    },
    SearchNext {
        reverse: bool,
    },
    WordSearch {
        forward: bool,
    },
    /// `+` / Enter
    NextLine,
    /// `-`
    PrevLine,
    /// `_`
    CurLine,
    /// `|`
    Column,
    /// `'a` / `` `a ``
    Mark {
        name: char,
        linewise: bool,
    },
    /// `]f`, `[a`…: the next or previous thing of a kind in the syntax tree.
    Syntax {
        kind: char,
        forward: bool,
    },
    /// To a place `motion.flash` found, and with an operator onto it.
    Flash(usize),
}

impl Motion {
    fn is_jump(self) -> bool {
        matches!(
            self,
            Motion::Line { .. }
                | Motion::MatchPair
                | Motion::ParaFwd
                | Motion::ParaBack
                | Motion::ScreenTop
                | Motion::ScreenMid
                | Motion::ScreenBot
                | Motion::SearchNext { .. }
                | Motion::WordSearch { .. }
                | Motion::Mark { .. }
                | Motion::Syntax { .. }
                | Motion::Flash(_)
        )
    }
}

struct MotionOut {
    pos: usize,
    inclusive: bool,
    linewise: bool,
    keep_goal: bool,
    scroll: Option<isize>,
}

/// What an operator acts on.
#[derive(Clone, Copy, Debug)]
enum Target {
    /// `start..end` in chars.
    Chars(usize, usize),
    /// Whole lines, inclusive.
    Lines(usize, usize),
}

/// A command that is only partly typed.
#[derive(Default)]
struct Pending {
    count: Option<usize>,
    op: Option<Operator>,
    /// The key that finished the operator's binding (`d`, the `u` of
    /// `gu`): pressed again, it applies the operator to whole lines.
    op_key: Option<Key>,
    op_count: Option<usize>,
    register: Option<char>,
    /// A command waiting for its literal argument (`f`, `r`, `"`…).
    awaiting: Option<Await>,
    /// Keys that so far only start a binding (`g`, the `space` of a
    /// leader binding).
    seq: Vec<Key>,
    /// Every key of the command so far, for display.
    typed: Vec<Key>,
    /// `motion.flash` is looking for text: keys are that text, or the
    /// label of a place to go to.
    flash: Option<Flash>,
}

impl Pending {
    fn is_idle(&self) -> bool {
        self.count.is_none()
            && self.op.is_none()
            && self.op_count.is_none()
            && self.register.is_none()
            && self.awaiting.is_none()
            && self.seq.is_empty()
            && self.flash.is_none()
    }

    fn count(&self) -> Option<usize> {
        match (self.count, self.op_count) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(1).saturating_mul(b.unwrap_or(1))),
        }
    }
}

/// A half-typed command, as [`Vim::pending_keys`] describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingKeys {
    /// The table the next key is looked up in.
    pub mode: MapMode,
    /// The start of a binding typed so far (`g`, `<Space>`), in key
    /// notation: what [`Keymap::next_keys`](crate::Keymap::next_keys)
    /// takes. Empty after a count or an operator, when any binding of
    /// `mode` can follow.
    pub keys: String,
    /// Every key of the command so far (`"a2d`), as
    /// [`Vim::pending_display`] has it.
    pub typed: String,
    /// The next key is not looked up in a table but taken as it is.
    pub awaiting: Option<Awaiting>,
}

/// What a command takes its next key as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Awaiting {
    /// The name of a register, after `"`, `q` or `@`.
    Register,
    /// A character to find or to replace with, after `f` or `r`, or a
    /// delimiter to surround with.
    Char,
    /// What kind of text object, after `i` or `a`.
    TextObject,
    /// The name of a mark, after `m`, `'` or `` ` ``.
    Mark,
    /// More of the text `motion.flash` looks for, or the label of a place.
    Flash,
}

struct CmdLine {
    kind: char,
    text: String,
}

/// What a surround operator was given, until the delimiter is typed.
struct Surround {
    /// `start..end` in chars: one, or a block's line by line.
    ranges: Vec<(usize, usize)>,
    /// Whole lines were selected: the delimiters get lines of their own.
    own_lines: bool,
}

/// An insert that began on a block (`I`, `A`, `c`): what is typed on its
/// first line goes on every other line when the insert ends.
struct BlockInsert {
    /// Where the typing began.
    start: usize,
    /// The other lines, inclusive.
    rows: (usize, usize),
    col: usize,
    /// Lines that end before `col` are filled up to it rather than left out.
    pad: bool,
}

/// One step of `select.expand`, to step back from.
struct Expansion {
    from: (usize, usize, Mode),
    to: (usize, usize),
}

pub struct Vim {
    pub mode: Mode,
    /// Char offset of the cursor.
    pub cursor: usize,
    /// The other end of the visual selection.
    pub anchor: usize,
    goal_col: Option<usize>,
    pending: Pending,
    cmdline: Option<CmdLine>,
    /// Keys of the command in progress, for `.`.
    keys: Vec<Key>,
    cmd_start_mode: Mode,
    cmd_changed: bool,
    no_repeat: bool,
    replaying: bool,
    /// Keys put back to be run next: the rest of a sequence whose start
    /// turned out to be a shorter binding, or what a key is mapped to.
    unread: Vec<Key>,
    last_visual: Option<(usize, usize, Mode)>,
    /// A one-line message for the status bar (errors, substitution counts).
    pub message: Option<String>,
    /// Places by name (`ma`), and `'` for where the last jump left from.
    marks: HashMap<char, usize>,
    /// The buffer version the marks are up to date with.
    marks_version: u64,
    surround: Option<Surround>,
    block_insert: Option<BlockInsert>,
    expansions: Vec<Expansion>,
    /// The register keys are being recorded into, and the keys so far.
    recording: Option<(char, Vec<Key>)>,
    /// How many keys were recorded when the command being typed began.
    record_mark: usize,
    /// Keys a `@` asked to have run, once the command it ended is done.
    macro_request: Option<Vec<Key>>,
    /// The keys being run are a macro's: they are not recorded again.
    playing: bool,
}

impl Default for Vim {
    fn default() -> Self {
        Self::new()
    }
}

fn minmax(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

impl Vim {
    pub fn new() -> Self {
        Self {
            mode: Mode::Normal,
            cursor: 0,
            anchor: 0,
            goal_col: None,
            pending: Pending::default(),
            cmdline: None,
            keys: Vec::new(),
            cmd_start_mode: Mode::Normal,
            cmd_changed: false,
            no_repeat: false,
            replaying: false,
            unread: Vec::new(),
            last_visual: None,
            message: None,
            marks: HashMap::new(),
            marks_version: 0,
            surround: None,
            block_insert: None,
            expansions: Vec::new(),
            recording: None,
            record_mark: 0,
            macro_request: None,
            playing: false,
        }
    }

    // ---- host-facing state ---------------------------------------------

    /// The command line being edited: its prompt char and text.
    pub fn cmdline(&self) -> Option<(char, &str)> {
        self.cmdline.as_ref().map(|c| (c.kind, c.text.as_str()))
    }

    /// Keys typed so far for an unfinished command (vim's `showcmd`).
    pub fn pending_display(&self) -> String {
        format_keys(&self.pending.typed)
    }

    /// True while a multi-key command is half typed.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_idle()
    }

    /// What a half-typed command is waiting for, for a host that shows
    /// which keys can come next. `None` when no command is half typed.
    pub fn pending_keys(&self) -> Option<PendingKeys> {
        if self.pending.is_idle() {
            return None;
        }
        let flash = self.pending.flash.as_ref().map(|_| Awaiting::Flash);
        Some(PendingKeys {
            mode: self.map_mode(),
            keys: format_keys(&self.pending.seq),
            typed: self.pending_display(),
            awaiting: flash.or(self.pending.awaiting.map(|awaiting| match awaiting {
                Await::Register | Await::RecordRegister | Await::PlayRegister => Awaiting::Register,
                Await::Find { .. }
                | Await::Replace
                | Await::SurroundAdd
                | Await::SurroundDelete
                | Await::SurroundChange
                | Await::SurroundChangeTo(_) => Awaiting::Char,
                Await::TextObject { .. } => Awaiting::TextObject,
                Await::SetMark | Await::Mark { .. } => Awaiting::Mark,
            })),
        })
    }

    /// The places on screen `motion.flash` has found so far, in the order
    /// of the text, for a host to mark; `None` when it is not looking.
    pub fn flash(&self) -> Option<&[FlashMatch]> {
        let flash = self.pending.flash.as_ref()?;
        Some(&flash.matches)
    }

    /// The register keys are being recorded into, after `q`.
    pub fn recording(&self) -> Option<char> {
        self.recording.as_ref().map(|(name, _)| *name)
    }

    /// Where the mark `name` is, if it is set.
    pub fn mark(&self, name: char) -> Option<usize> {
        match name {
            '`' => self.marks.get(&'\'').copied(),
            '<' | '>' => {
                let (anchor, cursor, _) = self.last_visual?;
                let (s, e) = minmax(anchor, cursor);
                Some(if name == '<' { s } else { e })
            }
            name => self.marks.get(&name).copied(),
        }
    }

    /// Move the marks along with the edits made to `buf` since they were
    /// last looked at. The engine does this for its own edits; a host that
    /// edits the buffer itself calls it before it takes the edits away.
    pub fn sync(&mut self, buf: &Buffer) {
        let version = buf.version();
        if version == self.marks_version {
            return;
        }
        let edits = buf.edits();
        // Each edit is one version; the oldest still listed came after this.
        let listed_from = version.saturating_sub(edits.len() as u64);
        let skip = self.marks_version.saturating_sub(listed_from) as usize;
        for edit in edits.iter().skip(skip) {
            for mark in self.marks.values_mut() {
                if *mark >= edit.old_end_char {
                    *mark = *mark + edit.new_end_char - edit.old_end_char;
                } else if *mark > edit.start_char {
                    *mark = edit.start_char;
                }
            }
        }
        let max = buf.max_offset();
        for mark in self.marks.values_mut() {
            *mark = (*mark).min(max);
        }
        self.marks_version = version;
    }

    /// The visual selection as `start..end` chars, if any.
    pub fn selection(&self, buf: &Buffer) -> Option<(usize, usize)> {
        let (s, e) = minmax(self.anchor, self.cursor);
        match self.mode {
            Mode::Visual => Some((s, (e + 1).min(buf.len_chars()))),
            Mode::VisualLine => Some((
                buf.line_start(buf.line_of(s)),
                buf.line_end(buf.line_of(e)) + 1,
            )),
            // From the block's first char to its last: what it spans, not
            // what it holds. See [`Vim::selections`].
            Mode::VisualBlock => {
                let ranges = self.block_ranges(buf);
                Some((ranges.first()?.0, ranges.last()?.1))
            }
            _ => None,
        }
    }

    /// Everything selected, as `start..end` chars: one range, or in
    /// blockwise visual mode one for each line of the block.
    pub fn selections(&self, buf: &Buffer) -> Vec<(usize, usize)> {
        match self.mode {
            Mode::VisualBlock => self.block_ranges(buf),
            _ => self.selection(buf).into_iter().collect(),
        }
    }

    /// The block between the anchor and the cursor, line by line: the part
    /// of each line that lies within its columns, which for a short line
    /// is nothing.
    fn block_ranges(&self, buf: &Buffer) -> Vec<(usize, usize)> {
        let (rows, cols) = self.block(buf);
        (rows.0..=rows.1)
            .map(|row| {
                let (ls, len) = (buf.line_start(row), buf.line_len(row));
                (ls + cols.0.min(len), ls + (cols.1 + 1).min(len))
            })
            .collect()
    }

    /// The rows and the columns of the block, both inclusive.
    fn block(&self, buf: &Buffer) -> ((usize, usize), (usize, usize)) {
        let (a, c) = (
            buf.offset_to_point(self.anchor),
            buf.offset_to_point(self.cursor),
        );
        (minmax(a.row, c.row), minmax(a.col, c.col))
    }

    /// Move the cursor from outside (mouse click, goto-definition…).
    pub fn set_cursor(&mut self, offset: usize, buf: &Buffer) {
        if self.mode.is_visual() {
            self.exit_visual();
        }
        self.cursor = offset;
        self.goal_col = None;
        self.surround = None;
        self.block_insert = None;
        self.pending = Pending::default();
        self.clamp(buf);
    }

    /// Put the cursor somewhere without giving up what is going on: the
    /// mode, a selection, a command half typed. For a host that has
    /// edited the text and knows where the cursor now belongs.
    pub fn place_cursor(&mut self, offset: usize, buf: &Buffer) {
        self.cursor = offset;
        self.goal_col = None;
        self.clamp(buf);
    }

    /// Select `anchor..=cursor` from outside (mouse drag).
    pub fn select(&mut self, anchor: usize, cursor: usize, linewise: bool, buf: &mut Buffer) {
        if self.mode == Mode::Insert {
            self.finish_insert(buf);
        }
        if self.mode == Mode::Command {
            self.cmdline = None;
        }
        self.mode = if linewise {
            Mode::VisualLine
        } else {
            Mode::Visual
        };
        self.anchor = anchor;
        self.cursor = cursor;
        self.goal_col = None;
        self.pending = Pending::default();
        self.clamp(buf);
    }

    /// Text typed by the user (including IME commits). In insert mode it
    /// gets the same treatment as `Key::Char`; in command mode it goes to
    /// the command line.
    pub fn type_text(&mut self, text: &str, cx: &mut Ctx<'_>) -> Vec<Effect> {
        let mut fx = Vec::new();
        for c in text.chars() {
            let key = match c {
                '\n' | '\r' => Key::Enter,
                '\t' => Key::Tab,
                c => Key::Char(c),
            };
            if matches!(self.mode, Mode::Insert | Mode::Command) {
                self.dispatch(key, cx, &mut fx);
            }
        }
        fx
    }

    /// Insert text verbatim (a paste): no auto-indent, one undo-able unit.
    pub fn paste(&mut self, text: &str, cx: &mut Ctx<'_>) {
        if matches!(self.mode, Mode::Insert | Mode::Command) {
            let mut fx = Vec::new();
            self.dispatch(Key::Text(text.replace("\r\n", "\n")), cx, &mut fx);
        }
    }

    fn clamp(&mut self, buf: &Buffer) {
        let max = buf.max_offset();
        self.cursor = self.cursor.min(max);
        self.anchor = self.anchor.min(max);
        if self.mode == Mode::Normal {
            let line = buf.line_of(self.cursor);
            let (ls, le) = (buf.line_start(line), buf.line_end(line));
            if le > ls && self.cursor >= le {
                self.cursor = le - 1;
            }
        }
    }

    // ---- key dispatch --------------------------------------------------

    pub fn handle_key(&mut self, key: Key, cx: &mut Ctx<'_>) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.message = None;
        self.dispatch(key, cx, &mut fx);
        self.run_macros(cx, &mut fx);
        fx
    }

    /// Run the keys a `@` asked for, as if they were typed: each command
    /// among them is one to undo and to repeat.
    fn run_macros(&mut self, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        let Some(keys) = self.macro_request.take() else {
            return;
        };
        let mut queue: VecDeque<Key> = keys.into();
        let was_playing = std::mem::replace(&mut self.playing, true);
        let mut run = 0;
        while let Some(key) = queue.pop_front() {
            run += 1;
            if run > MAX_MACRO_KEYS {
                self.message = Some("Macro never ends".to_string());
                break;
            }
            self.dispatch(key, cx, fx);
            // A macro that runs another: that one's keys come first.
            if let Some(more) = self.macro_request.take() {
                for key in more.into_iter().rev() {
                    queue.push_front(key);
                }
            }
        }
        self.playing = was_playing;
    }

    /// True while the keys typed so far are a binding of their own that
    /// longer bindings also start with (or, in insert mode, are held back
    /// as the possible start of one). The engine keeps no clock: if no key
    /// follows, the host calls [`Vim::timeout`] to settle it.
    pub fn wants_timeout(&self, shared: &Shared) -> bool {
        let seq = &self.pending.seq;
        !seq.is_empty()
            && (self.mode == Mode::Insert
                || shared.keymap.longest_prefix(self.map_mode(), seq).is_some())
    }

    /// Stop waiting for a longer binding: run what the keys typed so far
    /// are bound to (vim's `timeout`).
    pub fn timeout(&mut self, cx: &mut Ctx<'_>) -> Vec<Effect> {
        let mut fx = Vec::new();
        if self.wants_timeout(cx.shared) {
            self.message = None;
            self.drive(None, cx, &mut fx);
            self.run_macros(cx, &mut fx);
        }
        fx
    }

    fn dispatch(&mut self, key: Key, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        if !self.replaying {
            if !self.playing
                && let Some((_, recorded)) = &mut self.recording
            {
                if self.keys.is_empty() {
                    self.record_mark = recorded.len();
                }
                recorded.push(key.clone());
            }
            if self.keys.is_empty() {
                self.cmd_start_mode = self.mode;
                self.cmd_changed = false;
                self.no_repeat = false;
            }
            self.keys.push(key.clone());
        }
        self.drive(Some(key), cx, fx);
    }

    /// Run `key` — or with `None`, settle a sequence that was waiting for
    /// more — and then every key that puts back.
    fn drive(&mut self, key: Option<Key>, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        let version = cx.buf.version();
        let mut queue = VecDeque::new();
        match key {
            Some(key) => queue.push_back(key),
            None => {
                let seq = std::mem::take(&mut self.pending.seq);
                self.resolve_partial(seq, cx, fx);
            }
        }
        let mut steps = 0;
        loop {
            for key in self.unread.drain(..).rev() {
                queue.push_front(key);
            }
            let Some(key) = queue.pop_front() else {
                break;
            };
            steps += 1;
            if steps > MAX_MAPPED_KEYS {
                self.pending.seq.clear();
                if self.mode != Mode::Insert {
                    self.reset_pending();
                }
                self.message = Some("Key mapping never ends".to_string());
                break;
            }
            self.step(key, cx, fx);
        }
        if cx.buf.version() != version {
            self.cmd_changed = true;
        }
        self.sync(cx.buf);
        self.clamp(cx.buf);
        if !self.mode.is_visual() {
            self.expansions.clear();
        }

        let settled = self.pending.is_idle() && !matches!(self.mode, Mode::Insert | Mode::Command);
        if settled && !self.replaying {
            let keys = std::mem::take(&mut self.keys);
            if self.cmd_changed && !self.no_repeat && self.cmd_start_mode == Mode::Normal {
                cx.shared.last_change = keys;
            }
        }
    }

    fn step(&mut self, key: Key, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        match self.mode {
            Mode::Command => return self.cmdline_key(key, cx, fx),
            // Pasted text is not typing: it is never mapped.
            Mode::Insert if self.pending.seq.is_empty() && matches!(key, Key::Text(_)) => {
                return self.insert_default(key, cx);
            }
            Mode::Insert => {
                self.pending.typed.push(key.clone());
                self.map_key(key, cx, fx);
            }
            // Every key is `motion.flash`'s while it looks for text.
            Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock
                if self.pending.flash.is_some() =>
            {
                self.flash_key(key, cx, fx);
            }
            Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock => {
                // Escape always abandons a half-typed command, whatever
                // the keymap says; then it is looked up like any other key.
                if key.is_escape() && !self.pending.is_idle() {
                    self.reset_pending();
                }
                self.pending.typed.push(key.clone());
                self.normal_key(key, cx, fx);
            }
        }
        if self.pending.is_idle() {
            self.pending.typed.clear();
        }
    }

    fn reset_pending(&mut self) {
        let typed = std::mem::take(&mut self.pending.typed);
        self.pending = Pending::default();
        // A surround operator has its target, and still wants a delimiter.
        if self.surround.is_some() {
            self.pending.awaiting = Some(Await::SurroundAdd);
            self.pending.typed = typed;
        }
    }

    /// The key table in force.
    fn map_mode(&self) -> MapMode {
        match self.mode {
            Mode::Insert => MapMode::Insert,
            _ if self.pending.op.is_some() => MapMode::Operator,
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock => MapMode::Visual,
            Mode::Normal | Mode::Command => MapMode::Normal,
        }
    }

    /// The grammar around the keymap: literal arguments, counts, and the
    /// doubled operator.
    fn normal_key(&mut self, key: Key, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        if let Some(awaiting) = self.pending.awaiting.take() {
            cx.buf.begin(self.cursor);
            self.argument_key(awaiting, key, cx, fx);
            cx.buf.end();
            return;
        }

        if self.pending.seq.is_empty() {
            // Counts. A leading 0 is whatever `0` is bound to.
            if let Key::Char(c @ '0'..='9') = key {
                let slot = if self.pending.op.is_some() {
                    &mut self.pending.op_count
                } else {
                    &mut self.pending.count
                };
                if c != '0' || slot.is_some() {
                    let digit = c as usize - '0' as usize;
                    *slot = Some((slot.unwrap_or(0).saturating_mul(10) + digit).min(1_000_000));
                    return;
                }
            }

            // `dd`, `guu`, `gcc`: the operator's own key again.
            if let Some(op) = self.pending.op
                && self.pending.op_key.as_ref() == Some(&key)
            {
                cx.buf.begin(self.cursor);
                self.operator_key(op, cx);
                cx.buf.end();
                return;
            }
        }

        self.map_key(key, cx, fx);
    }

    /// Add `key` to the sequence being typed and run what that is bound to.
    fn map_key(&mut self, key: Key, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        self.pending.seq.push(key);
        match cx.shared.keymap.lookup(self.map_mode(), &self.pending.seq) {
            Lookup::Pending => {}
            Lookup::Found(action) => {
                let seq = std::mem::take(&mut self.pending.seq);
                self.exec(action, &seq[seq.len() - 1], cx, fx);
            }
            Lookup::None => {
                let seq = std::mem::take(&mut self.pending.seq);
                self.resolve_partial(seq, cx, fx);
            }
        }
    }

    /// `seq` is not a binding, or no key came to make it a longer one: run
    /// the longest leading part that is bound and put the rest back.
    fn resolve_partial(&mut self, seq: Vec<Key>, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        match cx.shared.keymap.longest_prefix(self.map_mode(), &seq) {
            Some((len, action)) => {
                self.exec(action, &seq[len - 1], cx, fx);
                self.unread.extend(seq.into_iter().skip(len));
            }
            // Unbound keys insert themselves.
            None if self.mode == Mode::Insert => {
                let mut keys = seq.into_iter();
                if let Some(first) = keys.next() {
                    self.insert_default(first, cx);
                }
                self.unread.extend(keys);
            }
            None => self.reset_pending(),
        }
    }

    /// Run what a key sequence is bound to; `trigger` is its last key.
    fn exec(&mut self, action: Action, trigger: &Key, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        match action {
            // Count, register and operator stay pending for the new keys.
            Action::Keys(keys) => self.unread.extend(keys.iter().cloned()),
            Action::Host(name) => {
                let count = self.pending.count();
                self.reset_pending();
                fx.push(Effect::Command {
                    name: name.to_string(),
                    count,
                });
            }
            Action::Builtin(builtin) if self.mode == Mode::Insert => {
                self.insert_command(builtin, cx, fx)
            }
            Action::Builtin(Builtin::Cmd(Cmd::Undo)) => self.undo(false, cx),
            Action::Builtin(Builtin::Cmd(Cmd::Redo)) => self.undo(true, cx),
            Action::Builtin(Builtin::Cmd(Cmd::Record)) => self.record_key(cx),
            Action::Builtin(Builtin::Cmd(Cmd::Surround)) => self.surround_key(trigger),
            // The count means nothing to it; an operator waits for where
            // it goes.
            Action::Builtin(Builtin::Cmd(Cmd::Flash)) => {
                self.pending.flash = Some(Flash::new(self.pending.typed.len()));
            }
            Action::Builtin(builtin) => {
                cx.buf.begin(self.cursor);
                match builtin {
                    Builtin::Motion(motion) => self.run_motion(motion, cx, fx),
                    Builtin::Operator(op) => {
                        self.operator_key(op, cx);
                        if self.pending.op.is_some() {
                            self.pending.op_key = Some(trigger.clone());
                        }
                    }
                    Builtin::Await(awaiting) => self.pending.awaiting = Some(awaiting),
                    Builtin::Scroll(scroll) => {
                        fx.push(Effect::Scroll(scroll));
                        self.reset_pending();
                    }
                    Builtin::Cmd(cmd) => self.command(cmd, cx, fx),
                    // Insert-mode commands mean nothing here.
                    Builtin::Insert(_) => self.reset_pending(),
                }
                cx.buf.end();
            }
        }
    }

    /// The key after `f`, `r`, `"`, or a text object's `i` / `a`.
    fn argument_key(&mut self, awaiting: Await, key: Key, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        match (awaiting, key) {
            (Await::Register, Key::Char(c)) => self.pending.register = Some(c),
            (Await::Find { forward, till }, Key::Char(c)) => {
                let find = Find { c, forward, till };
                cx.shared.last_find = Some(find);
                self.run_motion(Motion::Find(find), cx, fx);
            }
            (Await::Replace, Key::Char(c)) => {
                self.replace_chars(c, cx);
                self.reset_pending();
            }
            (Await::Replace, Key::Enter) => {
                self.replace_chars('\n', cx);
                self.reset_pending();
            }
            (Await::TextObject { around }, Key::Char(c)) => self.run_text_object(c, around, cx),
            (Await::SetMark, Key::Char(c)) => {
                if c.is_ascii_alphabetic() || c == '\'' || c == '`' {
                    let name = if c == '`' { '\'' } else { c };
                    self.marks.insert(name, self.cursor);
                }
                self.no_repeat = true;
                self.reset_pending();
            }
            (Await::Mark { linewise }, Key::Char(name)) => {
                self.run_motion(Motion::Mark { name, linewise }, cx, fx);
            }
            (Await::RecordRegister, Key::Char(c)) => {
                if c.is_ascii_alphanumeric() {
                    self.recording = Some((c.to_ascii_lowercase(), Vec::new()));
                    self.record_mark = 0;
                }
                self.no_repeat = true;
                self.reset_pending();
            }
            (Await::PlayRegister, Key::Char(c)) => {
                let count = self.pending.count().unwrap_or(1).max(1);
                self.reset_pending();
                self.no_repeat = true;
                self.play(c, count, cx);
            }
            (Await::SurroundAdd, key) => {
                let surround = self.surround.take();
                self.reset_pending();
                if let (Some(surround), Key::Char(c)) = (surround, key) {
                    self.surround_add(surround, c, cx);
                }
            }
            (Await::SurroundDelete, Key::Char(c)) => {
                self.reset_pending();
                self.surround_replace(c, None, cx);
            }
            (Await::SurroundChange, Key::Char(c)) => {
                self.pending.awaiting = Some(Await::SurroundChangeTo(c));
            }
            (Await::SurroundChangeTo(old), Key::Char(c)) => {
                self.reset_pending();
                self.surround_replace(old, Some(c), cx);
            }
            _ => {
                self.surround = None;
                self.reset_pending();
            }
        }
    }

    /// A key while `motion.flash` looks for text. A label goes to its
    /// place and `enter` to the first place after the cursor, as a motion:
    /// with an operator, onto it. Any other character is more of the text,
    /// backspace takes the last one back, and escape, or text that is
    /// nowhere on screen, gives up.
    fn flash_key(&mut self, key: Key, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        let Some(mut flash) = self.pending.flash.take() else {
            return;
        };
        match key {
            Key::Char(c) if flash.labelled(c).is_none() => flash.pattern.push(c),
            Key::Backspace | Key::Ctrl('h') if !flash.pattern.is_empty() => {
                flash.pattern.pop();
            }
            key => {
                let to = match key {
                    Key::Char(c) => flash.labelled(c),
                    Key::Enter | Key::Ctrl('j' | 'm') => flash.target(),
                    _ => None,
                };
                match to {
                    Some(to) => {
                        cx.buf.begin(self.cursor);
                        self.run_motion(Motion::Flash(to), cx, fx);
                        cx.buf.end();
                    }
                    None => self.reset_pending(),
                }
                return;
            }
        }
        flash.update(cx.buf, cx.viewport, self.cursor);
        if flash.matches.is_empty() && !flash.pattern.is_empty() {
            self.message = Some(format!("Pattern not found: {}", flash.pattern));
            self.reset_pending();
            return;
        }
        // The keys typed so far, as they are shown: the text as it now is.
        self.pending.typed.truncate(flash.typed);
        let pattern = flash.pattern.chars().map(Key::Char);
        self.pending.typed.extend(pattern);
        self.pending.flash = Some(flash);
    }

    // ---- macros --------------------------------------------------------

    /// `q`: stop recording, or wait for the register to record into.
    fn record_key(&mut self, cx: &mut Ctx<'_>) {
        self.reset_pending();
        self.no_repeat = true;
        match self.recording.take() {
            Some((name, mut keys)) => {
                // Without the `q` that ended it.
                keys.truncate(self.record_mark);
                let text = format_keys(&keys);
                let register = Register {
                    text: text.clone(),
                    ..Register::default()
                };
                cx.shared.registers.insert(name, register);
                cx.shared.macros.insert(name, (text, keys));
            }
            None => self.pending.awaiting = Some(Await::RecordRegister),
        }
    }

    /// `@a`: have the keys in a register run, `count` times over. `@@`
    /// runs the last one again.
    fn play(&mut self, name: char, count: usize, cx: &mut Ctx<'_>) {
        let name = match name {
            '@' => cx.shared.last_macro,
            name => Some(name.to_ascii_lowercase()),
        };
        let Some(name) = name else {
            return;
        };
        let Some(register) = cx.shared.registers.get(&name) else {
            self.message = Some(format!("Nothing in register {name}"));
            return;
        };
        let keys = match cx.shared.macros.get(&name) {
            Some((text, keys)) if *text == register.text => keys.clone(),
            _ => parse_keys(&register.text),
        };
        cx.shared.last_macro = Some(name);
        let mut all = Vec::with_capacity(keys.len() * count.min(1000));
        for _ in 0..count.min(1000) {
            all.extend(keys.iter().cloned());
        }
        self.macro_request = Some(all);
    }

    // ---- surround ------------------------------------------------------

    /// `s` after an operator: `ys` makes the operator the one that
    /// surrounds, `ds` and `cs` ask which delimiter is meant.
    fn surround_key(&mut self, trigger: &Key) {
        match self.pending.op {
            Some(Operator::Yank) => {
                self.pending.op = Some(Operator::Surround);
                // `yss`: the `s` again is the whole line.
                self.pending.op_key = Some(trigger.clone());
            }
            Some(Operator::Delete) => {
                self.reset_pending();
                self.pending.awaiting = Some(Await::SurroundDelete);
            }
            Some(Operator::Change) => {
                self.reset_pending();
                self.pending.awaiting = Some(Await::SurroundChange);
            }
            _ => self.reset_pending(),
        }
    }

    /// Put the delimiter `c` stands for around what the operator was given.
    fn surround_add(&mut self, surround: Surround, c: char, cx: &mut Ctx<'_>) {
        let (open, close) = surround_pair(c);
        let buf = &mut *cx.buf;
        let Some(&(first, _)) = surround.ranges.first() else {
            return;
        };
        if surround.own_lines {
            let (start, end) = surround.ranges[0];
            let indent = leading_ws(buf, buf.line_of(start));
            buf.insert(end, &format!("\n{indent}{}", close.trim()));
            buf.insert(start, &format!("{indent}{}\n", open.trim()));
            self.cursor = start + indent.chars().count();
        } else {
            for &(start, end) in surround.ranges.iter().rev() {
                buf.insert(end, &close);
                buf.insert(start, &open);
            }
            self.cursor = first;
        }
        self.goal_col = None;
    }

    /// Take the delimiters `old` stands for away from around the cursor,
    /// or put `new` in their place.
    fn surround_replace(&mut self, old: char, new: Option<char>, cx: &mut Ctx<'_>) {
        let buf = &mut *cx.buf;
        let Some((open, close)) = textobj::delimiters(buf, self.cursor, old) else {
            return;
        };
        let (new_open, new_close) = new.map(surround_pair).unwrap_or_default();
        // The opening bracket names the pair with the blanks inside it.
        let spaced = matches!(old, '(' | '[' | '{' | '<');
        let inner_end = if spaced && close > open + 1 && buf.char_at(close - 1) == ' ' {
            close - 1
        } else {
            close
        };
        let inner_start = if spaced && open + 1 < inner_end && buf.char_at(open + 1) == ' ' {
            open + 2
        } else {
            open + 1
        };
        buf.edit(inner_end..close + 1, &new_close);
        buf.edit(open..inner_start, &new_open);
        self.cursor = open;
        self.goal_col = None;
    }

    /// `u` / `ctrl-r`. Never inside an undo group, so `exec` keeps these
    /// out of the one it opens for everything else.
    fn undo(&mut self, redo: bool, cx: &mut Ctx<'_>) {
        let count = self.pending.count().unwrap_or(1);
        self.reset_pending();
        self.no_repeat = true;
        for _ in 0..count {
            let pos = if redo { cx.buf.redo() } else { cx.buf.undo() };
            match pos {
                Some(pos) => self.cursor = pos,
                None => break,
            }
        }
        self.goal_col = None;
    }

    // ---- motions -------------------------------------------------------

    fn vertical(&mut self, buf: &Buffer, row: usize) -> usize {
        let point = buf.offset_to_point(self.cursor);
        let goal = self.goal_col.unwrap_or(point.col);
        self.goal_col = Some(goal);
        buf.line_start(row) + goal.min(buf.line_len(row))
    }

    fn compute_motion(
        &mut self,
        motion: Motion,
        count: Option<usize>,
        op_pending: bool,
        cx: &mut Ctx<'_>,
    ) -> Option<MotionOut> {
        let buf = &*cx.buf;
        let n = count.unwrap_or(1).max(1);
        let cur = self.cursor;
        let last = buf.last_line();
        let line = buf.line_of(cur);
        let (ls, le) = (buf.line_start(line), buf.line_end(line));
        let out = |pos: usize, inclusive: bool, linewise: bool| {
            Some(MotionOut {
                pos,
                inclusive,
                linewise,
                keep_goal: false,
                scroll: None,
            })
        };
        let fnb = |row: usize| first_non_blank(buf, row);

        match motion {
            Motion::Left => {
                let pos = cur.saturating_sub(n).max(ls);
                if pos == cur {
                    None
                } else {
                    out(pos, false, false)
                }
            }
            Motion::Right => {
                let limit = if op_pending {
                    le
                } else {
                    le.saturating_sub(1).max(ls)
                };
                let pos = (cur + n).min(limit);
                if pos == cur {
                    None
                } else {
                    out(pos, false, false)
                }
            }
            Motion::Up | Motion::Down => {
                let row = if motion == Motion::Down {
                    (line + n).min(last)
                } else {
                    line.saturating_sub(n)
                };
                if row == line {
                    return None;
                }
                let pos = self.vertical(buf, row);
                Some(MotionOut {
                    pos,
                    inclusive: false,
                    linewise: true,
                    keep_goal: true,
                    scroll: None,
                })
            }
            Motion::WordFwd(big) => {
                let pos = (0..n).fold(cur, |p, _| motion::next_word_start(buf, p, big));
                out(pos, false, false)
            }
            Motion::WordBack(big) => {
                let pos = (0..n).fold(cur, |p, _| motion::prev_word_start(buf, p, big));
                out(pos, false, false)
            }
            Motion::WordEnd(big) => {
                let pos = (0..n).fold(cur, |p, _| motion::word_end(buf, p, big));
                out(pos, true, false)
            }
            Motion::WordEndBack(big) => {
                let pos = (0..n).fold(cur, |p, _| motion::prev_word_end(buf, p, big));
                out(pos, true, false)
            }
            Motion::LineStart => out(ls, false, false),
            Motion::FirstNonBlank => out(fnb(line), false, false),
            Motion::LineEnd => {
                let row = (line + n - 1).min(last);
                let (s, e) = (buf.line_start(row), buf.line_end(row));
                self.goal_col = Some(usize::MAX);
                let (pos, inclusive) = if e > s { (e - 1, true) } else { (e, false) };
                Some(MotionOut {
                    pos,
                    inclusive,
                    linewise: false,
                    keep_goal: true,
                    scroll: None,
                })
            }
            Motion::Line { default_last } => {
                let row = match count {
                    Some(c) => c.saturating_sub(1).min(last),
                    None if default_last => last,
                    None => 0,
                };
                out(fnb(row), false, true)
            }
            Motion::Find(find) => {
                let pos = motion::find_char(buf, cur, find, n, false)?;
                out(pos, find.forward, false)
            }
            Motion::RepeatFind { reverse } => {
                let mut find = cx.shared.last_find?;
                if reverse {
                    find.forward = !find.forward;
                }
                let pos = motion::find_char(buf, cur, find, n, true)?;
                out(pos, find.forward, false)
            }
            Motion::MatchPair => out(motion::match_pair(buf, cur)?, true, false),
            Motion::ParaFwd => {
                let pos = (0..n).fold(cur, |p, _| motion::paragraph_forward(buf, p));
                out(pos, false, false)
            }
            Motion::ParaBack => {
                let pos = (0..n).fold(cur, |p, _| motion::paragraph_backward(buf, p));
                out(pos, false, false)
            }
            Motion::ScreenTop | Motion::ScreenMid | Motion::ScreenBot => {
                let top = cx.viewport.top.min(last);
                let bottom = (top + cx.viewport.height.saturating_sub(1)).min(last);
                let row = match motion {
                    Motion::ScreenTop => (top + n - 1).min(bottom),
                    Motion::ScreenBot => bottom.saturating_sub(n - 1).max(top),
                    _ => top + (bottom - top) / 2,
                };
                out(fnb(row), false, true)
            }
            Motion::Page { down, half } => {
                let height = cx.viewport.height.max(1);
                let delta = if half {
                    count.unwrap_or((height / 2).max(1))
                } else {
                    height.saturating_sub(2).max(1).saturating_mul(n)
                };
                let row = if down {
                    (line + delta).min(last)
                } else {
                    line.saturating_sub(delta)
                };
                if row == line {
                    return None;
                }
                let pos = self.vertical(buf, row);
                let moved = row as isize - line as isize;
                Some(MotionOut {
                    pos,
                    inclusive: false,
                    linewise: true,
                    keep_goal: true,
                    scroll: Some(moved),
                })
            }
            Motion::SearchNext { reverse } => {
                let search = cx.shared.search.as_ref()?;
                let forward = search.forward != reverse;
                match find_match(buf, &search.regex, cur, forward, n) {
                    Some(pos) => out(pos, false, false),
                    None => {
                        self.message = Some(format!("Pattern not found: {}", search.pattern));
                        None
                    }
                }
            }
            Motion::WordSearch { forward } => {
                let (s, e) = motion::word_under_cursor(buf, cur)?;
                let word = buf.slice_string(s..e);
                let pattern = format!("\\b{}\\b", regex::escape(&word));
                let regex = regex::RegexBuilder::new(&pattern).build().ok()?;
                let pos = find_match(buf, &regex, s, forward, n);
                cx.shared.search = Some(Search {
                    pattern,
                    regex,
                    forward,
                });
                out(pos?, false, false)
            }
            Motion::NextLine => {
                if line + n > last {
                    return None;
                }
                out(fnb(line + n), false, true)
            }
            Motion::PrevLine => {
                if n > line {
                    return None;
                }
                out(fnb(line - n), false, true)
            }
            Motion::CurLine => out(fnb((line + n - 1).min(last)), false, true),
            Motion::Column => out(ls + (n - 1).min(le - ls), false, false),
            Motion::Mark { name, linewise } => {
                let Some(pos) = self.mark(name) else {
                    self.message = Some(format!("Mark {name} is not set"));
                    return None;
                };
                let pos = pos.min(buf.max_offset());
                if linewise {
                    out(fnb(buf.line_of(pos)), false, true)
                } else {
                    out(pos, false, false)
                }
            }
            Motion::Syntax { kind, forward } => {
                let syntax = cx.syntax.as_deref_mut()?;
                let mut pos = cur;
                for _ in 0..n {
                    pos = syntax.neighbor(buf, pos, kind, forward)?;
                }
                out(pos, false, false)
            }
            // An operator takes the first character of what was found
            // along with what lies before it.
            Motion::Flash(pos) => out(pos.min(buf.max_offset()), true, false),
        }
    }

    /// The cursor is about to make a long jump: tell the host, and leave
    /// the mark that `''` goes back to.
    fn jump_from(&mut self, fx: &mut Vec<Effect>) {
        self.marks.insert('\'', self.cursor);
        fx.push(Effect::Jump { from: self.cursor });
    }

    fn run_motion(&mut self, motion: Motion, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        let count = self.pending.count();
        let op = self.pending.op;

        // `cw` on a word changes to the end of that word, not up to the next.
        let out = match (op, motion) {
            (Some(Operator::Change), Motion::WordFwd(big))
                if class(cx.buf.char_at(self.cursor), big) != WS =>
            {
                let buf = &*cx.buf;
                let cls = class(buf.char_at(self.cursor), big);
                let mut pos = self.cursor;
                while class(buf.char_at(pos + 1), big) == cls {
                    pos += 1;
                }
                for _ in 1..count.unwrap_or(1) {
                    pos = motion::word_end(buf, pos, big);
                }
                Some(MotionOut {
                    pos,
                    inclusive: true,
                    linewise: false,
                    keep_goal: false,
                    scroll: None,
                })
            }
            _ => self.compute_motion(motion, count, op.is_some(), cx),
        };
        let Some(out) = out else {
            self.reset_pending();
            return;
        };
        if matches!(
            motion,
            Motion::SearchNext { .. } | Motion::WordSearch { .. }
        ) {
            cx.shared.hlsearch = true;
        }

        match op {
            Some(op) => {
                let target = self.motion_target(motion, &out, cx.buf);
                self.apply_operator(op, target, cx);
            }
            None => {
                if motion.is_jump() {
                    self.jump_from(fx);
                }
                self.cursor = out.pos;
                if !out.keep_goal {
                    self.goal_col = None;
                }
                if let Some(lines) = out.scroll {
                    fx.push(Effect::Scroll(Scroll::Lines(lines)));
                }
            }
        }
        self.reset_pending();
    }

    fn motion_target(&self, motion: Motion, out: &MotionOut, buf: &Buffer) -> Target {
        let cur = self.cursor;
        if out.linewise {
            let (a, b) = minmax(buf.line_of(cur), buf.line_of(out.pos));
            return Target::Lines(a, b);
        }
        let (s, mut e) = minmax(cur, out.pos);
        if out.inclusive {
            e += 1;
        }
        e = e.min(buf.max_offset());

        if matches!(motion, Motion::WordFwd(_)) && e > s {
            // Don't swallow the line break after the last word on a line.
            let mut k = e;
            while k > s && is_blank(buf.char_at(k - 1)) {
                k -= 1;
            }
            if k > s && buf.char_at(k - 1) == '\n' {
                e = if k - 1 > s { k - 1 } else { s + 1 };
            }
        } else if !out.inclusive && e > s {
            // An exclusive motion that ends at column 0 stops at the end of
            // the previous line, and is linewise if it started in the indent.
            let end_line = buf.line_of(e);
            if end_line > buf.line_of(s) && e == buf.line_start(end_line) {
                let start_line = buf.line_of(s);
                if s <= first_non_blank(buf, start_line)
                    && matches!(motion, Motion::ParaFwd | Motion::ParaBack)
                {
                    return Target::Lines(start_line, end_line - 1);
                }
                e = buf.line_end(end_line - 1);
            }
        }
        Target::Chars(s, e)
    }

    // ---- operators -----------------------------------------------------

    /// An operator key was pressed: apply it to the visual selection, to
    /// `count` lines if doubled (`dd`), or wait for a motion.
    fn operator_key(&mut self, op: Operator, cx: &mut Ctx<'_>) {
        if self.mode == Mode::VisualBlock {
            self.apply_to_block(op, cx);
            self.reset_pending();
        } else if self.mode.is_visual() {
            let target = self.visual_target(cx.buf);
            self.apply_operator(op, target, cx);
            self.reset_pending();
        } else if self.pending.op == Some(op) {
            let n = self.pending.count().unwrap_or(1).max(1);
            let line = cx.buf.line_of(self.cursor);
            let last = (line + n - 1).min(cx.buf.last_line());
            self.apply_operator(op, Target::Lines(line, last), cx);
            self.reset_pending();
        } else if self.pending.op.is_some() {
            self.reset_pending();
        } else {
            self.pending.op = Some(op);
        }
    }

    fn visual_target(&self, buf: &Buffer) -> Target {
        let (s, e) = minmax(self.anchor, self.cursor);
        match self.mode {
            Mode::VisualLine => Target::Lines(buf.line_of(s), buf.line_of(e)),
            _ => Target::Chars(s, (e + 1).min(buf.max_offset()).max(s)),
        }
    }

    fn store(&mut self, text: String, linewise: bool, yank: bool, cx: &mut Ctx<'_>) {
        let name = self.pending.register.take();
        let register = Register {
            text,
            linewise,
            blockwise: false,
        };
        cx.shared.write_register(name, register, yank, cx.clipboard);
    }

    /// An operator on a block: on the part of each line that is in it.
    fn apply_to_block(&mut self, op: Operator, cx: &mut Ctx<'_>) {
        self.last_visual = Some((self.anchor, self.cursor, self.mode));
        let (rows, cols) = self.block(cx.buf);
        let ranges = self.block_ranges(cx.buf);
        let first = ranges[0].0;
        self.mode = Mode::Normal;
        self.goal_col = None;
        let text = |buf: &Buffer| {
            let lines: Vec<String> = ranges
                .iter()
                .map(|&(s, e)| buf.slice_string(s..e))
                .collect();
            lines.join("\n")
        };
        let store = |this: &mut Self, yank: bool, cx: &mut Ctx<'_>| {
            let register = Register {
                text: text(cx.buf),
                linewise: false,
                blockwise: true,
            };
            let name = this.pending.register.take();
            cx.shared.write_register(name, register, yank, cx.clipboard);
        };
        match op {
            Operator::Yank => {
                store(self, true, cx);
                self.cursor = first;
            }
            Operator::Delete | Operator::Change => {
                store(self, false, cx);
                for &(s, e) in ranges.iter().rev() {
                    cx.buf.delete(s..e);
                }
                self.cursor = first;
                if op == Operator::Change {
                    self.enter_insert(cx.buf);
                    self.block_insert = Some(BlockInsert {
                        start: first,
                        rows: (rows.0 + 1, rows.1),
                        col: cols.0,
                        pad: false,
                    });
                }
            }
            Operator::Lower | Operator::Upper | Operator::ToggleCase => {
                for &(s, e) in &ranges {
                    change_case(cx.buf, op, s, e);
                }
                self.cursor = first;
            }
            Operator::Indent | Operator::Outdent | Operator::Comment => {
                self.apply_to_lines(op, rows.0, rows.1, cx);
            }
            Operator::Surround => {
                self.surround = Some(Surround {
                    ranges: ranges.into_iter().filter(|(s, e)| e > s).collect(),
                    own_lines: false,
                });
                self.cursor = first;
            }
        }
    }

    fn apply_operator(&mut self, op: Operator, target: Target, cx: &mut Ctx<'_>) {
        if self.mode.is_visual() {
            self.last_visual = Some((self.anchor, self.cursor, self.mode));
        }
        if op == Operator::Surround {
            let buf = &*cx.buf;
            let own_lines = self.mode == Mode::VisualLine;
            let range = match target {
                Target::Chars(s, e) => (s, e),
                Target::Lines(a, b) if own_lines => (buf.line_start(a), buf.line_end(b)),
                // `yss`: what is on the line, not its indentation.
                Target::Lines(a, b) => (first_non_blank(buf, a), buf.line_end(b)),
            };
            self.surround = Some(Surround {
                ranges: vec![range],
                own_lines,
            });
            self.mode = Mode::Normal;
            self.cursor = range.0;
            self.goal_col = None;
            return;
        }
        match target {
            Target::Lines(a, b) => self.apply_to_lines(op, a, b, cx),
            Target::Chars(s, e) => self.apply_to_chars(op, s, e, cx),
        }
        if self.mode.is_visual() {
            self.mode = Mode::Normal;
        }
        self.goal_col = None;
    }

    fn apply_to_lines(&mut self, op: Operator, a: usize, b: usize, cx: &mut Ctx<'_>) {
        let col = cx.buf.offset_to_point(self.cursor).col;
        let text = |buf: &Buffer| buf.slice_string(buf.line_start(a)..buf.line_end(b) + 1);
        match op {
            Operator::Yank => {
                let text = text(cx.buf);
                self.store(text, true, true, cx);
                let buf = &*cx.buf;
                self.cursor = buf.line_start(a) + col.min(buf.line_len(a));
            }
            Operator::Delete => {
                let text = text(cx.buf);
                self.store(text, true, false, cx);
                let buf = &mut *cx.buf;
                let range = if b == buf.last_line() {
                    if a == 0 {
                        0..buf.line_end(b)
                    } else {
                        buf.line_end(a - 1)..buf.line_end(b)
                    }
                } else {
                    buf.line_start(a)..buf.line_start(b + 1)
                };
                buf.delete(range);
                self.cursor = first_non_blank(buf, a.min(buf.last_line()));
            }
            Operator::Change => {
                let text = text(cx.buf);
                self.store(text, true, false, cx);
                self.enter_insert(cx.buf);
                let buf = &mut *cx.buf;
                let start = first_non_blank(buf, a);
                buf.delete(start..buf.line_end(b));
                self.cursor = start;
            }
            Operator::Indent | Operator::Outdent => {
                let buf = &mut *cx.buf;
                let unit = cx.lang.indent_unit();
                for line in a..=b {
                    let ls = buf.line_start(line);
                    if op == Operator::Indent {
                        if buf.line_len(line) > 0 {
                            buf.insert(ls, &unit);
                        }
                    } else {
                        let mut cols = 0;
                        let mut end = ls;
                        while cols < cx.lang.tab_width.max(1) {
                            match buf.char_at(end) {
                                ' ' => {
                                    cols += 1;
                                    end += 1;
                                }
                                '\t' => {
                                    end += 1;
                                    break;
                                }
                                _ => break,
                            }
                        }
                        buf.delete(ls..end);
                    }
                }
                self.cursor = first_non_blank(buf, a);
            }
            Operator::Lower | Operator::Upper | Operator::ToggleCase => {
                let (s, e) = (cx.buf.line_start(a), cx.buf.line_end(b));
                change_case(cx.buf, op, s, e);
                let buf = &*cx.buf;
                self.cursor = buf.line_start(a) + col.min(buf.line_len(a));
            }
            Operator::Comment => {
                toggle_comment(cx.buf, cx.lang, a, b);
                self.cursor = first_non_blank(cx.buf, a);
            }
            // `apply_operator` keeps it for itself.
            Operator::Surround => {}
        }
    }

    fn apply_to_chars(&mut self, op: Operator, s: usize, e: usize, cx: &mut Ctx<'_>) {
        // Nothing to act on (e.g. `di(` on `()`); only `c` still does
        // something, by entering insert mode there.
        if s == e && op != Operator::Change {
            return;
        }
        match op {
            Operator::Yank => {
                let text = cx.buf.slice_string(s..e);
                self.store(text, false, true, cx);
                self.cursor = s;
            }
            Operator::Delete => {
                let text = cx.buf.slice_string(s..e);
                if !text.is_empty() {
                    self.store(text, false, false, cx);
                }
                cx.buf.delete(s..e);
                self.cursor = s;
            }
            Operator::Change => {
                let text = cx.buf.slice_string(s..e);
                if !text.is_empty() {
                    self.store(text, false, false, cx);
                }
                self.enter_insert(cx.buf);
                cx.buf.delete(s..e);
                self.cursor = s;
            }
            Operator::Lower | Operator::Upper | Operator::ToggleCase => {
                change_case(cx.buf, op, s, e);
                self.cursor = s;
            }
            Operator::Indent | Operator::Outdent | Operator::Comment => {
                let (a, b) = (
                    cx.buf.line_of(s),
                    cx.buf.line_of(e.saturating_sub(1).max(s)),
                );
                self.apply_to_lines(op, a, b, cx);
            }
            Operator::Surround => {}
        }
    }

    fn run_text_object(&mut self, kind: char, around: bool, cx: &mut Ctx<'_>) {
        let obj = match textobj::text_object(cx.buf, self.cursor, kind, around) {
            Some(obj) => Some(obj),
            // Not one the text alone can tell: ask the syntax tree.
            None => self.syntax_object(kind, around, cx),
        };
        let Some(obj) = obj else {
            self.reset_pending();
            return;
        };
        if let Some(op) = self.pending.op {
            let target = if obj.linewise {
                Target::Lines(cx.buf.line_of(obj.start), cx.buf.line_of(obj.end - 1))
            } else {
                Target::Chars(obj.start, obj.end.min(cx.buf.max_offset()))
            };
            self.apply_operator(op, target, cx);
        } else if self.mode.is_visual() {
            if self.mode == Mode::VisualBlock {
                self.mode = Mode::Visual;
            }
            self.anchor = obj.start;
            if obj.linewise && kind == 'p' {
                self.mode = Mode::VisualLine;
                self.cursor = obj.end.saturating_sub(1).max(obj.start);
            } else if obj.linewise {
                // Stop before the last line's newline.
                self.cursor = obj.end.saturating_sub(2).max(obj.start);
            } else {
                self.cursor = obj.end.saturating_sub(1).max(obj.start);
            }
        }
        self.reset_pending();
    }

    /// A text object the syntax tree knows: a function, a type, an
    /// argument, a comment.
    fn syntax_object(&mut self, kind: char, around: bool, cx: &mut Ctx<'_>) -> Option<ObjRange> {
        let buf = &*cx.buf;
        let (start, end) = cx
            .syntax
            .as_deref_mut()?
            .object(buf, self.cursor, kind, around)?;
        if end <= start {
            return None;
        }
        // A whole function or type that has its lines to itself goes with
        // them, so that `daf` leaves no empty line behind.
        let (first, last) = (buf.line_of(start), buf.line_of(end - 1));
        let whole_lines = around
            && matches!(kind, 'f' | 't')
            && start == first_non_blank(buf, first)
            && end == buf.line_end(last);
        Some(if whole_lines {
            ObjRange {
                start: buf.line_start(first),
                end: buf.line_end(last) + 1,
                linewise: true,
            }
        } else {
            ObjRange {
                start,
                end,
                linewise: false,
            }
        })
    }

    /// `]x`: select the syntax node around the cursor, or the next one out
    /// from what is selected.
    fn expand_selection(&mut self, cx: &mut Ctx<'_>) {
        let buf = &*cx.buf;
        let (start, end) = match self.selection(buf) {
            Some((start, end)) => (start, end.min(buf.max_offset())),
            None => (self.cursor, self.cursor + 1),
        };
        let Some(syntax) = cx.syntax.as_deref_mut() else {
            self.message = Some("No syntax tree for this file".to_string());
            return;
        };
        let Some((start, end)) = syntax.enclosing(buf, start, end) else {
            return;
        };
        // Steps back only lead from here if this is where they led to.
        let here = (self.anchor, self.cursor);
        if self.expansions.last().is_some_and(|last| last.to != here) || !self.mode.is_visual() {
            self.expansions.clear();
        }
        let to = (start, end.saturating_sub(1).max(start));
        self.expansions.push(Expansion {
            from: (self.anchor, self.cursor, self.mode),
            to,
        });
        (self.anchor, self.cursor) = to;
        self.mode = Mode::Visual;
        self.goal_col = None;
    }

    /// `[x`: back to what was selected before the last `]x`.
    fn shrink_selection(&mut self) {
        let here = (self.anchor, self.cursor);
        match self.expansions.pop() {
            Some(last) if last.to == here && self.mode.is_visual() => {
                (self.anchor, self.cursor, self.mode) = last.from;
                self.goal_col = None;
            }
            _ => self.expansions.clear(),
        }
    }

    /// Enter the visual mode `target`, switch to it from another, or leave
    /// it if that is where we are.
    fn toggle_visual(&mut self, target: Mode) {
        if self.mode == target {
            self.exit_visual();
        } else {
            if !self.mode.is_visual() {
                self.anchor = self.cursor;
            }
            self.mode = target;
        }
    }

    /// `I` / `A` in visual mode: insert before the selection or after it.
    /// On a block, what is typed goes on every line of it.
    fn visual_insert(&mut self, append: bool, cx: &mut Ctx<'_>) {
        let buf = &mut *cx.buf;
        if self.mode != Mode::VisualBlock {
            let (s, e) = match self.selection(buf) {
                Some((s, e)) => (s, e.min(buf.max_offset())),
                None => return,
            };
            let linewise = self.mode == Mode::VisualLine;
            self.enter_insert(buf);
            self.cursor = match (append, linewise) {
                (false, true) => first_non_blank(buf, buf.line_of(s)),
                (false, false) => s,
                (true, true) => e.saturating_sub(1).max(s),
                (true, false) => e,
            };
            return;
        }
        let (rows, cols) = self.block(buf);
        let col = if append { cols.1 + 1 } else { cols.0 };
        self.enter_insert(buf);
        let (ls, len) = (buf.line_start(rows.0), buf.line_len(rows.0));
        if append && len < col {
            buf.insert(ls + len, &" ".repeat(col - len));
        }
        let start = ls + col.min(buf.line_len(rows.0));
        self.cursor = start;
        self.block_insert = Some(BlockInsert {
            start,
            rows: (rows.0 + 1, rows.1),
            col,
            pad: append,
        });
    }

    // ---- simple commands -----------------------------------------------

    fn command(&mut self, cmd: Cmd, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        let line = cx.buf.line_of(self.cursor);
        let visual = self.mode.is_visual();
        match cmd {
            Cmd::Escape => self.exit_visual(),
            Cmd::Insert => self.enter_insert(cx.buf),
            Cmd::Append => {
                self.enter_insert(cx.buf);
                if cx.buf.line_len(line) > 0 {
                    self.cursor += 1;
                }
            }
            Cmd::InsertLineStart => {
                self.enter_insert(cx.buf);
                self.cursor = first_non_blank(cx.buf, line);
            }
            Cmd::AppendLineEnd => {
                self.enter_insert(cx.buf);
                self.cursor = cx.buf.line_end(line);
            }
            Cmd::OpenBelow => {
                self.enter_insert(cx.buf);
                let buf = &mut *cx.buf;
                let mut indent = leading_ws(buf, line);
                if opens_block(buf, buf.line_start(line), buf.line_end(line)) {
                    indent.push_str(&cx.lang.indent_unit());
                }
                let at = buf.line_end(line);
                buf.insert(at, &format!("\n{indent}"));
                self.cursor = at + 1 + indent.chars().count();
            }
            Cmd::OpenAbove => {
                self.enter_insert(cx.buf);
                let buf = &mut *cx.buf;
                let indent = leading_ws(buf, line);
                let at = buf.line_start(line);
                buf.insert(at, &format!("{indent}\n"));
                self.cursor = at + indent.chars().count();
            }
            Cmd::DeleteChar { before } => self.delete_chars(before, cx),
            Cmd::SubstituteChar => {
                let n = self.pending.count().unwrap_or(1);
                let end = (self.cursor + n).min(cx.buf.line_end(line));
                self.apply_operator(Operator::Change, Target::Chars(self.cursor, end), cx);
            }
            Cmd::Lines(op) if visual => {
                self.mode = Mode::VisualLine;
                self.operator_key(op, cx);
            }
            Cmd::Lines(op) => self.repeat_op_on_lines(op, cx),
            Cmd::ToLineEnd(op) => {
                let n = self.pending.count().unwrap_or(1).max(1);
                let last = (line + n - 1).min(cx.buf.last_line());
                let end = cx.buf.line_end(last);
                self.apply_operator(op, Target::Chars(self.cursor, end), cx);
            }
            Cmd::Join { space } => self.join_lines(space, cx),
            Cmd::ToggleCaseChar => {
                let n = self.pending.count().unwrap_or(1).max(1);
                let le = cx.buf.line_end(line);
                let end = (self.cursor + n).min(le);
                change_case(cx.buf, Operator::ToggleCase, self.cursor, end);
                self.cursor = end;
            }
            Cmd::Put { .. } if visual => self.visual_put(cx),
            Cmd::Put { before } => self.put(before, cx),
            Cmd::Repeat => {
                let keys = cx.shared.last_change.clone();
                if !keys.is_empty() {
                    self.reset_pending();
                    let was_replaying = std::mem::replace(&mut self.replaying, true);
                    for key in keys {
                        self.dispatch(key, cx, fx);
                    }
                    self.replaying = was_replaying;
                    self.no_repeat = true;
                }
            }
            // `exec` runs these itself, outside its undo group.
            Cmd::Undo | Cmd::Redo => {}
            Cmd::Visual { linewise: true } => self.toggle_visual(Mode::VisualLine),
            Cmd::Visual { linewise: false } => self.toggle_visual(Mode::Visual),
            Cmd::VisualBlock => self.toggle_visual(Mode::VisualBlock),
            Cmd::BlockInsert { append } => {
                if visual {
                    self.visual_insert(append, cx);
                }
            }
            Cmd::Expand => self.expand_selection(cx),
            Cmd::Shrink => self.shrink_selection(),
            // `exec` runs these itself: they keep something pending.
            Cmd::Record | Cmd::Surround | Cmd::Flash => {}
            Cmd::SwapEnds => {
                if visual {
                    std::mem::swap(&mut self.anchor, &mut self.cursor);
                }
            }
            Cmd::Reselect => {
                if let Some((anchor, cursor, mode)) = self.last_visual {
                    self.anchor = anchor;
                    self.cursor = cursor;
                    self.mode = mode;
                }
            }
            Cmd::Prompt(':') if visual => {
                self.last_visual = Some((self.anchor, self.cursor, self.mode));
                self.open_cmdline(':', "'<,'>".to_string());
            }
            Cmd::Prompt(kind) => {
                self.exit_visual();
                self.open_cmdline(kind, String::new());
            }
        }
        self.reset_pending();
    }

    fn exit_visual(&mut self) {
        if self.mode.is_visual() {
            self.last_visual = Some((self.anchor, self.cursor, self.mode));
            self.mode = Mode::Normal;
        }
    }

    fn repeat_op_on_lines(&mut self, op: Operator, cx: &mut Ctx<'_>) {
        let n = self.pending.count().unwrap_or(1).max(1);
        let line = cx.buf.line_of(self.cursor);
        let last = (line + n - 1).min(cx.buf.last_line());
        self.apply_operator(op, Target::Lines(line, last), cx);
    }

    /// `x` / `X`
    fn delete_chars(&mut self, before: bool, cx: &mut Ctx<'_>) {
        let n = self.pending.count().unwrap_or(1).max(1);
        let line = cx.buf.line_of(self.cursor);
        let (ls, le) = (cx.buf.line_start(line), cx.buf.line_end(line));
        let (s, e) = if before {
            (self.cursor.saturating_sub(n).max(ls), self.cursor)
        } else {
            (self.cursor, (self.cursor + n).min(le))
        };
        if e > s {
            self.apply_operator(Operator::Delete, Target::Chars(s, e), cx);
        }
    }

    /// `r{char}`
    fn replace_chars(&mut self, c: char, cx: &mut Ctx<'_>) {
        let buf = &mut *cx.buf;
        if self.mode.is_visual() {
            let ranges = match self.mode {
                Mode::VisualBlock => self.block_ranges(buf),
                _ => vec![match self.visual_target(buf) {
                    Target::Chars(s, e) => (s, e),
                    Target::Lines(a, b) => (buf.line_start(a), buf.line_end(b)),
                }],
            };
            for &(s, e) in &ranges {
                let replaced: String = buf
                    .slice_string(s..e)
                    .chars()
                    .map(|ch| if ch == '\n' { '\n' } else { c })
                    .collect();
                buf.edit(s..e, &replaced);
            }
            self.exit_visual();
            self.cursor = ranges[0].0;
            return;
        }
        let n = self.pending.count().unwrap_or(1).max(1);
        let le = buf.line_end(buf.line_of(self.cursor));
        if self.cursor + n > le {
            return;
        }
        if c == '\n' {
            buf.edit(self.cursor..self.cursor + n, "\n");
            self.cursor += 1;
        } else {
            buf.edit(self.cursor..self.cursor + n, &c.to_string().repeat(n));
            self.cursor += n - 1;
        }
    }

    /// `J` / `gJ`
    fn join_lines(&mut self, with_space: bool, cx: &mut Ctx<'_>) {
        let buf = &mut *cx.buf;
        let (line, joins) = if self.mode.is_visual() {
            let (s, e) = minmax(self.anchor, self.cursor);
            let (a, b) = (buf.line_of(s), buf.line_of(e));
            (a, (b - a).max(1))
        } else {
            let n = self.pending.count().unwrap_or(2).max(2);
            (buf.line_of(self.cursor), n - 1)
        };
        for _ in 0..joins {
            if line >= buf.last_line() {
                break;
            }
            let (ls, le) = (buf.line_start(line), buf.line_end(line));
            if with_space {
                let next = first_non_blank(buf, line + 1);
                let next_empty = next == buf.line_end(line + 1);
                let ends_blank = le == ls || is_blank(buf.char_at(le - 1));
                let sep = if next_empty || ends_blank || buf.char_at(next) == ')' {
                    ""
                } else {
                    " "
                };
                buf.edit(le..next, sep);
            } else {
                buf.delete(le..le + 1);
            }
            self.cursor = le;
        }
        if self.mode.is_visual() {
            self.exit_visual();
        }
    }

    /// `p` / `P`
    fn put(&mut self, before: bool, cx: &mut Ctx<'_>) {
        let count = self.pending.count().unwrap_or(1).max(1);
        let name = self.pending.register.take();
        let Some(reg) = cx.shared.read_register(name, cx.clipboard) else {
            return;
        };
        if reg.text.is_empty() {
            return;
        }
        let buf = &mut *cx.buf;
        let line = buf.line_of(self.cursor);
        if reg.blockwise {
            // A rectangle goes back as one: a line of it to a line, from
            // the cursor's column down.
            let ls = buf.line_start(line);
            let after = !before && buf.line_len(line) > 0;
            let col = self.cursor - ls + usize::from(after);
            for (index, piece) in reg.text.split('\n').enumerate() {
                let row = line + index;
                while row > buf.last_line() {
                    buf.insert(buf.len_chars(), "\n");
                }
                let (ls, len) = (buf.line_start(row), buf.line_len(row));
                if len < col {
                    buf.insert(ls + len, &" ".repeat(col - len));
                }
                buf.insert(ls + col, &piece.repeat(count));
            }
            self.cursor = buf.line_start(line) + col;
        } else if reg.linewise {
            let mut unit = reg.text;
            if !unit.ends_with('\n') {
                unit.push('\n');
            }
            let at = if before {
                buf.line_start(line)
            } else {
                buf.line_end(line) + 1
            };
            buf.insert(at, &unit.repeat(count));
            self.cursor = first_non_blank(buf, buf.line_of(at));
        } else {
            let text = reg.text.repeat(count);
            let at = if before || buf.line_len(line) == 0 {
                self.cursor
            } else {
                self.cursor + 1
            };
            buf.insert(at, &text);
            self.cursor = if text.contains('\n') {
                at
            } else {
                at + text.chars().count() - 1
            };
        }
    }

    /// `p` over a visual selection: replace it, keeping the register.
    fn visual_put(&mut self, cx: &mut Ctx<'_>) {
        let name = self.pending.register.take();
        let Some(reg) = cx.shared.read_register(name, cx.clipboard) else {
            return;
        };
        let buf = &mut *cx.buf;
        if self.mode == Mode::VisualBlock {
            // Over a block: in place of its first line's part, the rest gone.
            let ranges = self.block_ranges(buf);
            for &(s, e) in ranges.iter().skip(1).rev() {
                buf.delete(s..e);
            }
            let (s, e) = ranges[0];
            buf.edit(s..e, reg.text.strip_suffix('\n').unwrap_or(&reg.text));
            self.exit_visual();
            self.cursor = s;
            return;
        }
        let start = match self.visual_target(buf) {
            Target::Lines(a, b) => {
                let (s, e) = (buf.line_start(a), buf.line_end(b));
                buf.edit(s..e, reg.text.strip_suffix('\n').unwrap_or(&reg.text));
                first_non_blank(buf, a)
            }
            Target::Chars(s, e) => {
                if reg.linewise {
                    let body = reg.text.strip_suffix('\n').unwrap_or(&reg.text);
                    buf.edit(s..e, &format!("\n{body}\n"));
                    s + 1
                } else {
                    buf.edit(s..e, &reg.text);
                    s
                }
            }
        };
        self.exit_visual();
        self.cursor = start;
    }

    // ---- insert mode ---------------------------------------------------

    fn enter_insert(&mut self, buf: &mut Buffer) {
        if self.mode.is_visual() {
            self.last_visual = Some((self.anchor, self.cursor, self.mode));
        }
        if self.mode != Mode::Insert {
            buf.begin(self.cursor);
            self.mode = Mode::Insert;
        }
    }

    fn leave_insert(&mut self, buf: &Buffer) {
        self.mode = Mode::Normal;
        let ls = buf.line_start(buf.line_of(self.cursor));
        if self.cursor > ls {
            self.cursor -= 1;
        }
        self.goal_col = None;
    }

    fn finish_insert(&mut self, buf: &mut Buffer) {
        if let Some(block) = self.block_insert.take() {
            self.repeat_on_block(block, buf);
        }
        // Drop auto-indent that was never typed after.
        let line = buf.line_of(self.cursor);
        let (ls, le) = (buf.line_start(line), buf.line_end(line));
        if le > ls && first_non_blank(buf, line) == le {
            buf.delete(ls..le);
            self.cursor = ls;
        }
        buf.end();
        self.leave_insert(buf);
    }

    /// What was typed on the first line of a block goes on the others.
    fn repeat_on_block(&mut self, block: BlockInsert, buf: &mut Buffer) {
        let typed_here = self.cursor >= block.start
            && buf.line_of(self.cursor) == buf.line_of(block.start)
            && block.rows.1 <= buf.last_line();
        if !typed_here {
            return;
        }
        let text = buf.slice_string(block.start..self.cursor);
        if text.is_empty() {
            return;
        }
        for row in block.rows.0..=block.rows.1 {
            let (ls, len) = (buf.line_start(row), buf.line_len(row));
            if len >= block.col {
                buf.insert(ls + block.col, &text);
            } else if block.pad {
                let fill = " ".repeat(block.col - len);
                buf.insert(ls + len, &format!("{fill}{text}"));
            }
        }
    }

    /// Typing an opening bracket or quote closes it; typing the closing
    /// one that is already there steps over it. True if `c` was dealt with.
    fn auto_pair(&mut self, c: char, cx: &mut Ctx<'_>) -> bool {
        let buf = &mut *cx.buf;
        let pairs = &cx.lang.pairs;
        let next = buf.char_at(self.cursor);
        let ls = buf.line_start(buf.line_of(self.cursor));
        let prev = (self.cursor > ls).then(|| buf.char_at(self.cursor - 1));
        let is_word = |c: char| c.is_alphanumeric() || c == '_';

        if next == c && pairs.iter().any(|&(_, close)| close == c) {
            self.cursor += 1;
            return true;
        }
        let Some(&(open, close)) = pairs.iter().find(|pair| pair.0 == c) else {
            return false;
        };
        // Not in front of a word or of something that opens.
        if is_word(next) || pairs.iter().any(|pair| pair.0 == next && pair.1 != next) {
            return false;
        }
        // A quote after a word is an apostrophe, or closes a string.
        if open == close && prev.is_some_and(|prev| is_word(prev) || prev == c || prev == '\\') {
            return false;
        }
        let mut both = String::with_capacity(8);
        both.push(open);
        both.push(close);
        buf.insert(self.cursor, &both);
        self.cursor += 1;
        true
    }

    fn insert_raw(&mut self, c: char, buf: &mut Buffer) {
        let mut tmp = [0u8; 4];
        buf.insert(self.cursor, c.encode_utf8(&mut tmp));
        self.cursor += 1;
    }

    /// A key with no binding in insert mode: typed text inserts itself.
    fn insert_default(&mut self, key: Key, cx: &mut Ctx<'_>) {
        if let Key::Char(c) = key
            && cx.shared.auto_pairs
            && self.auto_pair(c, cx)
        {
            return;
        }
        let buf = &mut *cx.buf;
        match key {
            Key::Char(c) => {
                let line = buf.line_of(self.cursor);
                let (ls, le) = (buf.line_start(line), buf.line_end(line));
                // A closing bracket on an otherwise blank line dedents.
                if matches!(c, '}' | ')' | ']')
                    && self.cursor == le
                    && self.cursor > ls
                    && first_non_blank(buf, line) == le
                {
                    let unit = cx.lang.indent_unit().chars().count();
                    let remove = if buf.char_at(self.cursor - 1) == '\t' {
                        1
                    } else {
                        unit.min(self.cursor - ls)
                    };
                    buf.delete(self.cursor - remove..self.cursor);
                    self.cursor -= remove;
                }
                self.insert_raw(c, buf);
            }
            Key::Text(text) => {
                buf.insert(self.cursor, &text);
                self.cursor += text.chars().count();
            }
            _ => {}
        }
    }

    fn insert_command(&mut self, builtin: Builtin, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        // Normal-mode commands mean nothing while inserting.
        let Builtin::Insert(cmd) = builtin else {
            return;
        };
        let buf = &mut *cx.buf;
        let line = buf.line_of(self.cursor);
        let (ls, le) = (buf.line_start(line), buf.line_end(line));
        match cmd {
            InsertCmd::Escape => self.finish_insert(buf),
            InsertCmd::Newline => self.insert_newline(cx),
            InsertCmd::Backspace => {
                if self.cursor == 0 {
                    return;
                }
                let in_indent =
                    self.cursor > ls && (ls..self.cursor).all(|i| buf.char_at(i) == ' ');
                let n = if in_indent && cx.lang.expand_tab {
                    let width = cx.lang.tab_width.max(1);
                    (self.cursor - ls - 1) % width + 1
                } else {
                    1
                };
                // Between a pair with nothing in it: both go.
                let pair = (buf.char_at(self.cursor - 1), buf.char_at(self.cursor));
                let end = if cx.shared.auto_pairs && cx.lang.pairs.contains(&pair) {
                    self.cursor + 1
                } else {
                    self.cursor
                };
                buf.delete(self.cursor - n..end);
                self.cursor -= n;
            }
            InsertCmd::Delete => {
                if self.cursor < buf.max_offset() {
                    buf.delete(self.cursor..self.cursor + 1);
                }
            }
            InsertCmd::Tab => {
                if cx.lang.expand_tab {
                    let width = cx.lang.tab_width.max(1);
                    let n = width - (self.cursor - ls) % width;
                    buf.insert(self.cursor, &" ".repeat(n));
                    self.cursor += n;
                } else {
                    self.insert_raw('\t', buf);
                }
            }
            InsertCmd::DeleteWord => {
                if self.cursor > ls {
                    let start = motion::prev_word_start(buf, self.cursor, false).max(ls);
                    buf.delete(start..self.cursor);
                    self.cursor = start;
                } else if self.cursor > 0 {
                    buf.delete(self.cursor - 1..self.cursor);
                    self.cursor -= 1;
                }
            }
            InsertCmd::DeleteLine => {
                let fnb = first_non_blank(buf, line);
                let start = if self.cursor > fnb { fnb } else { ls };
                buf.delete(start..self.cursor);
                self.cursor = start;
            }
            InsertCmd::Indent | InsertCmd::Outdent => {
                let before = buf.len_chars();
                let op = if cmd == InsertCmd::Indent {
                    Operator::Indent
                } else {
                    Operator::Outdent
                };
                let cursor = self.cursor;
                self.apply_to_lines(op, line, line, cx);
                let after = cx.buf.len_chars();
                self.cursor = (cursor + after)
                    .saturating_sub(before)
                    .max(cx.buf.line_start(line));
            }
            InsertCmd::Left => {
                if self.cursor > ls {
                    self.cursor -= 1;
                }
                self.goal_col = None;
            }
            InsertCmd::Right => {
                if self.cursor < le {
                    self.cursor += 1;
                }
                self.goal_col = None;
            }
            InsertCmd::Up | InsertCmd::Down | InsertCmd::PageUp | InsertCmd::PageDown => {
                let page = cx.viewport.height.saturating_sub(2).max(1);
                let row = match cmd {
                    InsertCmd::Up => line.saturating_sub(1),
                    InsertCmd::Down => line + 1,
                    InsertCmd::PageUp => line.saturating_sub(page),
                    _ => line + page,
                }
                .min(buf.last_line());
                if row != line {
                    self.cursor = self.vertical(buf, row);
                    if matches!(cmd, InsertCmd::PageUp | InsertCmd::PageDown) {
                        let moved = row as isize - line as isize;
                        fx.push(Effect::Scroll(Scroll::Lines(moved)));
                    }
                }
            }
            InsertCmd::Home => {
                let fnb = first_non_blank(buf, line);
                self.cursor = if self.cursor == fnb { ls } else { fnb };
                self.goal_col = None;
            }
            InsertCmd::End => {
                self.cursor = le;
                self.goal_col = None;
            }
        }
    }

    fn insert_newline(&mut self, cx: &mut Ctx<'_>) {
        let buf = &mut *cx.buf;
        let line = buf.line_of(self.cursor);
        let (ls, le) = (buf.line_start(line), buf.line_end(line));
        let indent: String = leading_ws(buf, line)
            .chars()
            .take(self.cursor - ls)
            .collect();
        let unit = cx.lang.indent_unit();

        // Enter on a line holding only auto-indent leaves it empty.
        if self.cursor == le && self.cursor > ls && first_non_blank(buf, line) == le {
            buf.delete(ls..le);
            buf.insert(ls, &format!("\n{indent}"));
            self.cursor = ls + 1 + indent.chars().count();
            return;
        }

        let opens = opens_block(buf, ls, self.cursor);
        let closes_here = opens && {
            let open = last_non_blank(buf, ls, self.cursor);
            let close = buf.char_at(self.cursor);
            matches!(
                (open, close),
                (Some('{'), '}') | (Some('('), ')') | (Some('['), ']')
            )
        };
        if closes_here {
            // `{|}` → opener, indented cursor line, closer.
            buf.insert(self.cursor, &format!("\n{indent}{unit}\n{indent}"));
            self.cursor += 1 + indent.chars().count() + unit.chars().count();
        } else {
            let extra = if opens { unit.as_str() } else { "" };
            buf.insert(self.cursor, &format!("\n{indent}{extra}"));
            self.cursor += 1 + indent.chars().count() + extra.chars().count();
        }
    }

    // ---- command line --------------------------------------------------

    fn open_cmdline(&mut self, kind: char, text: String) {
        self.cmdline = Some(CmdLine { kind, text });
        self.mode = Mode::Command;
        self.no_repeat = true;
    }

    fn cmdline_key(&mut self, key: Key, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        let Some(cl) = &mut self.cmdline else {
            self.mode = Mode::Normal;
            return;
        };
        if key.is_escape() {
            self.cmdline = None;
            self.mode = Mode::Normal;
            return;
        }
        match key {
            Key::Enter | Key::Ctrl('j') | Key::Ctrl('m') => {
                let cl = self.cmdline.take().expect("checked above");
                self.mode = Mode::Normal;
                match cl.kind {
                    ':' => self.run_ex(&cl.text, cx, fx),
                    kind => self.run_search(&cl.text, kind == '/', cx, fx),
                }
            }
            Key::Backspace | Key::Ctrl('h') => {
                if cl.text.pop().is_none() {
                    self.cmdline = None;
                    self.mode = Mode::Normal;
                }
            }
            Key::Ctrl('u') => cl.text.clear(),
            Key::Ctrl('w') => {
                let trimmed = cl.text.trim_end_matches(|c: char| !c.is_whitespace());
                let len = trimmed.trim_end().len();
                cl.text.truncate(len);
            }
            Key::Char(c) => cl.text.push(c),
            Key::Text(text) => cl.text.extend(text.chars().filter(|c| !c.is_control())),
            Key::Tab => cl.text.push('\t'),
            _ => {}
        }
    }

    fn run_search(&mut self, text: &str, forward: bool, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        let pattern = if text.is_empty() {
            match &cx.shared.search {
                Some(s) => s.pattern.clone(),
                None => return,
            }
        } else {
            text.to_string()
        };
        let regex = ex::build_regex(&pattern, false);
        let found = find_match(cx.buf, &regex, self.cursor, forward, 1);
        cx.shared.search = Some(Search {
            pattern: pattern.clone(),
            regex,
            forward,
        });
        cx.shared.hlsearch = true;
        match found {
            Some(pos) => {
                self.jump_from(fx);
                self.cursor = pos;
                self.goal_col = None;
            }
            None => self.message = Some(format!("Pattern not found: {pattern}")),
        }
    }

    fn run_ex(&mut self, text: &str, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let range_cx = RangeCtx {
            cur: cx.buf.line_of(self.cursor),
            last: cx.buf.last_line(),
            visual: self.last_visual.map(|(a, c, _)| {
                let (s, e) = minmax(a, c);
                (cx.buf.line_of(s), cx.buf.line_of(e))
            }),
        };
        let (range, rest) = ex::parse_range(text, &range_cx);
        let rest = rest.trim();
        let cur_line = range_cx.cur;

        if rest.is_empty() {
            if let Some((_, line)) = range {
                self.jump_from(fx);
                self.cursor = first_non_blank(cx.buf, line);
                self.goal_col = None;
            }
            return;
        }
        if matches!(rest, "noh" | "nohl" | "nohlsearch") {
            cx.shared.hlsearch = false;
            return;
        }
        if let Some(sub) = ex::parse_substitute(rest) {
            let (a, b) = range.unwrap_or((cur_line, cur_line));
            self.substitute(sub, a, b, cx);
            return;
        }
        let (a, b) = range.unwrap_or((cur_line, cur_line));
        let op = match rest {
            "d" | "delete" => Some(Operator::Delete),
            "y" | "yank" => Some(Operator::Yank),
            ">" => Some(Operator::Indent),
            "<" => Some(Operator::Outdent),
            _ => None,
        };
        match op {
            Some(op) => {
                cx.buf.begin(self.cursor);
                self.apply_to_lines(op, a, b, cx);
                cx.buf.end();
            }
            None => fx.push(Effect::Ex(text.to_string())),
        }
    }

    fn substitute(&mut self, sub: ex::Substitute, a: usize, b: usize, cx: &mut Ctx<'_>) {
        let pattern = if sub.pattern.is_empty() {
            match &cx.shared.search {
                Some(s) => s.pattern.clone(),
                None => return,
            }
        } else {
            sub.pattern.clone()
        };
        let regex = ex::build_regex(&pattern, sub.ignore_case);
        let buf = &mut *cx.buf;
        let (mut hits, mut lines, mut last_line) = (0usize, 0usize, None);
        buf.begin(self.cursor);
        // Bottom-up, so a replacement that adds lines doesn't shift the ones
        // still to be visited.
        for line in (a..=b.min(buf.last_line())).rev() {
            let text = buf.line_string(line);
            let found = regex.find_iter(&text).count();
            if found == 0 {
                continue;
            }
            let replaced = if sub.global {
                hits += found;
                regex.replace_all(&text, sub.replacement.as_str())
            } else {
                hits += 1;
                regex.replace(&text, sub.replacement.as_str())
            };
            let (ls, le) = (buf.line_start(line), buf.line_end(line));
            buf.edit(ls..le, &replaced);
            lines += 1;
            last_line.get_or_insert(line);
        }
        buf.end();
        match last_line {
            Some(line) => {
                self.cursor = first_non_blank(buf, line);
                self.message = Some(format!(
                    "{hits} substitution{} on {lines} line{}",
                    if hits == 1 { "" } else { "s" },
                    if lines == 1 { "" } else { "s" },
                ));
            }
            None => self.message = Some(format!("Pattern not found: {pattern}")),
        }
        cx.shared.search = Some(Search {
            pattern,
            regex,
            forward: true,
        });
    }
}

/// The delimiters `c` stands for when surrounding: a closing bracket (or
/// `b`, `r`, `B`, `a`) for the pair as it is, an opening one for the pair
/// with a blank inside each, anything else for itself on both sides.
fn surround_pair(c: char) -> (String, String) {
    let (open, close) = match c {
        '(' => ("( ", " )"),
        ')' | 'b' => ("(", ")"),
        '[' => ("[ ", " ]"),
        ']' | 'r' => ("[", "]"),
        '{' => ("{ ", " }"),
        '}' | 'B' => ("{", "}"),
        '<' => ("< ", " >"),
        '>' | 'a' => ("<", ">"),
        c => return (c.to_string(), c.to_string()),
    };
    (open.to_string(), close.to_string())
}

fn last_non_blank(buf: &Buffer, start: usize, end: usize) -> Option<char> {
    (start..end)
        .rev()
        .map(|i| buf.char_at(i))
        .find(|c| !is_blank(*c))
}

/// Does the text in `start..end` end with an opening bracket?
fn opens_block(buf: &Buffer, start: usize, end: usize) -> bool {
    matches!(last_non_blank(buf, start, end), Some('{' | '(' | '['))
}

fn change_case(buf: &mut Buffer, op: Operator, start: usize, end: usize) {
    let old = buf.slice_string(start..end);
    let new: String = old
        .chars()
        .map(|c| {
            let mapped = match op {
                Operator::Lower => c.to_lowercase().next(),
                Operator::Upper => c.to_uppercase().next(),
                _ if c.is_uppercase() => c.to_lowercase().next(),
                _ => c.to_uppercase().next(),
            };
            mapped.unwrap_or(c)
        })
        .collect();
    if new != old {
        buf.edit(start..end, &new);
    }
}

fn toggle_comment(buf: &mut Buffer, lang: &Lang, a: usize, b: usize) {
    let Some(token) = lang.line_comment.as_deref() else {
        return;
    };
    let token_len = token.chars().count();
    let lines: Vec<usize> = (a..=b)
        .filter(|&l| !motion::is_blank_line(buf, l))
        .collect();
    if lines.is_empty() {
        return;
    }
    let commented = |buf: &Buffer, line: usize| {
        let fnb = first_non_blank(buf, line);
        buf.slice_string(fnb..fnb + token_len) == token
    };
    if lines.iter().all(|&l| commented(buf, l)) {
        for &line in &lines {
            let fnb = first_non_blank(buf, line);
            let mut end = fnb + token_len;
            if buf.char_at(end) == ' ' {
                end += 1;
            }
            buf.delete(fnb..end);
        }
    } else {
        let indent = lines
            .iter()
            .map(|&l| first_non_blank(buf, l) - buf.line_start(l))
            .min()
            .unwrap_or(0);
        for &line in &lines {
            let at = buf.line_start(line) + indent;
            buf.insert(at, &format!("{token} "));
        }
    }
}

/// The `n`-th match start from `cur` in the given direction, wrapping.
pub fn find_match(
    buf: &Buffer,
    regex: &Regex,
    cur: usize,
    forward: bool,
    n: usize,
) -> Option<usize> {
    let text = buf.text();
    let cur_byte = buf.char_to_byte(cur);
    let limit = text.len().saturating_sub(1);
    let starts: Vec<usize> = regex
        .find_iter(&text)
        .map(|m| m.start())
        .filter(|&s| s < limit)
        .collect();
    if starts.is_empty() {
        return None;
    }
    let len = starts.len();
    let steps = (n.max(1) - 1) % len;
    let index = if forward {
        let first = starts.iter().position(|&s| s > cur_byte).unwrap_or(0);
        (first + steps) % len
    } else {
        let first = starts
            .iter()
            .rposition(|&s| s < cur_byte)
            .unwrap_or(len - 1);
        (first + len - steps) % len
    };
    Some(buf.byte_to_char(starts[index]))
}
