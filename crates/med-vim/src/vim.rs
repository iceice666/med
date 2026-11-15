//! The modal editing engine: a key-driven state machine over a [`Buffer`].
//!
//! The engine is UI-agnostic. The host feeds it [`Key`]s and applies the
//! returned [`Effect`]s for anything that reaches outside the buffer (ex
//! commands such as `:w`, LSP requests, scrolling).

use crate::ex::{self, RangeCtx};
use crate::motion::{self, Find, WS, class, first_non_blank, leading_ws};
use crate::textobj;
use med_core::Buffer;
use regex::Regex;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Editing the `:` / `/` / `?` command line.
    Command,
}

impl Mode {
    pub fn is_visual(self) -> bool {
        matches!(self, Mode::Visual | Mode::VisualLine)
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
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
}

impl Default for Lang {
    fn default() -> Self {
        Self {
            tab_width: 4,
            expand_tab: true,
            line_comment: None,
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Register {
    pub text: String,
    pub linewise: bool,
}

pub struct Search {
    pub pattern: String,
    pub regex: Regex,
    pub forward: bool,
}

/// State shared by every editor: registers, the last search, the last
/// repeatable change.
#[derive(Default)]
pub struct Shared {
    registers: HashMap<char, Register>,
    pub search: Option<Search>,
    pub hlsearch: bool,
    last_find: Option<Find>,
    last_change: Vec<Key>,
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
}

pub struct Ctx<'a> {
    pub buf: &'a mut Buffer,
    pub shared: &'a mut Shared,
    pub clipboard: &'a mut dyn Clipboard,
    pub viewport: Viewport,
    pub lang: &'a Lang,
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
    JumpBack,
    JumpForward,
    GotoDefinition,
    References,
    Hover,
    NextDiagnostic,
    PrevDiagnostic,
    TriggerCompletion,
    NextTab,
    PrevTab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operator {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
    Lower,
    Upper,
    ToggleCase,
    Comment,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Motion {
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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Prefix {
    #[default]
    None,
    G,
    Z,
    Find {
        forward: bool,
        till: bool,
    },
    Replace,
    TextObject {
        around: bool,
    },
    Register,
    Bracket {
        forward: bool,
    },
}

#[derive(Default)]
struct Pending {
    count: Option<usize>,
    op: Option<Operator>,
    op_count: Option<usize>,
    register: Option<char>,
    prefix: Prefix,
}

impl Pending {
    fn is_idle(&self) -> bool {
        self.count.is_none()
            && self.op.is_none()
            && self.op_count.is_none()
            && self.register.is_none()
            && self.prefix == Prefix::None
    }

    fn count(&self) -> Option<usize> {
        match (self.count, self.op_count) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(1).saturating_mul(b.unwrap_or(1))),
        }
    }
}

struct CmdLine {
    kind: char,
    text: String,
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
    last_visual: Option<(usize, usize, Mode)>,
    /// A one-line message for the status bar (errors, substitution counts).
    pub message: Option<String>,
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
            last_visual: None,
            message: None,
        }
    }

    // ---- host-facing state ---------------------------------------------

    /// The command line being edited: its prompt char and text.
    pub fn cmdline(&self) -> Option<(char, &str)> {
        self.cmdline.as_ref().map(|c| (c.kind, c.text.as_str()))
    }

    /// Keys typed so far for an unfinished command (vim's `showcmd`).
    pub fn pending_display(&self) -> String {
        let p = &self.pending;
        let mut out = String::new();
        if let Some(r) = p.register {
            out.push('"');
            out.push(r);
        }
        if let Some(c) = p.count {
            out.push_str(&c.to_string());
        }
        if let Some(op) = p.op {
            out.push_str(match op {
                Operator::Delete => "d",
                Operator::Change => "c",
                Operator::Yank => "y",
                Operator::Indent => ">",
                Operator::Outdent => "<",
                Operator::Lower => "gu",
                Operator::Upper => "gU",
                Operator::ToggleCase => "g~",
                Operator::Comment => "gc",
            });
        }
        if let Some(c) = p.op_count {
            out.push_str(&c.to_string());
        }
        match p.prefix {
            Prefix::None => {}
            Prefix::G => out.push('g'),
            Prefix::Z => out.push('z'),
            Prefix::Find { forward, till } => out.push(match (forward, till) {
                (true, false) => 'f',
                (false, false) => 'F',
                (true, true) => 't',
                (false, true) => 'T',
            }),
            Prefix::Replace => out.push('r'),
            Prefix::TextObject { around } => out.push(if around { 'a' } else { 'i' }),
            Prefix::Register => out.push('"'),
            Prefix::Bracket { forward } => out.push(if forward { ']' } else { '[' }),
        }
        out
    }

    /// True while a multi-key command is half typed.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_idle()
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
            _ => None,
        }
    }

