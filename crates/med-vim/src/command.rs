//! The engine's commands, by name.
//!
//! Keys are never matched directly: a key sequence is bound to a command
//! name in the [`Keymap`](crate::Keymap), and the name is resolved here.
//! Names the engine does not know belong to the host and reach it as
//! [`Effect::Command`](crate::Effect::Command).

use crate::vim::{Key, Motion, Operator, Scroll};
use std::sync::Arc;

/// What a key sequence is bound to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    Builtin(Builtin),
    /// A command the host implements.
    Host(Arc<str>),
    /// Other keys, typed in its place and mapped again.
    Keys(Arc<[Key]>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Builtin {
    Motion(Motion),
    Operator(Operator),
    Await(Await),
    Scroll(Scroll),
    Cmd(Cmd),
    Insert(InsertCmd),
}

/// Commands that take the next key as a literal argument.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Await {
    /// `"x`
    Register,
    /// `f` / `F` / `t` / `T`
    Find { forward: bool, till: bool },
    /// `r`
    Replace,
    /// `i` / `a` after an operator or in visual mode
    TextObject { around: bool },
    /// `m`: the name of the mark to set.
    SetMark,
    /// `'` / `` ` ``: the mark to go to, to its line or to the very place.
    Mark { linewise: bool },
    /// `q`: the register to record keys into.
    RecordRegister,
    /// `@`: the register whose keys to run.
    PlayRegister,
    /// The delimiter to put around what a surround operator was given.
    SurroundAdd,
    /// `ds`: the delimiter to remove.
    SurroundDelete,
    /// `cs`: the delimiter to replace…
    SurroundChange,
    /// …and, with that one known, what to replace it with.
    SurroundChangeTo(char),
}