    /// Move the cursor from outside (mouse click, goto-definition…).
    pub fn set_cursor(&mut self, offset: usize, buf: &Buffer) {
        if self.mode.is_visual() {
            self.exit_visual();
        }
        self.cursor = offset;
        self.goal_col = None;
        self.pending = Pending::default();
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
        fx
    }

    fn dispatch(&mut self, key: Key, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        if !self.replaying {
            if self.keys.is_empty() {
                self.cmd_start_mode = self.mode;
                self.cmd_changed = false;
                self.no_repeat = false;
            }
            self.keys.push(key.clone());
        }
        let version = cx.buf.version();
        match self.mode {
            Mode::Insert => self.insert_key(key, cx, fx),
            Mode::Command => self.cmdline_key(key, cx, fx),
            Mode::Normal | Mode::Visual | Mode::VisualLine => {
                let undo_redo = self.mode == Mode::Normal
                    && self.pending.op.is_none()
                    && self.pending.prefix == Prefix::None
                    && matches!(key, Key::Char('u') | Key::Ctrl('r'));
                if undo_redo {
                    // Undo cannot run inside the undo group below.
                    let count = self.pending.count().unwrap_or(1);
                    self.pending = Pending::default();
                    self.no_repeat = true;
                    for _ in 0..count {
                        let pos = if key == Key::Char('u') {
                            cx.buf.undo()
                        } else {
                            cx.buf.redo()
                        };
                        match pos {
                            Some(pos) => self.cursor = pos,
                            None => break,
                        }
                    }
                    self.goal_col = None;
                } else {
                    cx.buf.begin(self.cursor);
                    self.normal_key(key, cx, fx);
                    cx.buf.end();
                }
            }
        }
        if cx.buf.version() != version {
            self.cmd_changed = true;
        }
        self.clamp(cx.buf);

        let settled = self.pending.is_idle() && !matches!(self.mode, Mode::Insert | Mode::Command);
        if settled && !self.replaying {
            let keys = std::mem::take(&mut self.keys);
            if self.cmd_changed && !self.no_repeat && self.cmd_start_mode == Mode::Normal {
                cx.shared.last_change = keys;
            }
        }
    }

    fn reset_pending(&mut self) {
        self.pending = Pending::default();
    }