/// Normal- and visual-mode commands that are neither motions nor operators.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Cmd {
    /// Leave visual mode; in normal mode, nothing.
    Escape,
    Insert,
    Append,
    InsertLineStart,
    AppendLineEnd,
    OpenBelow,
    OpenAbove,
    DeleteChar {
        before: bool,
    },
    SubstituteChar,
    /// The operator on whole lines: `count` lines, or the selection's.
    Lines(Operator),
    /// The operator from the cursor to the end of the line.
    ToLineEnd(Operator),
    Join {
        space: bool,
    },
    ToggleCaseChar,
    Put {
        before: bool,
    },
    Repeat,
    Undo,
    Redo,
    /// Enter visual mode, switch its kind, or leave it.
    Visual {
        linewise: bool,
    },
    SwapEnds,
    Reselect,
    /// Open the command line with this prompt (`:`, `/`, `?`).
    Prompt(char),
    /// Enter blockwise visual mode, switch to it, or leave it.
    VisualBlock,
    /// `I` / `A` in visual mode: insert before or after the selection, on
    /// every line of a block.
    BlockInsert {
        append: bool,
    },
    /// `q`: start recording keys into a register, or stop.
    Record,
    /// `s` after an operator: `ys` surrounds, `ds` and `cs` take
    /// delimiters away and change them.
    Surround,
    /// Select the syntax node around the cursor or the selection.
    Expand,
    /// Go back to what was selected before the last `Expand`.
    Shrink,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum InsertCmd {
    Escape,
    Newline,
    Backspace,
    Delete,
    Tab,
    DeleteWord,
    DeleteLine,
    Indent,
    Outdent,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

const fn motion(m: Motion) -> Builtin {
    Builtin::Motion(m)
}

const fn syntax(kind: char, forward: bool) -> Builtin {
    Builtin::Motion(Motion::Syntax { kind, forward })
}

const fn find(forward: bool, till: bool) -> Builtin {
    Builtin::Await(Await::Find { forward, till })
}

const fn op(o: Operator) -> Builtin {
    Builtin::Operator(o)
}

const fn cmd(c: Cmd) -> Builtin {
    Builtin::Cmd(c)
}

const fn insert(c: InsertCmd) -> Builtin {
    Builtin::Insert(c)
}

const BUILTINS: &[(&str, Builtin)] = &[
    // Motions: on their own they move the cursor, after an operator they
    // say what it acts on, in visual mode they extend the selection.
    ("motion.left", motion(Motion::Left)),
    ("motion.right", motion(Motion::Right)),
    ("motion.up", motion(Motion::Up)),
    ("motion.down", motion(Motion::Down)),
    ("motion.word_forward", motion(Motion::WordFwd(false))),
    ("motion.big_word_forward", motion(Motion::WordFwd(true))),
    ("motion.word_backward", motion(Motion::WordBack(false))),
    ("motion.big_word_backward", motion(Motion::WordBack(true))),
    ("motion.word_end", motion(Motion::WordEnd(false))),
    ("motion.big_word_end", motion(Motion::WordEnd(true))),
    (
        "motion.word_end_backward",
        motion(Motion::WordEndBack(false)),
    ),
    (
        "motion.big_word_end_backward",
        motion(Motion::WordEndBack(true)),
    ),
    ("motion.line_start", motion(Motion::LineStart)),
    ("motion.first_non_blank", motion(Motion::FirstNonBlank)),
    ("motion.line_end", motion(Motion::LineEnd)),
    (
        "motion.goto_line",
        motion(Motion::Line { default_last: true }),
    ),
    (
        "motion.goto_first_line",
        motion(Motion::Line {
            default_last: false,
        }),
    ),
    ("motion.find_forward", find(true, false)),
    ("motion.find_backward", find(false, false)),
    ("motion.till_forward", find(true, true)),
    ("motion.till_backward", find(false, true)),
    (
        "motion.repeat_find",
        motion(Motion::RepeatFind { reverse: false }),
    ),
    (
        "motion.repeat_find_reverse",
        motion(Motion::RepeatFind { reverse: true }),
    ),
    ("motion.match_pair", motion(Motion::MatchPair)),
    ("motion.paragraph_forward", motion(Motion::ParaFwd)),
    ("motion.paragraph_backward", motion(Motion::ParaBack)),
    ("motion.screen_top", motion(Motion::ScreenTop)),
    ("motion.screen_middle", motion(Motion::ScreenMid)),
    ("motion.screen_bottom", motion(Motion::ScreenBot)),
    (
        "motion.half_page_down",
        motion(Motion::Page {
            down: true,
            half: true,
        }),
    ),
    (
        "motion.half_page_up",
        motion(Motion::Page {
            down: false,
            half: true,
        }),
    ),
    (
        "motion.page_down",
        motion(Motion::Page {
            down: true,
            half: false,
        }),
    ),
    (
        "motion.page_up",
        motion(Motion::Page {
            down: false,
            half: false,
        }),
    ),
    (
        "motion.search_next",
        motion(Motion::SearchNext { reverse: false }),
    ),
    (
        "motion.search_prev",
        motion(Motion::SearchNext { reverse: true }),
    ),
    (
        "motion.word_search_forward",
        motion(Motion::WordSearch { forward: true }),
    ),
    (
        "motion.word_search_backward",
        motion(Motion::WordSearch { forward: false }),
    ),
    ("motion.next_line", motion(Motion::NextLine)),
    ("motion.prev_line", motion(Motion::PrevLine)),
    ("motion.current_line", motion(Motion::CurLine)),
    ("motion.column", motion(Motion::Column)),
    // Marks: the next key names one.
    ("mark.set", Builtin::Await(Await::SetMark)),
    (
        "motion.mark_line",
        Builtin::Await(Await::Mark { linewise: true }),
    ),
    (
        "motion.mark",
        Builtin::Await(Await::Mark { linewise: false }),
    ),
    // What the syntax tree knows: functions, types, arguments, comments.
    ("motion.next_function", syntax('f', true)),
    ("motion.prev_function", syntax('f', false)),
    ("motion.next_type", syntax('t', true)),
    ("motion.prev_type", syntax('t', false)),
    ("motion.next_argument", syntax('a', true)),
    ("motion.prev_argument", syntax('a', false)),
    ("motion.next_comment", syntax('c', true)),
    ("motion.prev_comment", syntax('c', false)),
    // Operators: wait for a motion or text object, or act on the selection.
    ("operator.delete", op(Operator::Delete)),
    ("operator.change", op(Operator::Change)),
    ("operator.yank", op(Operator::Yank)),
    ("operator.indent", op(Operator::Indent)),
    ("operator.outdent", op(Operator::Outdent)),
    ("operator.lowercase", op(Operator::Lower)),
    ("operator.uppercase", op(Operator::Upper)),
    ("operator.toggle_case", op(Operator::ToggleCase)),
    ("operator.comment", op(Operator::Comment)),
    // Surround: the operator waits for a motion, then for the delimiter.
    ("surround.add", op(Operator::Surround)),
    ("surround.delete", Builtin::Await(Await::SurroundDelete)),
    ("surround.change", Builtin::Await(Await::SurroundChange)),
    ("surround.operator", cmd(Cmd::Surround)),
    // The next key names the object (`w`, `"`, `(`…) or the register.
    (
        "textobj.inner",
        Builtin::Await(Await::TextObject { around: false }),
    ),
    (
        "textobj.around",
        Builtin::Await(Await::TextObject { around: true }),
    ),
    ("register.select", Builtin::Await(Await::Register)),
    ("mode.normal", cmd(Cmd::Escape)),
    ("mode.insert", cmd(Cmd::Insert)),
    ("mode.append", cmd(Cmd::Append)),
    ("mode.insert_line_start", cmd(Cmd::InsertLineStart)),
    ("mode.append_line_end", cmd(Cmd::AppendLineEnd)),
    ("mode.visual", cmd(Cmd::Visual { linewise: false })),
    ("mode.visual_line", cmd(Cmd::Visual { linewise: true })),
    ("mode.visual_block", cmd(Cmd::VisualBlock)),
    ("edit.open_below", cmd(Cmd::OpenBelow)),
    ("edit.open_above", cmd(Cmd::OpenAbove)),
    ("edit.delete_char", cmd(Cmd::DeleteChar { before: false })),
    (
        "edit.delete_char_before",
        cmd(Cmd::DeleteChar { before: true }),
    ),
    ("edit.substitute_char", cmd(Cmd::SubstituteChar)),
    ("edit.replace_char", Builtin::Await(Await::Replace)),
    ("edit.delete_lines", cmd(Cmd::Lines(Operator::Delete))),
    ("edit.change_lines", cmd(Cmd::Lines(Operator::Change))),
    ("edit.yank_lines", cmd(Cmd::Lines(Operator::Yank))),
    (
        "edit.delete_to_line_end",
        cmd(Cmd::ToLineEnd(Operator::Delete)),
    ),
    (
        "edit.change_to_line_end",
        cmd(Cmd::ToLineEnd(Operator::Change)),
    ),
    ("edit.join_lines", cmd(Cmd::Join { space: true })),
    ("edit.join_lines_no_space", cmd(Cmd::Join { space: false })),
    ("edit.toggle_case_char", cmd(Cmd::ToggleCaseChar)),
    ("edit.put_after", cmd(Cmd::Put { before: false })),
    ("edit.put_before", cmd(Cmd::Put { before: true })),
    ("edit.repeat", cmd(Cmd::Repeat)),
    ("edit.undo", cmd(Cmd::Undo)),
    ("edit.redo", cmd(Cmd::Redo)),
    ("visual.swap_ends", cmd(Cmd::SwapEnds)),
    ("visual.reselect", cmd(Cmd::Reselect)),
    ("visual.insert", cmd(Cmd::BlockInsert { append: false })),
    ("visual.append", cmd(Cmd::BlockInsert { append: true })),
    ("select.expand", cmd(Cmd::Expand)),
    ("select.shrink", cmd(Cmd::Shrink)),
    ("macro.record", cmd(Cmd::Record)),
    ("macro.play", Builtin::Await(Await::PlayRegister)),
    ("cmdline.ex", cmd(Cmd::Prompt(':'))),
    ("cmdline.search_forward", cmd(Cmd::Prompt('/'))),
    ("cmdline.search_backward", cmd(Cmd::Prompt('?'))),
    ("view.center", Builtin::Scroll(Scroll::Center)),
    ("view.top", Builtin::Scroll(Scroll::Top)),
    ("view.bottom", Builtin::Scroll(Scroll::Bottom)),
    ("view.scroll_down", Builtin::Scroll(Scroll::Lines(1))),
    ("view.scroll_up", Builtin::Scroll(Scroll::Lines(-1))),
    // Insert mode. Keys bound to nothing insert themselves.
    ("insert.escape", insert(InsertCmd::Escape)),
    ("insert.newline", insert(InsertCmd::Newline)),
    ("insert.backspace", insert(InsertCmd::Backspace)),
    ("insert.delete", insert(InsertCmd::Delete)),
    ("insert.tab", insert(InsertCmd::Tab)),
    ("insert.delete_word", insert(InsertCmd::DeleteWord)),
    ("insert.delete_line", insert(InsertCmd::DeleteLine)),
    ("insert.indent", insert(InsertCmd::Indent)),
    ("insert.outdent", insert(InsertCmd::Outdent)),
    ("insert.left", insert(InsertCmd::Left)),
    ("insert.right", insert(InsertCmd::Right)),
    ("insert.up", insert(InsertCmd::Up)),
    ("insert.down", insert(InsertCmd::Down)),
    ("insert.page_up", insert(InsertCmd::PageUp)),
    ("insert.page_down", insert(InsertCmd::PageDown)),
    ("insert.home", insert(InsertCmd::Home)),
    ("insert.end", insert(InsertCmd::End)),
];

pub(crate) fn builtin(name: &str) -> Option<Builtin> {
    BUILTINS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, builtin)| *builtin)
}

pub(crate) fn name(builtin: Builtin) -> Option<&'static str> {
    BUILTINS
        .iter()
        .find(|(_, known)| *known == builtin)
        .map(|(name, _)| *name)
}

/// The name of every command the engine implements itself.
pub fn command_names() -> impl Iterator<Item = &'static str> {
    BUILTINS.iter().map(|(name, _)| *name)
}