    fn normal_key(&mut self, key: Key, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        if key.is_escape() {
            self.reset_pending();
            if self.mode.is_visual() {
                self.exit_visual();
            }
            return;
        }

        match std::mem::take(&mut self.pending.prefix) {
            Prefix::None => {}
            Prefix::Register => {
                match key {
                    Key::Char(c) => self.pending.register = Some(c),
                    _ => self.reset_pending(),
                }
                return;
            }
            Prefix::Find { forward, till } => {
                match key {
                    Key::Char(c) => {
                        let find = Find { c, forward, till };
                        cx.shared.last_find = Some(find);
                        self.run_motion(Motion::Find(find), cx, fx);
                    }
                    _ => self.reset_pending(),
                }
                return;
            }
            Prefix::Replace => {
                match key {
                    Key::Char(c) => self.replace_chars(c, cx),
                    Key::Enter => self.replace_chars('\n', cx),
                    _ => {}
                }
                self.reset_pending();
                return;
            }
            Prefix::TextObject { around } => {
                match key {
                    Key::Char(c) => self.run_text_object(c, around, cx),
                    _ => self.reset_pending(),
                }
                return;
            }
            Prefix::G => return self.g_key(key, cx, fx),
            Prefix::Z => {
                match key {
                    Key::Char('z') | Key::Char('.') => fx.push(Effect::Scroll(Scroll::Center)),
                    Key::Char('t') | Key::Enter => fx.push(Effect::Scroll(Scroll::Top)),
                    Key::Char('b') | Key::Char('-') => fx.push(Effect::Scroll(Scroll::Bottom)),
                    _ => {}
                }
                self.reset_pending();
                return;
            }
            Prefix::Bracket { forward } => {
                if key == Key::Char('d') {
                    fx.push(if forward {
                        Effect::NextDiagnostic
                    } else {
                        Effect::PrevDiagnostic
                    });
                }
                self.reset_pending();
                return;
            }
        }

        // Counts. A leading 0 is the line-start motion.
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

        let op_pending = self.pending.op.is_some();
        let visual = self.mode.is_visual();

        // Doubled g-operators: `guu`, `gUU`, `g~~`, `gcc`.
        if let (Some(op), Key::Char(c)) = (self.pending.op, &key) {
            let doubled = matches!(
                (op, *c),
                (Operator::Lower, 'u')
                    | (Operator::Upper, 'U')
                    | (Operator::ToggleCase, '~')
                    | (Operator::Comment, 'c')
            );
            if doubled {
                return self.operator_key(op, cx);
            }
        }

        if let Some(motion) = Self::key_to_motion(&key) {
            return self.run_motion(motion, cx, fx);
        }

        let Key::Char(c) = key else {
            match key {
                Key::Ctrl('o') if !op_pending => fx.push(Effect::JumpBack),
                Key::Ctrl('i') | Key::Tab if !op_pending => fx.push(Effect::JumpForward),
                Key::Ctrl('e') if !op_pending => fx.push(Effect::Scroll(Scroll::Lines(1))),
                Key::Ctrl('y') if !op_pending => fx.push(Effect::Scroll(Scroll::Lines(-1))),
                Key::Delete if !op_pending => self.delete_chars(false, cx),
                _ => {}
            }
            self.reset_pending();
            return;
        };

        match c {
            '"' => self.pending.prefix = Prefix::Register,
            'f' => {
                self.pending.prefix = Prefix::Find {
                    forward: true,
                    till: false,
                }
            }
            'F' => {
                self.pending.prefix = Prefix::Find {
                    forward: false,
                    till: false,
                }
            }
            't' => {
                self.pending.prefix = Prefix::Find {
                    forward: true,
                    till: true,
                }
            }
            'T' => {
                self.pending.prefix = Prefix::Find {
                    forward: false,
                    till: true,
                }
            }
            'g' => self.pending.prefix = Prefix::G,
            'z' if !op_pending => self.pending.prefix = Prefix::Z,
            ']' if !op_pending => self.pending.prefix = Prefix::Bracket { forward: true },
            '[' if !op_pending => self.pending.prefix = Prefix::Bracket { forward: false },
            'i' | 'a' if op_pending || visual => {
                self.pending.prefix = Prefix::TextObject { around: c == 'a' }
            }
            'd' => self.operator_key(Operator::Delete, cx),
            'c' => self.operator_key(Operator::Change, cx),
            'y' => self.operator_key(Operator::Yank, cx),
            '>' => self.operator_key(Operator::Indent, cx),
            '<' => self.operator_key(Operator::Outdent, cx),
            _ if op_pending => self.reset_pending(),
            _ if visual => self.visual_command(c, cx, fx),
            _ => self.normal_command(c, cx, fx),
        }
    }

    fn key_to_motion(key: &Key) -> Option<Motion> {
        Some(match key {
            Key::Char('h') | Key::Left | Key::Backspace | Key::Ctrl('h') => Motion::Left,
            Key::Char('l') | Key::Right | Key::Char(' ') => Motion::Right,
            Key::Char('j') | Key::Down | Key::Ctrl('j') | Key::Ctrl('n') => Motion::Down,
            Key::Char('k') | Key::Up | Key::Ctrl('p') => Motion::Up,
            Key::Char('w') => Motion::WordFwd(false),
            Key::Char('W') => Motion::WordFwd(true),
            Key::Char('b') => Motion::WordBack(false),
            Key::Char('B') => Motion::WordBack(true),
            Key::Char('e') => Motion::WordEnd(false),
            Key::Char('E') => Motion::WordEnd(true),
            Key::Char('0') | Key::Home => Motion::LineStart,
            Key::Char('^') => Motion::FirstNonBlank,
            Key::Char('$') | Key::End => Motion::LineEnd,
            Key::Char('G') => Motion::Line { default_last: true },
            Key::Char(';') => Motion::RepeatFind { reverse: false },
            Key::Char(',') => Motion::RepeatFind { reverse: true },
            Key::Char('%') => Motion::MatchPair,
            Key::Char('}') => Motion::ParaFwd,
            Key::Char('{') => Motion::ParaBack,
            Key::Char('H') => Motion::ScreenTop,
            Key::Char('M') => Motion::ScreenMid,
            Key::Char('L') => Motion::ScreenBot,
            Key::Ctrl('d') => Motion::Page {
                down: true,
                half: true,
            },
            Key::Ctrl('u') => Motion::Page {
                down: false,
                half: true,
            },
            Key::Ctrl('f') | Key::PageDown => Motion::Page {
                down: true,
                half: false,
            },
            Key::Ctrl('b') | Key::PageUp => Motion::Page {
                down: false,
                half: false,
            },
            Key::Char('n') => Motion::SearchNext { reverse: false },
            Key::Char('N') => Motion::SearchNext { reverse: true },
            Key::Char('*') => Motion::WordSearch { forward: true },
            Key::Char('#') => Motion::WordSearch { forward: false },
            Key::Char('+') | Key::Enter => Motion::NextLine,
            Key::Char('-') => Motion::PrevLine,
            Key::Char('_') => Motion::CurLine,
            Key::Char('|') => Motion::Column,
            _ => return None,
        })
    }

    fn g_key(&mut self, key: Key, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        let op_pending = self.pending.op.is_some();
        match key {
            Key::Char('g') => {
                return self.run_motion(
                    Motion::Line {
                        default_last: false,
                    },
                    cx,
                    fx,
                );
            }
            Key::Char('e') => return self.run_motion(Motion::WordEndBack(false), cx, fx),
            Key::Char('E') => return self.run_motion(Motion::WordEndBack(true), cx, fx),
            Key::Char('j') => return self.run_motion(Motion::Down, cx, fx),
            Key::Char('k') => return self.run_motion(Motion::Up, cx, fx),
            Key::Char('u') => return self.operator_key(Operator::Lower, cx),
            Key::Char('U') => return self.operator_key(Operator::Upper, cx),
            Key::Char('~') => return self.operator_key(Operator::ToggleCase, cx),
            Key::Char('c') => return self.operator_key(Operator::Comment, cx),
            _ if op_pending => {}
            Key::Char('d') | Key::Char('D') => fx.push(Effect::GotoDefinition),
            Key::Char('r') => fx.push(Effect::References),
            Key::Char('h') => fx.push(Effect::Hover),
            Key::Char('t') => fx.push(Effect::NextTab),
            Key::Char('T') => fx.push(Effect::PrevTab),
            Key::Char('J') => self.join_lines(false, cx),
            Key::Char('v') => {
                if let Some((anchor, cursor, mode)) = self.last_visual {
                    self.anchor = anchor;
                    self.cursor = cursor;
                    self.mode = mode;
                }
            }
            _ => {}
        }
        self.reset_pending();
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
        }
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
                    fx.push(Effect::Jump { from: self.cursor });
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
        if self.mode.is_visual() {
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
        cx.shared
            .write_register(name, Register { text, linewise }, yank, cx.clipboard);
    }

    fn apply_operator(&mut self, op: Operator, target: Target, cx: &mut Ctx<'_>) {
        if self.mode.is_visual() {
            self.last_visual = Some((self.anchor, self.cursor, self.mode));
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
        }
    }

    fn run_text_object(&mut self, kind: char, around: bool, cx: &mut Ctx<'_>) {
        let Some(obj) = textobj::text_object(cx.buf, self.cursor, kind, around) else {
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

    // ---- simple commands -----------------------------------------------

    fn normal_command(&mut self, c: char, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        let line = cx.buf.line_of(self.cursor);
        match c {
            'i' => self.enter_insert(cx.buf),
            'a' => {
                self.enter_insert(cx.buf);
                if cx.buf.line_len(line) > 0 {
                    self.cursor += 1;
                }
            }
            'I' => {
                self.enter_insert(cx.buf);
                self.cursor = first_non_blank(cx.buf, line);
            }
            'A' => {
                self.enter_insert(cx.buf);
                self.cursor = cx.buf.line_end(line);
            }
            'o' => {
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
            'O' => {
                self.enter_insert(cx.buf);
                let buf = &mut *cx.buf;
                let indent = leading_ws(buf, line);
                let at = buf.line_start(line);
                buf.insert(at, &format!("{indent}\n"));
                self.cursor = at + indent.chars().count();
            }
            'x' => self.delete_chars(false, cx),
            'X' => self.delete_chars(true, cx),
            's' => {
                let n = self.pending.count().unwrap_or(1);
                let end = (self.cursor + n).min(cx.buf.line_end(line));
                self.apply_operator(Operator::Change, Target::Chars(self.cursor, end), cx);
            }
            'S' => self.repeat_op_on_lines(Operator::Change, cx),
            'Y' => self.repeat_op_on_lines(Operator::Yank, cx),
            'D' | 'C' => {
                let n = self.pending.count().unwrap_or(1).max(1);
                let last = (line + n - 1).min(cx.buf.last_line());
                let end = cx.buf.line_end(last);
                let op = if c == 'D' {
                    Operator::Delete
                } else {
                    Operator::Change
                };
                self.apply_operator(op, Target::Chars(self.cursor, end), cx);
            }
            'r' => {
                self.pending.prefix = Prefix::Replace;
                return;
            }
            'J' => self.join_lines(true, cx),
            '~' => {
                let n = self.pending.count().unwrap_or(1).max(1);
                let le = cx.buf.line_end(line);
                let end = (self.cursor + n).min(le);
                change_case(cx.buf, Operator::ToggleCase, self.cursor, end);
                self.cursor = end;
            }
            'p' => self.put(false, cx),
            'P' => self.put(true, cx),
            '.' => {
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
            'v' => {
                self.mode = Mode::Visual;
                self.anchor = self.cursor;
            }
            'V' => {
                self.mode = Mode::VisualLine;
                self.anchor = self.cursor;
            }
            ':' | '/' | '?' => self.open_cmdline(c, String::new()),
            'K' => fx.push(Effect::Hover),
            _ => {}
        }
        self.reset_pending();
    }

    fn visual_command(&mut self, c: char, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        match c {
            'v' | 'V' => {
                let target = if c == 'v' {
                    Mode::Visual
                } else {
                    Mode::VisualLine
                };
                if self.mode == target {
                    self.exit_visual();
                } else {
                    self.mode = target;
                }
            }
            'o' => std::mem::swap(&mut self.anchor, &mut self.cursor),
            'x' => self.operator_key(Operator::Delete, cx),
            's' => self.operator_key(Operator::Change, cx),
            'u' => self.operator_key(Operator::Lower, cx),
            'U' => self.operator_key(Operator::Upper, cx),
            '~' => self.operator_key(Operator::ToggleCase, cx),
            'Y' | 'D' | 'X' | 'C' | 'S' | 'R' => {
                self.mode = Mode::VisualLine;
                let op = match c {
                    'Y' => Operator::Yank,
                    'D' | 'X' => Operator::Delete,
                    _ => Operator::Change,
                };
                self.operator_key(op, cx);
            }
            'J' => self.join_lines(true, cx),
            'r' => {
                self.pending.prefix = Prefix::Replace;
                return;
            }
            'p' | 'P' => self.visual_put(cx),
            ':' => {
                self.last_visual = Some((self.anchor, self.cursor, self.mode));
                self.open_cmdline(':', "'<,'>".to_string());
            }
            '/' | '?' => {
                self.exit_visual();
                self.open_cmdline(c, String::new());
            }
            'K' => fx.push(Effect::Hover),
            _ => {}
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
            let (s, e) = match self.visual_target(buf) {
                Target::Chars(s, e) => (s, e),
                Target::Lines(a, b) => (buf.line_start(a), buf.line_end(b)),
            };
            let replaced: String = buf
                .slice_string(s..e)
                .chars()
                .map(|ch| if ch == '\n' { '\n' } else { c })
                .collect();
            buf.edit(s..e, &replaced);
            self.exit_visual();
            self.cursor = s;
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
        if reg.linewise {
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

    fn insert_raw(&mut self, c: char, buf: &mut Buffer) {
        let mut tmp = [0u8; 4];
        buf.insert(self.cursor, c.encode_utf8(&mut tmp));
        self.cursor += 1;
    }

    fn insert_key(&mut self, key: Key, cx: &mut Ctx<'_>, fx: &mut Vec<Effect>) {
        if key.is_escape() {
            self.finish_insert(cx.buf);
            return;
        }
        let buf = &mut *cx.buf;
        let line = buf.line_of(self.cursor);
        let (ls, le) = (buf.line_start(line), buf.line_end(line));
        match key {
            Key::Char(c) => {
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
            Key::Enter | Key::Ctrl('j') | Key::Ctrl('m') => self.insert_newline(cx),
            Key::Backspace | Key::Ctrl('h') => {
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
                buf.delete(self.cursor - n..self.cursor);
                self.cursor -= n;
            }
            Key::Delete => {
                if self.cursor < buf.max_offset() {
                    buf.delete(self.cursor..self.cursor + 1);
                }
            }
            Key::Tab => {
                if cx.lang.expand_tab {
                    let width = cx.lang.tab_width.max(1);
                    let n = width - (self.cursor - ls) % width;
                    buf.insert(self.cursor, &" ".repeat(n));
                    self.cursor += n;
                } else {
                    self.insert_raw('\t', buf);
                }
            }
            Key::Ctrl('w') => {
                if self.cursor > ls {
                    let start = motion::prev_word_start(buf, self.cursor, false).max(ls);
                    buf.delete(start..self.cursor);
                    self.cursor = start;
                } else if self.cursor > 0 {
                    buf.delete(self.cursor - 1..self.cursor);
                    self.cursor -= 1;
                }
            }
            Key::Ctrl('u') => {
                let fnb = first_non_blank(buf, line);
                let start = if self.cursor > fnb { fnb } else { ls };
                buf.delete(start..self.cursor);
                self.cursor = start;
            }
            Key::Ctrl('t') | Key::Ctrl('d') => {
                let before = buf.len_chars();
                let op = if key == Key::Ctrl('t') {
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
            Key::Left => {
                if self.cursor > ls {
                    self.cursor -= 1;
                }
                self.goal_col = None;
            }
            Key::Right => {
                if self.cursor < le {
                    self.cursor += 1;
                }
                self.goal_col = None;
            }
            Key::Up | Key::Down | Key::PageUp | Key::PageDown => {
                let page = cx.viewport.height.saturating_sub(2).max(1);
                let row = match key {
                    Key::Up => line.saturating_sub(1),
                    Key::Down => line + 1,
                    Key::PageUp => line.saturating_sub(page),
                    _ => line + page,
                }
                .min(buf.last_line());
                if row != line {
                    self.cursor = self.vertical(buf, row);
                    if matches!(key, Key::PageUp | Key::PageDown) {
                        let moved = row as isize - line as isize;
                        fx.push(Effect::Scroll(Scroll::Lines(moved)));
                    }
                }
            }
            Key::Home => {
                let fnb = first_non_blank(buf, line);
                self.cursor = if self.cursor == fnb { ls } else { fnb };
                self.goal_col = None;
            }
            Key::End => {
                self.cursor = le;
                self.goal_col = None;
            }
            Key::Ctrl('n') | Key::Ctrl('p') | Key::Ctrl(' ') => fx.push(Effect::TriggerCompletion),
            _ => {}
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
                fx.push(Effect::Jump { from: self.cursor });
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
                fx.push(Effect::Jump { from: self.cursor });
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
