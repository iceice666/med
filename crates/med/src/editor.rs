//! The editor view: one buffer, its vim state, and everything needed to
//! draw it and feed it input.

use crate::commands::{Args, Commands, Handler};
use crate::lsp::{
    LspEvent, LspStore, Place, SemanticAnswer, SemanticTokens, ServerName, uri_to_path,
};
use crate::syntax::{LangInfo, Syntax};
use crate::{Settings, VimGlobal};
use anyhow::Context as _;
use gpui::component::ActiveTheme as _;
use gpui::component::text::{RangeHighlight, TextView, TextViewState};
use gpui::prelude::*;
use gpui::*;
use lsp_types as lsp;
use med_core::Buffer;
use med_git::{Hunk, HunkKind};
pub(crate) use med_vim::wrap_breaks;
use med_vim::{
    Clipboard, Ctx, Effect, FlashMatch, Hidden, Key, Mode, PendingKeys, Scroll, Viewport, Vim,
    cell_width,
};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

const COMPLETION_ROWS: usize = 10;

/// Lines above and below the view looked through for a blank line's
/// indent, and for the block the cursor is in.
const GUIDE_REACH: usize = 100;

/// The most first lines of scopes sticky scroll stands at the top.
const STICKY_MAX: usize = 5;

/// How long writing a file waits for the language server to format it.
const FORMAT_TIMEOUT: Duration = Duration::from_millis(800);

/// A diagnostic in buffer coordinates (char offsets).
#[derive(Clone, Debug)]
pub struct Diag {
    pub start: usize,
    pub end: usize,
    /// 1 = error … 4 = hint, as in LSP.
    pub severity: u8,
    pub message: String,
}

/// A mark a program put on the text: see `buffer_api.rs`.
#[derive(Clone)]
pub(crate) struct Decoration {
    /// Char offsets. For a line's decoration both are where it starts.
    pub start: usize,
    pub end: usize,
    pub whole_line: bool,
    /// A character beside the line's number.
    pub sign: Option<SharedString>,
    /// Words after the end of the line.
    pub text: Option<SharedString>,
    pub bg: Option<crate::panel::Color>,
    pub color: Option<crate::panel::Color>,
}

/// Keep `start..end` on its text through an edit.
/// What a language server says inline of the text, before the char at
/// `offset`: a type, a parameter's name.
#[derive(Clone, Debug, PartialEq)]
pub struct InlayHint {
    pub offset: usize,
    pub text: String,
}

/// A hint's label as it is shown: its parts, with the blanks it asks for
/// either side, on one line and not too long.
fn hint_text(hint: &lsp::InlayHint) -> String {
    let label = match &hint.label {
        lsp::InlayHintLabel::String(text) => text.clone(),
        lsp::InlayHintLabel::LabelParts(parts) => parts.iter().map(|p| p.value.as_str()).collect(),
    };
    let mut text: String = label.chars().filter(|c| !c.is_control()).take(80).collect();
    if hint.padding_left == Some(true) {
        text.insert(0, ' ');
    }
    if hint.padding_right == Some(true) {
        text.push(' ');
    }
    text
}

/// A fold's ends through an edit: after what it took out, moved by it;
/// in it, to the end of what it put in, so that a fold taken out whole
/// comes to nothing.
fn shift_fold(fold: &mut Range<usize>, edit: &med_core::Edit) {
    let shift = |offset: usize| {
        if offset >= edit.old_end_char {
            offset + edit.new_end_char - edit.old_end_char
        } else if offset <= edit.start_char {
            offset
        } else {
            edit.new_end_char
        }
    };
    *fold = shift(fold.start)..shift(fold.end);
}

fn shift_range(start: &mut usize, end: &mut usize, edit: &med_core::Edit) {
    let delta = edit.new_end_char as isize - edit.old_end_char as isize;
    let shift = |offset: usize| (offset as isize + delta).max(0) as usize;
    if *start >= edit.old_end_char {
        *start = shift(*start);
        *end = shift(*end);
    } else if *end >= edit.old_end_char {
        *end = shift(*end).max(*start);
    }
}

pub enum EditorEvent {
    /// An ex command for the workspace (`w`, `q`, `e foo`…).
    Ex(String),
    /// A key bound to a command that is not the editor's to run.
    Command {
        name: String,
        args: Args,
    },
    /// A key is about to be handled: what the status bar says about the
    /// last one is stale.
    KeyPress,
    /// What the keys typed so far wait for is no longer what it was:
    /// `None` when no command is half typed any more.
    Pending(Option<PendingKeys>),
    /// Title, dirty state, cursor or mode changed.
    Changed,
    OpenLocation {
        path: PathBuf,
        position: lsp::Position,
    },
    /// Places to choose among: the references to a name, its
    /// implementations, the symbols of the file. `what` they are, in the
    /// plural, for a message.
    Locations {
        what: &'static str,
        places: Vec<Place>,
    },
    /// Edits a language server asked for, in this file or in others.
    WorkspaceEdit(lsp::WorkspaceEdit),
    /// A program for a terminal to run, as a language server's command
    /// comes to: from `cwd`, or the project's root.
    Run {
        command: String,
        cwd: Option<PathBuf>,
    },
    /// The cursor is about to jump away from this offset.
    PushJump(usize),
    Message(String),
}

/// Geometry of the last paint, for hit-testing and popup placement.
#[derive(Clone)]
struct Metrics {
    text_origin: Point<Pixels>,
    text_width: Pixels,
    line_height: Pixels,
    char_width: Pixels,
    font_size: Pixels,
    font: Font,
}

/// A scope's first line standing at the top of the view.
struct StickyHeader {
    line: usize,
    /// The row of the view it stands for.
    row: usize,
    /// Where it was drawn, in window coordinates.
    bounds: Bounds<Pixels>,
}

struct CompletionMenu {
    items: Vec<lsp::CompletionItem>,
    /// For each item, the place of the server it came from among those
    /// asked: what is asked of the first comes first among equals.
    ranks: Vec<usize>,
    /// Indices into `items` that match what has been typed, best first.
    filtered: Vec<usize>,
    selected: usize,
    /// Start of the word being completed.
    start: usize,
}

/// A snippet put in, while its places are being filled in.
struct SnippetSession {
    /// Its places, in the order `Tab` goes through them, each with every
    /// range that has its text, the one typed in first; the last is where
    /// the cursor ends up.
    stops: Vec<Vec<Range<usize>>>,
    current: usize,
    /// The placeholder of the place the cursor is at is as the snippet
    /// put it: what is typed there first takes its place.
    fresh: bool,
}

impl SnippetSession {
    /// From the start of the first place to the end of the last.
    fn contains(&self, offset: usize) -> bool {
        let ranges = self.stops.iter().flatten();
        let start = ranges.clone().map(|r| r.start).min();
        let end = ranges.map(|r| r.end).max();
        matches!((start, end), (Some(start), Some(end)) if start <= offset && offset <= end)
    }
}

/// Keep a place of a snippet on its text through an edit. One of the place
/// being filled in (`active`) takes in what is typed at either end of it;
/// another moves on from what is typed at its start.
fn shift_place(range: &mut Range<usize>, edit: &med_core::Edit, active: bool) {
    let (start, old_end, new_end) = (edit.start_char, edit.old_end_char, edit.new_end_char);
    let moved =
        |offset: usize| (offset as isize + new_end as isize - old_end as isize).max(0) as usize;
    if active && start >= range.start && old_end <= range.end {
        range.end = moved(range.end);
        return;
    }
    let follow = |offset: usize| {
        if offset < start || (offset == start && start != old_end) {
            offset
        } else if offset >= old_end {
            moved(offset)
        } else {
            new_end
        }
    };
    // What is typed at its start goes before it.
    let at_start = start == old_end && start == range.start;
    let (from, to) = match at_start {
        true => (moved(range.start), moved(range.end)),
        false => (follow(range.start), follow(range.end)),
    };
    *range = from..to.max(from);
}

/// The code actions on offer, to choose one from.
struct ActionMenu {
    /// Each with the server that offered it, which is to run its command.
    actions: Vec<(Option<ServerName>, lsp::CodeActionOrCommand)>,
    selected: usize,
}

/// The signature of what is being called, with the argument the cursor
/// is at.
struct Signature {
    label: String,
    /// The bytes of `label` that are that argument's parameter.
    active: Option<Range<usize>>,
}

/// Replace what `edits` say in `buf`, as one step to undo, and say how
/// many changed anything and where a cursor at `cursor` ends up.
pub(crate) fn apply_edits(
    buf: &mut Buffer,
    edits: &[lsp::TextEdit],
    cursor: usize,
) -> (usize, usize) {
    let offset = |position: lsp::Position| {
        buf.utf16_to_offset(med_core::Point::new(
            position.line as usize,
            position.character as usize,
        ))
    };
    let mut resolved: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|edit| {
            let (start, end) = (offset(edit.range.start), offset(edit.range.end));
            (start, end.max(start), edit.new_text.as_str())
        })
        .collect();
    // From the end, so that no edit moves one still to be made.
    resolved.sort_by_key(|edit| std::cmp::Reverse((edit.0, edit.1)));

    let point = buf.offset_to_point(cursor);
    let (mut applied, mut shifted, mut inside) = (0, cursor as isize, false);
    buf.begin(cursor);
    for (start, end, text) in resolved {
        // A formatter may send the whole file back: only the part that
        // differs is an edit.
        let old: Vec<char> = buf.slice(start..end).chars().collect();
        let new: Vec<char> = text.chars().collect();
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let (start, end) = (start + prefix, end - suffix);
        let text: String = new[prefix..new.len() - suffix].iter().collect();
        if start == end && text.is_empty() {
            continue;
        }
        if end <= cursor {
            shifted += text.chars().count() as isize - (end - start) as isize;
        } else if start < cursor {
            inside = true;
        }
        buf.edit(start..end, &text);
        applied += 1;
    }
    buf.end();
    let cursor = if inside {
        // Its text was rewritten: the same line and column is the best guess.
        let row = point.row.min(buf.last_line());
        buf.line_start(row) + point.col.min(buf.line_len(row))
    } else {
        shifted.max(0) as usize
    };
    (applied, cursor.min(buf.max_offset()))
}

/// What to show of a server's answer about the call the cursor is in.
fn signature_of(help: lsp::SignatureHelp) -> Option<Signature> {
    let index = help.active_signature.unwrap_or(0) as usize;
    let signature = help.signatures.into_iter().nth(index)?;
    let parameter = signature.active_parameter.or(help.active_parameter);
    let parameter = parameter.and_then(|index| signature.parameters.as_ref()?.get(index as usize));
    let label = signature.label;
    let active = parameter.and_then(|parameter| match &parameter.label {
        lsp::ParameterLabel::Simple(text) => {
            let start = label.find(text.as_str())?;
            Some(start..start + text.len())
        }
        // Counted in UTF-16 units of the label.
        lsp::ParameterLabel::LabelOffsets([start, end]) => {
            let byte = |units: u32| {
                let mut seen = 0;
                for (byte, c) in label.char_indices() {
                    if seen >= units as usize {
                        return byte;
                    }
                    seen += c.len_utf16();
                }
                label.len()
            };
            Some(byte(*start)..byte(*end))
        }
    });
    Some(Signature { label, active })
}

pub struct Editor {
    focus_handle: FocusHandle,
    pub buf: Buffer,
    pub vim: Vim,
    pub path: Option<PathBuf>,
    pub lang: Option<&'static LangInfo>,
    vim_lang: med_vim::Lang,
    /// The tab width the file's language and indentation give; the
    /// `tab-width` option, the tab's own or every tab's, where set, is the
    /// one `vim_lang` has.
    file_tab_width: usize,
    /// This tab's own values of the options that can have one, over the
    /// ones `set-option` gives every tab.
    pub options: BufferOptions,
    syntax: Option<Syntax>,
    lsp: Option<Entity<LspStore>>,
    lsp_version: i32,
    /// First visible line; fractional while wheel-scrolling.
    scroll_top: f32,
    /// Wrapped, the row of that line the view starts at, and the wheel's
    /// scrolling not yet come to a whole row.
    top_row: usize,
    wheel_rows: f32,
    /// Cells to a row as the view was last laid out, where lines wrap.
    wrap_cells: Option<usize>,
    scroll_left: Pixels,
    visible_lines: usize,
    metrics: Option<Metrics>,
    /// The first lines of scopes standing at the top of the view, as it
    /// was last laid out, the outermost first: a click on one goes to it.
    sticky: Vec<StickyHeader>,
    /// The closed folds, each from the start of its first line to the end
    /// of its last, as char offsets that follow the text as it is edited;
    /// and the lines they hide.
    folds: Vec<Range<usize>>,
    hidden: Hidden,
    /// IME composition in progress, as a char range of the buffer.
    marked: Option<Range<usize>>,
    /// IME composition on the `:` line, not yet given to it: it goes in at
    /// the line's cursor.
    pub(crate) cmdline_preedit: Option<String>,
    /// Where the status bar last drew the `:` line's cursor, in window
    /// coordinates: where the input method puts its window.
    pub(crate) cmdline_caret: Option<Bounds<Pixels>>,
    diagnostics: Vec<Diag>,
    /// What the language server says inline of the text, in order; the
    /// version of the text they were last asked for, and the asking.
    inlay_hints: Vec<InlayHint>,
    hints_asked: Option<u64>,
    hints_task: Option<Task<()>>,
    /// Whether they are drawn, as the view was last laid out.
    show_hints: bool,
    /// The places of the name under the cursor, as the language server
    /// sees them, each with whether it is written there; the text's
    /// version and the cursor they were last asked for, and the asking.
    doc_highlights: Vec<(Range<usize>, bool)>,
    highlights_asked: Option<(u64, usize)>,
    highlights_task: Option<Task<()>>,
    /// The tokens the language server tells apart, each a char range and
    /// the highlight it is drawn as, in order; the numbers they were read
    /// from, as the server last sent them, and which of its answers that
    /// was; the version of the text they were last asked for, and the
    /// asking.
    semantic_spans: Vec<(Range<usize>, &'static str)>,
    semantic_data: Vec<u32>,
    semantic_result: Option<(ServerName, String)>,
    tokens_asked: Option<u64>,
    tokens_task: Option<Task<()>>,
    /// What programs marked on the text, by the group they gave.
    decorations: Vec<(SharedString, Vec<Decoration>)>,
    completion: Option<CompletionMenu>,
    completion_task: Option<Task<()>>,
    /// The snippet whose places are being filled in.
    snippet: Option<SnippetSession>,
    /// `Tab` going through what the `:` line could be.
    cmdline_completion: Option<crate::cmdline::Completion>,
    /// The project's root, which a file named on the command line is
    /// relative to; set by the workspace.
    pub(crate) root: Option<PathBuf>,
    key_timeout: Option<Task<()>>,
    /// What the keys typed so far waited for when that was last told.
    pending_keys: Option<PendingKeys>,
    hover: Option<SharedString>,
    request_task: Option<Task<()>>,
    menu: Option<ActionMenu>,
    signature: Option<Signature>,
    signature_task: Option<Task<()>>,
    drag_anchor: Option<usize>,
    mtime: Option<SystemTime>,
    /// The file as it is at HEAD, which the gutter marks the changes from.
    git_base: GitBase,
    /// The lines changed from it, the version of the text they are for,
    /// and the working out.
    git_hunks: Vec<Hunk>,
    git_diffed: Option<u64>,
    git_diff_task: Option<Task<()>>,
    /// A tab that a program made, with no file behind it.
    pub scratch: Option<Scratch>,
    /// A tab that shows a Markdown file rendered.
    pub preview: Option<Preview>,
    _subscriptions: Vec<Subscription>,
}

/// What a Markdown preview tab shows: another tab's file, rendered. Its own
/// text is that file's, as it is typed there, so that the keys move about
/// in it, but it is not to be edited here: what is, is put back.
pub struct Preview {
    /// The file shown, which tells one file's preview from another's.
    pub source_path: PathBuf,
    source: WeakEntity<Editor>,
    /// The text, as last taken from the file's tab.
    text: SharedString,
    /// The file's version that `text` is.
    source_version: u64,
    /// The version of this tab's text when it was last given `text`: any
    /// other is an edit made here.
    version: u64,
    view: Entity<TextViewState>,
    /// The line the view last followed the cursor to.
    line: Option<usize>,
    _subscriptions: Vec<Subscription>,
}

/// A tab of text that is no file: a program's, by the name it gave it.
pub struct Scratch {
    pub name: SharedString,
    /// What a program does with the text when it is written (`:w`); with
    /// nothing to do, the text is never unsaved.
    pub on_save: Option<med_lisp::Value>,
}

impl EventEmitter<EditorEvent> for Editor {}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

struct AppClipboard<'a>(&'a mut App);

impl Clipboard for AppClipboard<'_> {
    fn get(&mut self) -> Option<String> {
        self.0.read_from_clipboard().and_then(|item| item.text())
    }
    fn set(&mut self, text: &str) {
        self.0
            .write_to_clipboard(ClipboardItem::new_string(text.to_string()));
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Where undo histories are kept, if they are to be: the `undo-file`
/// option.
fn undo_dir(cx: &App) -> Option<PathBuf> {
    let settings = cx.try_global::<Settings>()?;
    settings
        .undo_file
        .then(|| settings.undo_dir.clone())
        .flatten()
}

/// A tab's own values of options, from `set-buffer-option`; `None` where
/// it goes by the one of every tab. Auto-pairs is the vim engine's,
/// [`Vim::auto_pairs`](med_vim::Vim::auto_pairs).
#[derive(Clone, Debug, Default)]
pub struct BufferOptions {
    pub tab_width: Option<usize>,
    pub relative_number: Option<bool>,
    pub format_on_save: Option<bool>,
    pub scroll_off: Option<usize>,
    pub cursor_line: Option<bool>,
    pub color_columns: Option<Vec<usize>>,
    pub wrap: Option<bool>,
    pub inlay_hints: Option<bool>,
    pub semantic_tokens: Option<bool>,
    pub indent_guides: Option<bool>,
    pub sticky_scroll: Option<bool>,
    pub git_gutter: Option<bool>,
}

/// What the file was at HEAD, as far as it is known.
enum GitBase {
    /// To be read: the file is new to the tab, or HEAD may have moved.
    Unread,
    /// Kept to be dropped, which gives up the reading.
    Reading {
        _task: Task<()>,
    },
    /// Nothing to compare with: no repository, or not committed.
    Missing,
    Text(Arc<str>),
}

/// The cells of a line's leading blanks, a tab to its stop; `None` for a
/// line that is nothing but blanks.
pub(crate) fn indent_cells(text: &str, tab_width: usize) -> Option<usize> {
    let mut cells = 0;
    for c in text.chars() {
        match c {
            ' ' | '\t' => cells += cell_width(c, cells, tab_width.max(1)),
            _ => return Some(cells),
        }
    }
    None
}

/// The indent of each line, a blank one taking the less of the nearest
/// lines either side that are not: a block's guides go on through it.
pub(crate) fn line_indents(indents: &[Option<usize>]) -> Vec<usize> {
    let mut before = vec![None; indents.len()];
    let mut last = None;
    for (i, indent) in indents.iter().enumerate() {
        last = indent.or(last);
        before[i] = last;
    }
    let mut next = None;
    let mut out = vec![0; indents.len()];
    for i in (0..indents.len()).rev() {
        next = indents[i].or(next);
        out[i] = match (indents[i], before[i], next) {
            (Some(indent), ..) => indent,
            (None, Some(a), Some(b)) => a.min(b),
            _ => 0,
        };
    }
    out
}

/// The guide of the block the line at `at` is in: its column, and the
/// first and last line of the block, the lines either side indented past
/// it. `None` for a line at the margin.
pub(crate) fn block_guide(
    indents: &[usize],
    at: usize,
    width: usize,
) -> Option<(usize, usize, usize)> {
    let width = width.max(1);
    let indent = *indents.get(at)?;
    if indent == 0 {
        return None;
    }
    let column = (indent - 1) / width * width;
    let inside = |i: usize| indents[i] > column;
    let first = (0..at)
        .rev()
        .take_while(|&i| inside(i))
        .last()
        .unwrap_or(at);
    let last = (at + 1..indents.len())
        .take_while(|&i| inside(i))
        .last()
        .unwrap_or(at);
    Some((column, first, last))
}

/// The scopes whose first lines stand at the top of the view, a row of it
/// each, the outermost first. `under(row)` is the first and the last line
/// that row of the view is over (two where the view starts part of a line
/// down), or `None` past the text; `scopes(line)` those a line is in, as
/// [`Syntax::scopes`]. A row's is the next scope in of the line under it,
/// and no row is put over the cursor's line.
pub(crate) fn sticky_scopes(
    max: usize,
    cursor_line: usize,
    under: impl Fn(usize) -> Option<(usize, usize)>,
    mut scopes: impl FnMut(usize) -> Vec<(usize, usize)>,
) -> Vec<(usize, usize)> {
    let mut sticky: Vec<(usize, usize)> = Vec::new();
    while sticky.len() < max {
        let row = sticky.len();
        let Some((first, last)) = under(row) else {
            break;
        };
        if (first..=last).contains(&cursor_line) {
            break;
        }
        let around = scopes(first);
        match around.get(row) {
            Some(&scope) if around[..row] == sticky[..] => sticky.push(scope),
            _ => break,
        }
    }
    sticky
}

/// Where the `color-column` columns are drawn: each a cell wide, down the
/// whole of the text, as far in as it is from the start of the line.
pub(crate) fn column_bands(
    columns: &[usize],
    text: Bounds<Pixels>,
    char_width: Pixels,
    scroll_left: Pixels,
) -> Vec<Bounds<Pixels>> {
    columns
        .iter()
        .map(|column| {
            let x = text.left() + char_width * column.saturating_sub(1) as f32 - scroll_left;
            Bounds::new(point(x, text.top()), size(char_width, text.size.height))
        })
        .filter(|band| band.right() > text.left() && band.left() < text.right())
        .collect()
}

/// What the git gutter marks at a line.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct GitMarks {
    /// The line was added, or changed.
    pub line: Option<HunkKind>,
    /// Lines were taken out just before it, or, it being the last, after.
    pub deleted_above: bool,
    pub deleted_below: bool,
}

/// What the git gutter marks at `line`, of `hunks` in order; `last` is
/// the last line of the text.
pub(crate) fn git_marks(hunks: &[Hunk], line: usize, last: usize) -> GitMarks {
    let mut marks = GitMarks::default();
    // The one that could hold the line is the last to start before it.
    let from = hunks
        .partition_point(|h| h.lines.start < line)
        .saturating_sub(1);
    for hunk in hunks[from..]
        .iter()
        .take_while(|h| h.lines.start <= line + 1)
    {
        match hunk.kind {
            HunkKind::Deleted if hunk.lines.start == line => marks.deleted_above = true,
            HunkKind::Deleted if hunk.lines.start > line && line == last => {
                marks.deleted_below = true
            }
            HunkKind::Deleted => {}
            kind if hunk.lines.contains(&line) => marks.line = Some(kind),
            _ => {}
        }
    }
    marks
}

/// What the gutter shows for a line, right-aligned to `digits`: its number,
/// or with `relative` how many lines it is from the cursor's, the count
/// `j` and `k` take to get there. The cursor's line keeps its number.
pub(crate) fn line_number(
    line: usize,
    cursor_line: usize,
    relative: bool,
    digits: usize,
) -> String {
    let number = if relative && line != cursor_line {
        line.abs_diff(cursor_line)
    } else {
        line + 1
    };
    format!("{number:>digits$}")
}

/// Guess (expand_tab, width) from how the file is already indented.
fn detect_indent(text: &str) -> Option<(bool, usize)> {
    let mut min_spaces: Option<usize> = None;
    for line in text.lines().take(400) {
        if line.starts_with('\t') {
            return Some((false, 4));
        }
        let spaces = line.len() - line.trim_start_matches(' ').len();
        if spaces >= 2 && spaces < line.len() {
            min_spaces = Some(min_spaces.map_or(spaces, |m| m.min(spaces)));
        }
    }
    min_spaces.map(|n| (true, n.min(8)))
}

/// Where a line's things go on screen: each of its rows starts at an x of
/// the line laid out on one row, and is drawn `line_height` below the
/// last. Unwrapped, one row starts where the view is scrolled to.
struct Fold {
    /// The x, in the line, each row starts at.
    starts: Vec<Pixels>,
    /// Where the text area starts, and where the line's first row is.
    left: Pixels,
    top: Pixels,
    line_height: Pixels,
}

impl Fold {
    fn rows(&self) -> usize {
        self.starts.len()
    }

    fn row_at(&self, x: Pixels) -> usize {
        self.starts
            .partition_point(|start| *start <= x)
            .saturating_sub(1)
    }

    fn row_top(&self, row: usize) -> Pixels {
        self.top + self.line_height * row as f32
    }

    /// The screen point of an x in the line.
    fn at(&self, x: Pixels) -> Point<Pixels> {
        let row = self.row_at(x);
        point(self.left + x - self.starts[row], self.row_top(row))
    }

    /// From one x of the line to another, as rectangles a row high: one
    /// on each row it goes over.
    fn spans(&self, from: Pixels, to: Pixels) -> Vec<Bounds<Pixels>> {
        let last = if to > from {
            self.row_at(to - px(0.01))
        } else {
            self.row_at(from)
        };
        (self.row_at(from)..=last)
            .map(|row| {
                let start = from.max(self.starts[row]);
                let end = match self.starts.get(row + 1) {
                    Some(next) => to.min(*next),
                    None => to,
                };
                let origin = point(self.left + start - self.starts[row], self.row_top(row));
                Bounds::new(origin, size((end - start).max(px(0.)), self.line_height))
            })
            .collect()
    }
}

/// Expand tabs for display. Returns the display text and, for every char
/// column (plus one past the end), its byte offset in that text.
fn expand_tabs(text: &str, tab_width: usize) -> (String, Vec<u32>) {
    let tab_width = tab_width.max(1);
    let mut display = String::with_capacity(text.len());
    let mut offsets = Vec::with_capacity(text.len() + 1);
    let mut col = 0;
    for c in text.chars() {
        offsets.push(display.len() as u32);
        if c == '\t' {
            let n = tab_width - col % tab_width;
            display.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            display.push(if c.is_control() { ' ' } else { c });
            col += 1;
        }
    }
    offsets.push(display.len() as u32);
    (display, offsets)
}

/// Put labels into a line's display text, between its characters. Each of
/// `labels` is the char column it stands before and the label, in the
/// order of the columns, and for one column in the order they are to
/// stand in. The `offsets` of [`expand_tabs`] then point past the labels
/// before a column; what comes back is how many bytes of them there are
/// before each.
fn insert_labels(display: &mut String, offsets: &mut [u32], labels: &[(usize, char)]) -> Vec<u32> {
    let mut before = vec![0; offsets.len()];
    // From the last one back: what is still to come stays where it was.
    for &(col, label) in labels.iter().rev() {
        let col = col.min(offsets.len() - 1);
        display.insert((offsets[col] - before[col]) as usize, label);
        let len = label.len_utf8() as u32;
        before[col] += len;
        for offset in &mut offsets[col..] {
            *offset += len;
        }
    }
    before
}

/// Put text into a line's display text, between its characters, as
/// [`insert_labels`] puts labels: before the column each is at, in order.
/// What comes back is how many bytes of it there are before each column.
fn insert_texts(display: &mut String, offsets: &mut [u32], texts: &[(usize, &str)]) -> Vec<u32> {
    let mut before = vec![0; offsets.len()];
    for &(col, text) in texts.iter().rev() {
        let col = col.min(offsets.len() - 1);
        display.insert_str((offsets[col] - before[col]) as usize, text);
        let len = text.len() as u32;
        before[col] += len;
        for offset in &mut offsets[col..] {
            *offset += len;
        }
    }
    before
}

/// What is closed as it is opened in the file at `path`: everything a
/// language has in pairs, which a `'` is not where it marks a lifetime or
/// quotes a form.
fn pairs_for(path: &Path) -> Vec<(char, char)> {
    let extension = path.extension().and_then(|ext| ext.to_str());
    let lone_quote = matches!(
        extension,
        Some("rs" | "lisp" | "el" | "clj" | "cljs" | "scm" | "rkt" | "ml" | "mli" | "hs")
    );
    let mut pairs = med_vim::PAIRS.to_vec();
    pairs.retain(|pair| !(lone_quote && pair.0 == '\''));
    pairs
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A keystroke as the vim engine's key. `None` for the ones that are not
/// its business: app shortcuts, and text that must go through the input
/// method.
pub(crate) fn translate_key(keystroke: &Keystroke, mode: Mode) -> Option<Key> {
    let m = &keystroke.modifiers;
    if m.platform || m.function {
        return None;
    }
    let key = keystroke.key.as_str();
    let named = match key {
        "escape" => Some(Key::Esc),
        "enter" => Some(Key::Enter),
        "backspace" => Some(Key::Backspace),
        "delete" => Some(Key::Delete),
        // Back through what the command line completes to, or the places
        // of a snippet.
        "tab" if m.shift && matches!(mode, Mode::Command | Mode::Insert) => Some(Key::BackTab),
        "tab" => Some(Key::Tab),
        "left" => Some(Key::Left),
        "right" => Some(Key::Right),
        "up" => Some(Key::Up),
        "down" => Some(Key::Down),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" => Some(Key::PageUp),
        "pagedown" => Some(Key::PageDown),
        _ => None,
    };
    if m.control && !m.alt {
        if key == "space" {
            return Some(Key::Ctrl(' '));
        }
        let mut chars = key.chars();
        return match (chars.next(), chars.next()) {
            (Some(c), None) => Some(Key::Ctrl(c.to_ascii_lowercase())),
            _ => named,
        };
    }
    if named.is_some() {
        return named;
    }
    // Printable keys: while inserting they must reach the input method, so
    // they are left for the text input handler.
    if m.alt || matches!(mode, Mode::Insert | Mode::Command) {
        return None;
    }
    let text = match &keystroke.key_char {
        Some(text) => text.clone(),
        None if key == "space" => " ".to_string(),
        None if m.shift => key.to_uppercase(),
        None => key.to_string(),
    };
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(Key::Char(c)),
        _ => None,
    }
}

fn hover_markdown(hover: lsp::Hover) -> String {
    fn marked(s: lsp::MarkedString) -> String {
        match s {
            lsp::MarkedString::String(s) => s,
            lsp::MarkedString::LanguageString(ls) => {
                format!("```{}\n{}\n```", ls.language, ls.value)
            }
        }
    }
    match hover.contents {
        lsp::HoverContents::Scalar(s) => marked(s),
        lsp::HoverContents::Array(items) => items
            .into_iter()
            .map(marked)
            .collect::<Vec<_>>()
            .join("\n\n"),
        lsp::HoverContents::Markup(markup) => markup.value,
    }
}

/// What every server said of a symbol, each apart, in the order they
/// were asked; none when none said anything.
fn hovers_markdown(hovers: Vec<lsp::Hover>) -> Option<String> {
    let parts: Vec<String> = hovers
        .into_iter()
        .map(hover_markdown)
        .filter(|text| !text.trim().is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n---\n\n"))
}

impl Editor {
    pub fn new(
        path: Option<PathBuf>,
        text: &str,
        lsp: Option<Entity<LspStore>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            buf: Buffer::new(text),
            vim: Vim::new(),
            path: None,
            lang: None,
            vim_lang: med_vim::Lang::default(),
            file_tab_width: med_vim::Lang::default().tab_width,
            options: BufferOptions::default(),
            syntax: None,
            lsp,
            lsp_version: 0,
            scroll_top: 0.,
            top_row: 0,
            wheel_rows: 0.,
            wrap_cells: None,
            scroll_left: px(0.),
            visible_lines: 0,
            metrics: None,
            sticky: Vec::new(),
            folds: Vec::new(),
            hidden: Hidden::default(),
            marked: None,
            cmdline_preedit: None,
            cmdline_caret: None,
            diagnostics: Vec::new(),
            inlay_hints: Vec::new(),
            hints_asked: None,
            hints_task: None,
            show_hints: false,
            doc_highlights: Vec::new(),
            highlights_asked: None,
            highlights_task: None,
            semantic_spans: Vec::new(),
            semantic_data: Vec::new(),
            semantic_result: None,
            tokens_asked: None,
            tokens_task: None,
            decorations: Vec::new(),
            completion: None,
            completion_task: None,
            snippet: None,
            cmdline_completion: None,
            root: None,
            key_timeout: None,
            pending_keys: None,
            hover: None,
            request_task: None,
            menu: None,
            signature: None,
            signature_task: None,
            drag_anchor: None,
            mtime: None,
            git_base: GitBase::Unread,
            git_hunks: Vec::new(),
            git_diffed: None,
            git_diff_task: None,
            scratch: None,
            preview: None,
            _subscriptions: Vec::new(),
        };
        if let Some(lsp) = this.lsp.clone() {
            this._subscriptions
                .push(cx.subscribe(&lsp, |this, store, event: &LspEvent, cx| {
                    match event {
                        LspEvent::Diagnostics(path)
                            if this.path.as_deref() == Some(path.as_path()) =>
                        {
                            let diagnostics = store.read(cx).diagnostics(path).to_vec();
                            this.set_diagnostics(&diagnostics);
                            // The server has been through the file: what it
                            // says inline may be more now.
                            this.hints_asked = None;
                            cx.notify();
                        }
                        LspEvent::InlayHintsRefresh => {
                            this.hints_asked = None;
                            cx.notify();
                        }
                        LspEvent::SemanticTokensRefresh => {
                            this.tokens_asked = None;
                            cx.notify();
                        }
                        _ => {}
                    }
                }));
        }
        if let Some(path) = path {
            if let Some(dir) = undo_dir(cx) {
                crate::undo_file::read(&dir, &path, &mut this.buf);
            }
            this.set_path(path, cx);
        }
        this
    }

    /// Announce the file to the language server again, as after the
    /// servers were restarted.
    pub fn reopen_in_lsp(&mut self, cx: &mut Context<Self>) {
        if let (Some(lsp), Some(lang), Some(path)) = (&self.lsp, self.lang, &self.path) {
            self.lsp_version = 0;
            self.diagnostics.clear();
            let text = self.buf.text();
            lsp.update(cx, |store, cx| store.open(path, lang, text, cx));
        }
    }

    /// Attach the buffer to a file: language, syntax tree, language server.
    pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let (Some(lsp), Some(old)) = (&self.lsp, &self.path) {
            lsp.update(cx, |store, cx| store.close(old, cx));
        }
        self.lang = crate::lsp_config::lang_for_path(&path, cx);
        self.syntax = self.lang.and_then(|lang| Syntax::new(lang, &self.buf));
        let text = self.buf.text();
        let detected = detect_indent(&text);
        self.file_tab_width = detected
            .map(|d| d.1)
            .or(self.lang.map(|l| l.tab_width))
            .unwrap_or(4);
        self.vim_lang = med_vim::Lang {
            tab_width: self.tab_width(cx),
            expand_tab: detected
                .map(|d| d.0)
                .or(self.lang.map(|l| l.expand_tab))
                .unwrap_or(true),
            line_comment: self.lang.and_then(|l| l.line_comment).map(str::to_string),
            pairs: pairs_for(&path),
            wrap: self.wrap_cells,
            hidden: self.hidden.clone(),
        };
        self.mtime = mtime(&path);
        self.diagnostics.clear();
        self.git_base = GitBase::Unread;
        self.git_hunks.clear();
        self.git_diffed = None;
        self.git_diff_task = None;
        if let (Some(lsp), Some(lang)) = (&self.lsp, self.lang) {
            self.lsp_version = 0;
            lsp.update(cx, |store, cx| store.open(&path, lang, text, cx));
        }
        self.path = Some(path);
        // Written under a name, a program's text is a file from then on,
        // and its keys are the program's no longer.
        if self.scratch.take().is_some() {
            self.vim.tables.clear();
        }
        self.preview = None;
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    pub fn title(&self) -> String {
        if let Some(scratch) = &self.scratch {
            return scratch.name.to_string();
        }
        match &self.path {
            Some(path) => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            None => "[No Name]".to_string(),
        }
    }

    /// The empty tab that stands in when no file is open: no file, no
    /// program's text, never typed in. The next file opened takes its place.
    pub fn is_pristine(&self) -> bool {
        self.path.is_none() && self.scratch.is_none() && self.buf.version() == 0
    }

    /// Whether there is anything to lose: a program's text that it does not
    /// write anywhere is never unsaved.
    pub fn is_dirty(&self) -> bool {
        let nowhere = self.scratch.as_ref().is_some_and(|s| s.on_save.is_none());
        self.buf.is_dirty() && !nowhere
    }

    /// Put `text` in place of what is there, as written: a program's
    /// scratch text, made again. Text that was never changed is replaced
    /// for good, so that there is no undoing back to none.
    pub fn replace_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.folds.clear();
        self.hidden = Hidden::default();
        if self.buf.version() == 0 {
            self.buf = Buffer::new(text);
        } else {
            self.buf.set_text(text);
            self.buf.mark_saved();
        }
        self.vim.set_cursor(self.vim.cursor, &self.buf);
        self.after_change(cx);
    }

    // ---- Markdown preview ----------------------------------------------

    /// A tab that shows `source`'s file, at `path`, as rendered Markdown,
    /// and follows it as it is typed.
    pub fn preview_of(source: &Entity<Editor>, path: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Editor::new(None, "", None, cx);
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        this.scratch = Some(Scratch {
            name: format!("{name} (preview)").into(),
            on_save: None,
        });
        let view = cx.new(|cx| {
            TextViewState::markdown("", cx)
                .scrollable(true)
                .selectable(false)
        });
        // Once the text is parsed, the view can find the cursor's line.
        let parsed = cx.observe(&view, |this, _, cx| this.follow_cursor_in_preview(cx));
        this.preview = Some(Preview {
            source_path: path,
            source: source.downgrade(),
            text: SharedString::default(),
            source_version: 0,
            version: this.buf.version(),
            view,
            line: None,
            _subscriptions: vec![parsed],
        });
        this.link_preview(source, cx);
        this
    }

    /// Follow `source` from now on: the preview's file, in a tab that may
    /// have been opened again since.
    pub fn link_preview(&mut self, source: &Entity<Editor>, cx: &mut Context<Self>) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        preview.source = source.downgrade();
        let changed = cx.observe(source, |this, source, cx| this.mirror_source(&source, cx));
        // The view's, then the file's.
        preview._subscriptions.truncate(1);
        preview._subscriptions.push(changed);
        // Whatever version it is at, take its text.
        preview.source_version = source.read(cx).buf.version().wrapping_add(1);
        self.mirror_source(source, cx);
    }

    /// The tab whose file this previews, while it is open.
    pub fn preview_source(&self) -> Option<Entity<Editor>> {
        self.preview.as_ref()?.source.upgrade()
    }

    /// Take the text of the file's tab, if it changed.
    fn mirror_source(&mut self, source: &Entity<Editor>, cx: &mut Context<Self>) {
        let buf = &source.read(cx).buf;
        let version = buf.version();
        if self
            .preview
            .as_ref()
            .is_none_or(|p| p.source_version == version)
        {
            return;
        }
        let text = SharedString::from(buf.text());
        let cursor = self.vim.cursor;
        self.buf = Buffer::new(&text);
        self.vim.set_cursor(cursor, &self.buf);
        let Some(preview) = &mut self.preview else {
            return;
        };
        preview.source_version = version;
        preview.version = self.buf.version();
        preview.text = text.clone();
        preview.line = None;
        preview.view.update(cx, |view, cx| view.set_text(&text, cx));
        self.after_change(cx);
    }

    /// A preview's text is its file's: an edit made in it is put back, and
    /// it does not stay in insert mode.
    fn keep_preview_text(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        let edited = self.buf.version() != preview.version;
        if !edited && self.vim.mode != Mode::Insert {
            return;
        }
        if edited {
            let cursor = self.vim.cursor;
            self.buf = Buffer::new(&preview.text);
            preview.version = self.buf.version();
            self.vim.set_cursor(cursor, &self.buf);
        }
        if self.vim.mode == Mode::Insert {
            self.vim.mode = Mode::Normal;
        }
        cx.emit(EditorEvent::Message(
            "A preview is not edited here: edit its file".into(),
        ));
    }

    /// Scroll a preview to what the cursor's line renders, and mark it.
    fn follow_cursor_in_preview(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = &self.preview else {
            return;
        };
        let line = self.buf.line_of(self.vim.cursor);
        if preview.line == Some(line) {
            return;
        }
        let view = preview.view.clone();
        let color = cx.theme().selection;
        let buf = &self.buf;
        let rope = buf.rope();
        let source_len = rope.len_bytes();
        let bytes = |line: usize| {
            rope.char_to_byte(buf.line_start(line))..rope.char_to_byte(buf.line_end(line))
        };
        // A blank line, a fence or a rule renders nothing of its own: the
        // nearest line that does, below and then above.
        let mut lines = (line..=buf.last_line()).chain((0..line).rev());
        let found = view.update(cx, |view, cx| {
            let rendered = view.rendered_text();
            // Not parsed yet: the view says when it is.
            if rendered.source().len() != source_len {
                return false;
            }
            let Some(range) = lines.find_map(|line| rendered.range_for_source(bytes(line))) else {
                return false;
            };
            view.reveal_range(range.clone(), cx).ok();
            view.set_range_highlights([RangeHighlight::new(range, color)], cx)
                .ok();
            true
        });
        if found && let Some(preview) = &mut self.preview {
            preview.line = Some(line);
        }
    }

    fn render_preview(
        &self,
        preview: &Preview,
        key_context: KeyContext,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let focus_handle = self.focus_handle.clone();
        div()
            .id("editor")
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .track_focus(&self.focus_handle)
            .key_context(key_context)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.focus_handle.focus(window, cx);
                }),
            )
            // What is typed on the `:` line comes as text input.
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus_handle,
                            ElementInputHandler::new(bounds, entity),
                            cx,
                        );
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(
                div().size_full().px_6().py_2().child(
                    TextView::new(&preview.view)
                        .scrollable(true)
                        .selectable(false)
                        .size_full(),
                ),
            )
            .into_any_element()
    }

    pub fn language_name(&self) -> &'static str {
        self.lang.map_or("text", |l| l.name)
    }

    /// (line, column), both 1-based, for the status bar.
    pub fn cursor_position(&self) -> (usize, usize) {
        let point = self.buf.offset_to_point(self.vim.cursor);
        (point.row + 1, point.col + 1)
    }

    /// The most severe diagnostic on the cursor's line, if any.
    pub fn diagnostic_at_cursor(&self) -> Option<&Diag> {
        let line = self.buf.line_of(self.vim.cursor);
        let (ls, le) = (self.buf.line_start(line), self.buf.line_end(line));
        self.diagnostics
            .iter()
            .filter(|d| d.start <= le && d.end >= ls)
            .min_by_key(|d| d.severity)
    }

    /// The first line on screen, and how many there are room for.
    #[cfg(test)]
    pub(crate) fn view(&self) -> (usize, usize) {
        (self.scroll_top as usize, self.visible_lines)
    }

    /// Wrapped: cells to a row, the line and row the view starts at, and
    /// how many rows down it the cursor is, if it is on screen.
    #[cfg(test)]
    pub(crate) fn wrapped_view(&self) -> (Option<usize>, (usize, usize), Option<usize>) {
        let cursor = self.place_of(self.vim.cursor);
        let rows = self.rows_between(self.top(), cursor, self.visible_lines.max(1) - 1);
        (self.wrap_cells, self.top(), rows)
    }

    /// The lines on screen, for the engine's `H`, `L`, paging and flash.
    /// Wrapped, the lines wholly on screen, or the one there is: a line
    /// is a row at least, so the engine's margin in lines keeps the
    /// view's in rows, and the first and last line of the file are the
    /// view's edge only where it shows all of them.
    pub(crate) fn viewport(&self, cx: &App) -> Viewport {
        let scroll_off = self.scroll_margin(cx);
        if !self.by_rows() {
            return Viewport {
                top: self.scroll_top.max(0.) as usize,
                height: self.visible_lines.max(1),
                scroll_off,
            };
        }
        let (top, bottom) = (self.top(), self.bottom());
        let first = if top.1 == 0 { top.0 } else { top.0 + 1 };
        let whole = bottom.1 + 1 == self.rows(bottom.0);
        let last = if whole {
            bottom.0
        } else {
            bottom.0.saturating_sub(1)
        };
        let (first, last) = if first <= last {
            (first, last)
        } else {
            (top.0, top.0)
        };
        Viewport {
            top: first,
            height: last - first + 1,
            scroll_off,
        }
    }

    /// Lines between the cursor and the edges of the view: the
    /// `scroll-off` option, no more than fit with the cursor between.
    fn scroll_margin(&self, cx: &App) -> usize {
        let wanted = self.options.scroll_off.unwrap_or_else(|| {
            cx.try_global::<Settings>()
                .map_or(Settings::SCROLL_OFF, |s| s.scroll_off)
        });
        wanted.min(self.visible_lines.max(1).saturating_sub(1) / 2)
    }

    /// Columns to an indent: this tab's `tab-width`, else every tab's,
    /// else what the file's language and indentation say.
    fn tab_width(&self, cx: &App) -> usize {
        self.options
            .tab_width
            .or_else(|| cx.try_global::<Settings>().and_then(|s| s.tab_width))
            .unwrap_or(self.file_tab_width)
    }

    pub(crate) fn format_on_save(&self, cx: &App) -> bool {
        self.options.format_on_save.unwrap_or_else(|| {
            cx.try_global::<Settings>()
                .is_some_and(|s| s.format_on_save)
        })
    }

    pub(crate) fn cursor_line(&self, cx: &App) -> bool {
        self.options
            .cursor_line
            .unwrap_or_else(|| cx.try_global::<Settings>().is_none_or(|s| s.cursor_line))
    }

    pub(crate) fn color_columns(&self, cx: &App) -> Vec<usize> {
        match &self.options.color_columns {
            Some(columns) => columns.clone(),
            None => cx
                .try_global::<Settings>()
                .map(|s| s.color_columns.clone())
                .unwrap_or_default(),
        }
    }

    pub(crate) fn indent_guides(&self, cx: &App) -> bool {
        self.options
            .indent_guides
            .unwrap_or_else(|| cx.try_global::<Settings>().is_some_and(|s| s.indent_guides))
    }

    pub(crate) fn git_gutter(&self, cx: &App) -> bool {
        self.options
            .git_gutter
            .unwrap_or_else(|| cx.try_global::<Settings>().is_some_and(|s| s.git_gutter))
    }

    pub(crate) fn sticky_scroll(&self, cx: &App) -> bool {
        self.options
            .sticky_scroll
            .unwrap_or_else(|| cx.try_global::<Settings>().is_some_and(|s| s.sticky_scroll))
    }

    pub(crate) fn wrap(&self, cx: &App) -> bool {
        self.options
            .wrap
            .unwrap_or_else(|| cx.try_global::<Settings>().is_some_and(|s| s.wrap))
    }

    pub(crate) fn relative_number(&self, cx: &App) -> bool {
        self.options.relative_number.unwrap_or_else(|| {
            cx.try_global::<Settings>()
                .is_some_and(|s| s.relative_number)
        })
    }

    pub fn save(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let path = self.path.clone().context("No file name")?;
        if self.format_on_save(cx) {
            self.format_now(cx);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, self.buf.to_disk_string())
            .with_context(|| format!("Cannot write {}", path.display()))?;
        self.buf.mark_saved();
        self.mtime = mtime(&path);
        if let Some(dir) = undo_dir(cx) {
            crate::undo_file::write(&dir, &path, &self.buf);
        }
        if let Some(lsp) = &self.lsp {
            lsp.update(cx, |store, _| store.save(&path));
        }
        cx.emit(EditorEvent::Changed);
        cx.notify();
        Ok(())
    }

    /// Pick up a change made on disk, unless there are unsaved edits.
    pub fn reload_if_changed(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let disk = mtime(&path);
        if disk.is_none() || disk == self.mtime || self.buf.is_dirty() {
            return;
        }
        self.mtime = disk;
        if let Ok(text) = std::fs::read_to_string(&path) {
            self.folds.clear();
            self.hidden = Hidden::default();
            self.buf.set_text(&text);
            self.buf.mark_saved();
            self.vim.set_cursor(self.vim.cursor, &self.buf);
            self.after_change(cx);
        }
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if let (Some(lsp), Some(path)) = (&self.lsp, &self.path) {
            lsp.update(cx, |store, cx| store.close(path, cx));
        }
    }

    // ---- positions -----------------------------------------------------

    fn lsp_position(&self, offset: usize) -> lsp::Position {
        let point = self.buf.offset_to_utf16(offset);
        lsp::Position::new(point.row as u32, point.col as u32)
    }

    fn offset_from_lsp(&self, position: lsp::Position) -> usize {
        self.buf.utf16_to_offset(med_core::Point::new(
            position.line as usize,
            position.character as usize,
        ))
    }

    /// Move the cursor to an LSP position and center it.
    pub fn goto_position(&mut self, position: lsp::Position, cx: &mut Context<Self>) {
        let offset = self.offset_from_lsp(position);
        self.goto_offset(offset, cx);
    }

    /// Move the cursor to a 0-based line and char column and center it.
    pub fn goto_line_col(&mut self, line: usize, col: usize, cx: &mut Context<Self>) {
        let offset = self.buf.point_to_offset(med_core::Point::new(line, col));
        self.goto_offset(offset, cx);
    }

    pub fn goto_offset(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.vim.set_cursor(offset, &self.buf);
        self.center_cursor();
        self.report_pending(cx);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    fn center_cursor(&mut self) {
        let half = self.visible_lines.max(20) / 2;
        if self.by_rows() {
            self.set_top(self.rows_back(self.place_of(self.vim.cursor), half));
            return;
        }
        let line = self.buf.line_of(self.vim.cursor);
        self.scroll_top = line.saturating_sub(half) as f32;
    }

    // ---- git gutter ----------------------------------------------------

    /// Read again what the file is at HEAD, which a commit, a checkout or
    /// a pull may have changed. The marks stay as they are until it is.
    pub fn reread_git_base(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.git_base, GitBase::Unread) {
            self.git_base = GitBase::Unread;
            cx.notify();
        }
    }

    /// Work out which lines are not as at HEAD, once the text has been
    /// still a moment, if that is not known for this version of it.
    /// Drawing asks.
    pub fn request_git_diff(&mut self, cx: &mut Context<Self>) {
        let file = self.scratch.is_none() && self.preview.is_none();
        let Some(path) = self.path.clone().filter(|_| file && self.git_gutter(cx)) else {
            return;
        };
        let base = match &self.git_base {
            GitBase::Unread => {
                let task = cx.spawn(async move |this, cx| {
                    let base = cx
                        .background_executor()
                        .spawn(async move { med_git::committed_text(&path) })
                        .await;
                    this.update(cx, |this, cx| this.set_git_base(base.as_deref(), cx))
                        .ok();
                });
                self.git_base = GitBase::Reading { _task: task };
                return;
            }
            GitBase::Reading { .. } => return,
            GitBase::Missing => return,
            GitBase::Text(base) => base.clone(),
        };
        if self.git_diff_task.is_some() || self.git_diffed == Some(self.buf.version()) {
            return;
        }
        self.git_diffed = Some(self.buf.version());
        self.git_diff_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            let Ok((version, rope)) =
                this.update(cx, |this, _| (this.buf.version(), this.buf.rope().clone()))
            else {
                return;
            };
            let hunks = cx
                .background_executor()
                .spawn(async move {
                    // A file so changed that the diff takes long is marked
                    // more roughly.
                    let deadline = Instant::now() + Duration::from_millis(200);
                    med_git::line_hunks(&base, &rope.to_string(), Some(deadline))
                })
                .await;
            this.update(cx, |this, cx| {
                this.git_diff_task = None;
                if this.buf.version() == version {
                    this.git_hunks = hunks;
                    this.git_diffed = Some(version);
                } else {
                    // Typed in meanwhile: again, for the text as it is now.
                    this.git_diffed = None;
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Take what the file is at HEAD (`None`: nothing to compare with),
    /// and mark the lines again by it.
    pub(crate) fn set_git_base(&mut self, base: Option<&str>, cx: &mut Context<Self>) {
        self.git_base = match base {
            Some(text) => GitBase::Text(text.into()),
            None => GitBase::Missing,
        };
        if base.is_none() {
            self.git_hunks.clear();
        }
        self.git_diffed = None;
        self.git_diff_task = None;
        cx.notify();
    }

    /// Go to the first line of the next change from HEAD, or the
    /// previous, around the ends of the file; there, to its first
    /// character that is not a blank. Says which of how many it is.
    fn jump_to_hunk(&mut self, forward: bool, cx: &mut Context<Self>) {
        let hunks: &[Hunk] = match self.git_gutter(cx) {
            true => &self.git_hunks,
            false => &[],
        };
        let line = self.buf.line_of(self.vim.cursor);
        let index = if forward {
            hunks
                .iter()
                .position(|h| h.lines.start > line)
                .or((!hunks.is_empty()).then_some(0))
        } else {
            hunks
                .iter()
                .rposition(|h| h.lines.start < line)
                .or(hunks.len().checked_sub(1))
        };
        let Some(index) = index else {
            cx.emit(EditorEvent::Message("No changes from HEAD".into()));
            return;
        };
        // Lines taken out after the last are marked on the last.
        let to = hunks[index].lines.start.min(self.buf.last_line());
        let count = hunks.len();
        let indent = self
            .buf
            .line_string(to)
            .chars()
            .take_while(|c| c.is_whitespace() && *c != '\n')
            .count();
        let offset = self.offset_at_place(to, indent);
        self.vim.set_cursor(offset, &self.buf);
        let message = format!("Change {} of {count}", index + 1);
        cx.emit(EditorEvent::Message(message));
        cx.notify();
    }

    /// The lines changed from HEAD as last worked out, for programs and
    /// tests.
    pub fn git_hunks(&self) -> &[Hunk] {
        &self.git_hunks
    }

    // ---- inlay hints ---------------------------------------------------

    pub(crate) fn inlay_hints_on(&self, cx: &App) -> bool {
        self.options
            .inlay_hints
            .unwrap_or_else(|| cx.try_global::<Settings>().is_some_and(|s| s.inlay_hints))
    }

    /// Ask the server what it says inline, once the text has been still a
    /// moment, if it is not known for this version of it. Drawing asks;
    /// so can whatever has an editor that is not drawn.
    pub fn request_hints(&mut self, cx: &mut Context<Self>) {
        // Off, they are not drawn nor asked for, but kept for when it is on.
        if !self.inlay_hints_on(cx) {
            return;
        }
        let version = self.buf.version();
        if self.hints_task.is_some() || self.hints_asked == Some(version) || self.lsp.is_none() {
            return;
        }
        self.hints_asked = Some(version);
        self.hints_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let request = this.update(cx, |this, cx| {
                let (lsp, path) = (this.lsp.as_ref()?, this.path.as_ref()?);
                if this.buf.version() != version {
                    return None;
                }
                let end = this.lsp_position(this.buf.max_offset());
                let range = lsp::Range::new(lsp::Position::new(0, 0), end);
                lsp.read(cx).inlay_hints(path, range)
            });
            let hints = match request.ok().flatten() {
                Some(request) => request.await.ok().flatten(),
                None => None,
            };
            this.update(cx, |this, cx| {
                this.hints_task = None;
                match hints {
                    Some(hints) => {
                        if this.apply_inlay_hints(version, &hints) {
                            cx.notify();
                        }
                    }
                    // No server to ask yet, or no answer: ask again later.
                    None => this.hints_asked = None,
                }
            })
            .ok();
        }));
    }

    /// Take the hints a server gave for `version` of the text, unless the
    /// text has changed since: theirs would be in the wrong places.
    pub(crate) fn apply_inlay_hints(&mut self, version: u64, hints: &[lsp::InlayHint]) -> bool {
        if self.buf.version() != version {
            return false;
        }
        let mut hints: Vec<InlayHint> = hints
            .iter()
            .map(|hint| InlayHint {
                offset: self.offset_from_lsp(hint.position),
                text: hint_text(hint),
            })
            .filter(|hint| !hint.text.is_empty())
            .collect();
        hints.sort_by_key(|hint| hint.offset);
        self.inlay_hints = hints;
        true
    }

    #[cfg(test)]
    pub(crate) fn set_inlay_hints(&mut self, hints: &[(usize, &str)]) {
        self.inlay_hints = hints
            .iter()
            .map(|&(offset, text)| InlayHint {
                offset,
                text: text.to_string(),
            })
            .collect();
        self.hints_asked = Some(self.buf.version());
    }

    pub fn inlay_hint_texts(&self) -> Vec<String> {
        self.inlay_hints
            .iter()
            .map(|hint| hint.text.clone())
            .collect()
    }

    // ---- the tokens the language server tells apart ----------------------

    pub(crate) fn semantic_tokens_on(&self, cx: &App) -> bool {
        self.options.semantic_tokens.unwrap_or_else(|| {
            cx.try_global::<Settings>()
                .is_none_or(|s| s.semantic_tokens)
        })
    }

    /// Ask the server what each token of the text is, once the text has
    /// been still a moment, if it is not known for this version of it:
    /// only what changed, when the server can say so.
    pub fn request_tokens(&mut self, cx: &mut Context<Self>) {
        if !self.semantic_tokens_on(cx) {
            self.semantic_spans.clear();
            return;
        }
        let version = self.buf.version();
        if self.tokens_task.is_some() || self.tokens_asked == Some(version) || self.lsp.is_none() {
            return;
        }
        self.tokens_asked = Some(version);
        self.tokens_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let request = this.update(cx, |this, cx| {
                if this.buf.version() != version {
                    return None;
                }
                let (lsp, path) = (this.lsp.as_ref()?, this.path.as_ref()?);
                let previous = this
                    .semantic_result
                    .as_ref()
                    .map(|(server, id)| (&**server, id.as_str()));
                lsp.read(cx).semantic_tokens(path, previous)
            });
            // Changed meanwhile, or no server to ask yet: later, still
            // only what changed since the last answer.
            let Some(request) = request.ok().flatten() else {
                this.update(cx, |this, _| {
                    this.tokens_task = None;
                    this.tokens_asked = None;
                })
                .ok();
                return;
            };
            let answer = request.await.ok();
            this.update(cx, |this, cx| {
                this.tokens_task = None;
                match answer {
                    Some(answer) => {
                        if this.take_tokens(version, answer) {
                            cx.notify();
                        }
                    }
                    // No answer: all of them, later.
                    None => {
                        this.semantic_result = None;
                        this.tokens_asked = None;
                    }
                }
            })
            .ok();
        }));
    }

    /// Take a server's answer about the tokens of `version` of the text.
    /// What it sent is kept whatever the text is now, as what it says
    /// next is of that; where the tokens are is only worked out if the
    /// text is still that version.
    pub(crate) fn take_tokens(&mut self, version: u64, answer: SemanticAnswer) -> bool {
        let SemanticAnswer {
            server,
            legend,
            tokens,
        } = answer;
        let result_id = match tokens {
            SemanticTokens::All { result_id, data } => {
                self.semantic_data = data;
                result_id
            }
            SemanticTokens::Changes { result_id, edits } => {
                crate::lsp::apply_token_edits(&mut self.semantic_data, &edits);
                result_id
            }
            SemanticTokens::None => {
                self.semantic_result = None;
                self.tokens_asked = None;
                return false;
            }
        };
        self.semantic_result = result_id.map(|id| (server, id));
        if self.buf.version() != version {
            return false;
        }
        let tokens = crate::lsp::decode_tokens(&self.semantic_data);
        self.semantic_spans = tokens
            .iter()
            .filter_map(|token| {
                let name = crate::lsp::token_highlight(&legend, token)?;
                let at = |character| lsp::Position {
                    line: token.line,
                    character,
                };
                let start = self.offset_from_lsp(at(token.start));
                let end = self.offset_from_lsp(at(token.start + token.length));
                (start < end).then_some((start..end, name))
            })
            .collect();
        true
    }

    /// The tokens the server told apart, each as its text and the
    /// highlight it is drawn as.
    pub fn semantic_token_texts(&self) -> Vec<(String, &'static str)> {
        self.semantic_spans
            .iter()
            .map(|(range, name)| (self.buf.slice_string(range.clone()), *name))
            .collect()
    }

    // ---- the places of the name under the cursor -------------------------

    /// Ask the server where else the name under the cursor is, once the
    /// cursor has been still a moment, unless that is known. What was
    /// found goes as soon as the text changes or the cursor leaves it.
    pub fn request_highlights(&mut self, cx: &mut Context<Self>) {
        let key = (self.buf.version(), self.vim.cursor);
        if self.highlights_asked == Some(key) {
            return;
        }
        let cursor = self.vim.cursor;
        let same_text = self.highlights_asked.map(|(version, _)| version) == Some(key.0);
        let on_one = self
            .doc_highlights
            .iter()
            .any(|(range, _)| range.contains(&cursor));
        if !(same_text && on_one) {
            self.doc_highlights.clear();
        }
        self.highlights_asked = Some(key);
        self.highlights_task = None;
        if self.lsp.is_none() || self.path.is_none() || self.vim.mode == Mode::Insert {
            return;
        }
        self.highlights_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            let request = this.update(cx, |this, cx| {
                if (this.buf.version(), this.vim.cursor) != key {
                    return None;
                }
                let (lsp, path) = (this.lsp.as_ref()?, this.path.as_ref()?);
                lsp.read(cx)
                    .document_highlight(path, this.lsp_position(key.1))
            });
            let Some(request) = request.ok().flatten() else {
                return;
            };
            let found = request.await.ok().flatten().unwrap_or_default();
            this.update(cx, |this, cx| {
                if this.apply_document_highlights(key, &found) {
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Take the places a server gave for the cursor at `key.1` in version
    /// `key.0` of the text, unless either has changed since.
    pub(crate) fn apply_document_highlights(
        &mut self,
        key: (u64, usize),
        found: &[lsp::DocumentHighlight],
    ) -> bool {
        if (self.buf.version(), self.vim.cursor) != key {
            return false;
        }
        self.doc_highlights = found
            .iter()
            .map(|highlight| {
                let start = self.offset_from_lsp(highlight.range.start);
                let end = self.offset_from_lsp(highlight.range.end);
                let write = highlight.kind == Some(lsp::DocumentHighlightKind::WRITE);
                (start..end, write)
            })
            .filter(|(range, _)| range.start < range.end)
            .collect();
        true
    }

    /// The places of the name under the cursor that are marked, and
    /// whether each is written there.
    pub fn document_highlights(&self) -> &[(Range<usize>, bool)] {
        &self.doc_highlights
    }

    /// The hints drawn between `from` and `to`, both offsets of one line.
    fn hints_between(&self, from: usize, to: usize) -> &[InlayHint] {
        if !self.show_hints {
            return &[];
        }
        let start = self.inlay_hints.partition_point(|hint| hint.offset < from);
        let end = self.inlay_hints.partition_point(|hint| hint.offset <= to);
        &self.inlay_hints[start..end.max(start)]
    }

    /// A line as it is drawn: its tabs laid out, and the hints in it. The
    /// offsets of each char in that, and how many bytes of hints are
    /// before each.
    fn display_line(&self, line: usize) -> (String, Vec<u32>, Vec<u32>) {
        let (mut display, mut offsets) =
            expand_tabs(&self.buf.line_string(line), self.vim_lang.tab_width);
        let ls = self.buf.line_start(line);
        let hints: Vec<(usize, &str)> = self
            .hints_between(ls, ls + offsets.len() - 1)
            .iter()
            .map(|hint| (hint.offset - ls, hint.text.as_str()))
            .collect();
        let hint_bytes = insert_texts(&mut display, &mut offsets, &hints);
        (display, offsets, hint_bytes)
    }

    fn set_diagnostics(&mut self, diagnostics: &[lsp::Diagnostic]) {
        self.diagnostics = diagnostics
            .iter()
            .map(|d| Diag {
                start: self.offset_from_lsp(d.range.start),
                end: self.offset_from_lsp(d.range.end),
                severity: match d.severity {
                    Some(lsp::DiagnosticSeverity::ERROR) | None => 1,
                    Some(lsp::DiagnosticSeverity::WARNING) => 2,
                    Some(lsp::DiagnosticSeverity::INFORMATION) => 3,
                    _ => 4,
                },
                message: d.message.clone(),
            })
            .collect();
        self.diagnostics.sort_by_key(|d| d.start);
    }

    // ---- vim plumbing --------------------------------------------------

    fn with_vim<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Vim, &mut Ctx<'_>) -> R,
    ) -> R {
        let viewport = self.viewport(cx);
        self.vim_lang.tab_width = self.tab_width(cx);
        self.vim_lang.wrap = self.wrap_cells;
        self.vim_lang.hidden = self.hidden.clone();
        let (vim, buf, lang) = (&mut self.vim, &mut self.buf, &self.vim_lang);
        let syntax = &mut self.syntax;
        cx.update_global::<VimGlobal, R>(|global, cx| {
            let mut clipboard = AppClipboard(cx);
            let mut ctx = Ctx {
                buf,
                shared: &mut global.0,
                clipboard: &mut clipboard,
                viewport,
                lang,
                syntax: syntax
                    .as_mut()
                    .map(|syntax| syntax as &mut dyn med_vim::SyntaxTree),
            };
            f(vim, &mut ctx)
        })
    }

    /// Run one key through the vim engine and apply what comes out.
    pub fn handle_key(&mut self, key: Key, cx: &mut Context<Self>) {
        self.hover = None;
        cx.emit(EditorEvent::KeyPress);
        let effects = self.with_vim(cx, |vim, ctx| vim.handle_key(key, ctx));
        self.apply_effects(effects, cx);
        self.after_change(cx);
        self.refresh_completion();
        self.arm_key_timeout(cx);
    }

    /// Say so when what the keys typed so far wait for has changed.
    fn report_pending(&mut self, cx: &mut Context<Self>) {
        let pending = self.vim.pending_keys();
        if pending != self.pending_keys {
            self.pending_keys = pending.clone();
            cx.emit(EditorEvent::Pending(pending));
        }
    }

    /// While the keys typed could still grow into a longer binding, give
    /// the next key a moment to arrive, then settle for what there is.
    /// With no `key-timeout`, they wait for the next key instead.
    fn arm_key_timeout(&mut self, cx: &mut Context<Self>) {
        // Headless, there may be no settings.
        let timeout = match cx.try_global::<Settings>() {
            Some(settings) => settings.key_timeout,
            None => Some(Settings::KEY_TIMEOUT),
        };
        let wanted = self.vim.wants_timeout(&cx.global::<VimGlobal>().0);
        let Some(timeout) = timeout.filter(|_| wanted) else {
            self.key_timeout = None;
            return;
        };
        let timer = cx.background_executor().timer(timeout);
        self.key_timeout = Some(cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |this, cx| {
                let effects = this.with_vim(cx, |vim, ctx| vim.timeout(ctx));
                this.apply_effects(effects, cx);
                this.after_change(cx);
                this.refresh_completion();
            })
            .ok();
        }));
    }

    pub fn feed_keys(&mut self, keys: impl IntoIterator<Item = Key>, cx: &mut Context<Self>) {
        for key in keys {
            self.handle_key(key, cx);
        }
    }

    /// Text typed by the user, including input-method commits.
    pub fn type_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.type_over_placeholder(None, cx);
        self.hover = None;
        cx.emit(EditorEvent::KeyPress);
        let effects = self.with_vim(cx, |vim, ctx| vim.type_text(text, ctx));
        self.apply_effects(effects, cx);
        self.after_change(cx);
        self.arm_key_timeout(cx);
        if self.vim.mode != Mode::Insert {
            self.completion = None;
            return;
        }
        let Some(last) = text.chars().last() else {
            return;
        };
        let is_trigger = match (&self.lsp, &self.path) {
            (Some(lsp), Some(path)) => lsp.read(cx).triggers_completion(path, last),
            _ => false,
        };
        let has_signature = match (&self.lsp, &self.path) {
            (Some(lsp), Some(path)) => lsp.read(cx).signature_triggers(path).contains(&last),
            _ => false,
        };
        // Once it is up it follows the cursor from argument to argument.
        if has_signature || self.signature.is_some() {
            self.request_signature(cx);
        }
        if is_trigger {
            // What was on offer before it is for another place.
            self.completion = None;
            self.request_completion(Some(last), cx);
        } else if is_ident(last) {
            self.refresh_completion();
            self.request_completion(None, cx);
        } else {
            self.completion = None;
        }
    }

    fn apply_effects(&mut self, effects: Vec<Effect>, cx: &mut Context<Self>) {
        for effect in effects {
            match effect {
                Effect::Ex(command) => cx.emit(EditorEvent::Ex(command)),
                Effect::Scroll(scroll) => self.scroll(scroll, cx),
                Effect::Jump { from } => cx.emit(EditorEvent::PushJump(from)),
                Effect::Command { name, count } => {
                    let args = Args {
                        count,
                        ..Args::default()
                    };
                    match cx.global::<Commands>().handler(&name) {
                        Some(Handler::Editor(run)) => run(self, &args, cx),
                        // Another view's, or unknown: the workspace sorts
                        // it out.
                        _ => cx.emit(EditorEvent::Command { name, args }),
                    }
                }
            }
        }
        if let Some(message) = self.vim.message.clone() {
            cx.emit(EditorEvent::Message(message));
        }
    }

    // ---- folds -----------------------------------------------------------
    //
    // A closed fold shows its first line, with how many lines it hides
    // after it, and none of the rest. What can be folded is worked out when
    // it is asked for: the syntax tree's nodes over more than one line, or
    // lines indented further where there is no tree.

    /// What can be folded, as first and last lines, in the order they start.
    fn foldable(&self) -> Vec<(usize, usize)> {
        if let Some(syntax) = &self.syntax {
            return syntax.folds(&self.buf);
        }
        let tab = self.vim_lang.tab_width;
        crate::syntax::indent_folds(
            (0..self.buf.line_count()).map(|line| indent_cells(&self.buf.line_string(line), tab)),
        )
    }

    /// The first and last line of a closed fold, as the text is now.
    fn fold_lines(&self, fold: &Range<usize>) -> (usize, usize) {
        (self.buf.line_of(fold.start), self.buf.line_of(fold.end))
    }

    /// After the folds or the text changed: a fold edited down to a line is
    /// gone, and the lines hidden are worked out again.
    fn refold(&mut self) {
        let len = self.buf.len_chars();
        let mut folds = std::mem::take(&mut self.folds);
        folds.retain(|fold| {
            let (first, last) = self.fold_lines(fold);
            fold.end <= len && first < last
        });
        folds.sort_by_key(|fold| (fold.start, std::cmp::Reverse(fold.end)));
        folds.dedup();
        self.hidden = Hidden::new(
            folds
                .iter()
                .map(|fold| {
                    let (first, last) = self.fold_lines(fold);
                    (first + 1, last)
                })
                .collect(),
        );
        self.folds = folds;
    }

    /// Open every closed fold that hides `line`.
    fn unfold_line(&mut self, line: usize) {
        if !self.hidden.is_hidden(line) {
            return;
        }
        let mut folds = std::mem::take(&mut self.folds);
        folds.retain(|fold| {
            let (first, last) = self.fold_lines(fold);
            !(first < line && line <= last)
        });
        self.folds = folds;
        self.refold();
    }

    /// The closed fold whose first line is `line`, the outermost there.
    fn closed_fold_at(&self, line: usize) -> Option<usize> {
        self.folds
            .iter()
            .position(|fold| self.fold_lines(fold).0 == line)
    }

    /// Close `folds`; the cursor, if hidden now, goes to the first line of
    /// the fold it is in.
    fn close_folds(&mut self, folds: &[(usize, usize)], cx: &mut Context<Self>) {
        for &(first, last) in folds {
            let fold = self.buf.line_start(first)..self.buf.line_end(last);
            self.folds.push(fold);
        }
        self.refold();
        let line = self.buf.line_of(self.vim.cursor);
        if self.hidden.is_hidden(line) {
            let first = self.hidden.shown(line);
            let at = med_vim::motion::first_non_blank(&self.buf, first);
            self.vim.set_cursor(at, &self.buf);
        }
        self.after_folding(cx);
    }

    /// The view after folds opened or closed: it starts on a line on
    /// screen, and keeps the cursor.
    fn after_folding(&mut self, cx: &mut Context<Self>) {
        let (line, _) = self.top();
        if self.hidden.is_hidden(line) {
            self.set_top((self.hidden.shown(line), 0));
        }
        if !self.by_rows() {
            self.top_row = 0;
        } else {
            self.scroll_top = self.scroll_top.floor();
        }
        self.after_change(cx);
    }

    /// `zc`: close the innermost fold the cursor's line is in that is not
    /// closed already, of those that start on it first.
    fn fold_close(&mut self, cx: &mut Context<Self>) {
        let line = self.buf.line_of(self.vim.cursor);
        let closed: Vec<(usize, usize)> = self.folds.iter().map(|f| self.fold_lines(f)).collect();
        let fold = self
            .foldable()
            .into_iter()
            .filter(|&(first, last)| {
                first <= line && line <= last && !closed.contains(&(first, last))
            })
            .max_by_key(|&(first, _)| first);
        match fold {
            Some(fold) => self.close_folds(&[fold], cx),
            None => cx.emit(EditorEvent::Message("No fold here".into())),
        }
    }

    /// `zo`: open the closed fold on the cursor's line.
    fn fold_open(&mut self, cx: &mut Context<Self>) -> bool {
        let line = self.buf.line_of(self.vim.cursor);
        let Some(at) = self.closed_fold_at(line) else {
            return false;
        };
        self.folds.remove(at);
        self.refold();
        self.after_folding(cx);
        true
    }

    /// `za`: open the closed fold on the cursor's line, or close one.
    fn fold_toggle(&mut self, cx: &mut Context<Self>) {
        if !self.fold_open(cx) {
            self.fold_close(cx);
        }
    }

    /// `zM`: close every fold.
    fn fold_close_all(&mut self, cx: &mut Context<Self>) {
        let folds = self.foldable();
        if folds.is_empty() {
            cx.emit(EditorEvent::Message("Nothing to fold".into()));
            return;
        }
        self.close_folds(&folds, cx);
    }

    /// `zR`: open every fold.
    fn fold_open_all(&mut self, cx: &mut Context<Self>) {
        self.folds.clear();
        self.refold();
        self.after_folding(cx);
    }

    /// The closed folds, as first and last lines, in the order they start.
    pub fn closed_folds(&self) -> Vec<(usize, usize)> {
        self.folds
            .iter()
            .map(|fold| self.fold_lines(fold))
            .collect()
    }

    // ---- rows of wrapped lines ------------------------------------------
    //
    // Where lines wrap, a place on screen is a line and a row of it, and
    // the view starts at one. Unwrapped, every line is one row. A line a
    // closed fold hides has none: the walks go over it.

    /// Whether the view goes by rows rather than by lines, which may be
    /// scrolled a part of one: where lines wrap, or folds hide some.
    fn by_rows(&self) -> bool {
        self.wrap_cells.is_some() || !self.hidden.is_empty()
    }

    /// Where a line's rows start, as char columns.
    fn breaks(&self, line: usize) -> Vec<usize> {
        match self.wrap_cells {
            Some(cells) => wrap_breaks(&self.buf.line_string(line), self.vim_lang.tab_width, cells),
            None => vec![0],
        }
    }

    fn rows(&self, line: usize) -> usize {
        self.breaks(line).len()
    }

    /// The line and row an offset is on; in a closed fold, its first line.
    fn place_of(&self, offset: usize) -> (usize, usize) {
        let line = self.buf.line_of(offset);
        if self.hidden.is_hidden(line) {
            return (self.hidden.shown(line), 0);
        }
        let col = offset - self.buf.line_start(line);
        let row = self.breaks(line).partition_point(|&b| b <= col);
        (line, row.saturating_sub(1))
    }

    fn top(&self) -> (usize, usize) {
        (self.scroll_top.max(0.) as usize, self.top_row)
    }

    fn set_top(&mut self, (line, row): (usize, usize)) {
        self.scroll_top = line as f32;
        self.top_row = row;
    }

    /// `n` rows on from a place, or as far as there are.
    fn rows_on(&self, (mut line, mut row): (usize, usize), mut n: usize) -> (usize, usize) {
        let last = self.buf.last_line();
        while n > 0 {
            let rows = self.rows(line);
            let ahead = (rows - 1 - row).min(n);
            row += ahead;
            n -= ahead;
            if n == 0 {
                break;
            }
            let Some(next) = self.hidden.next(line, last) else {
                break;
            };
            line = next;
            row = 0;
            n -= 1;
        }
        (line, row)
    }

    /// `n` rows back from a place, or to the first.
    fn rows_back(&self, (mut line, mut row): (usize, usize), mut n: usize) -> (usize, usize) {
        while n > 0 {
            let back = row.min(n);
            row -= back;
            n -= back;
            if n == 0 {
                break;
            }
            let Some(prev) = self.hidden.prev(line) else {
                break;
            };
            line = prev;
            row = self.rows(line) - 1;
            n -= 1;
        }
        (line, row)
    }

    /// How many rows down `to` is from `from`, if it is no further than
    /// `limit`: the walk goes no further, whatever the size of the file.
    fn rows_between(
        &self,
        from: (usize, usize),
        to: (usize, usize),
        limit: usize,
    ) -> Option<usize> {
        if to < from || self.hidden.between(from.0, to.0) > limit {
            return None;
        }
        // A line at a time: a long line is broken into rows once.
        let (mut rows, mut row) = (0, from.1);
        let mut line = from.0;
        while line < to.0 {
            rows += self.rows(line) - row;
            row = 0;
            if rows > limit {
                return None;
            }
            line = self.hidden.next(line, to.0).unwrap_or(to.0);
        }
        rows += to.1 - row;
        (rows <= limit).then_some(rows)
    }

    /// The offset a row starts at.
    fn row_start(&self, (line, row): (usize, usize)) -> usize {
        let col = self.breaks(line).get(row).copied().unwrap_or(0);
        self.buf.line_start(line) + col.min(self.buf.line_len(line))
    }

    /// The last place on screen, from where the view starts.
    fn bottom(&self) -> (usize, usize) {
        self.rows_on(self.top(), self.visible_lines.max(1) - 1)
    }

    /// `scroll` where lines wrap: by rows.
    fn scroll_rows(&mut self, scroll: Scroll, cx: &App) {
        let visible = self.visible_lines.max(1);
        let cursor = self.place_of(self.vim.cursor);
        match scroll {
            Scroll::Center => self.set_top(self.rows_back(cursor, visible / 2)),
            Scroll::Top => self.set_top(cursor),
            Scroll::Bottom => self.set_top(self.rows_back(cursor, visible - 1)),
            Scroll::Lines(n) => {
                let top = self.top();
                let top = if n < 0 {
                    self.rows_back(top, n.unsigned_abs())
                } else {
                    self.rows_on(top, n as usize)
                };
                self.set_top(top);
                // The cursor is dragged along as it is unwrapped, by rows.
                let margin = self.scroll_margin(cx);
                let last = self.hidden.shown(self.buf.last_line());
                let end = (last, self.rows(last) - 1);
                let bottom = self.bottom();
                let low = if top == (0, 0) {
                    top
                } else {
                    self.rows_on(top, margin)
                };
                let high = if bottom == end {
                    end
                } else {
                    self.rows_back(bottom, margin)
                };
                let target = if cursor < low {
                    low
                } else if cursor > high {
                    high
                } else {
                    return;
                };
                // The same column in the line there, if that is on the
                // row; else where the row starts.
                let col = self.vim.cursor - self.buf.line_start(cursor.0);
                let same = self.buf.line_start(target.0) + col.min(self.buf.line_len(target.0));
                self.vim.cursor = if self.place_of(same) == target {
                    same
                } else {
                    self.row_start(target)
                };
            }
        }
    }

    fn scroll(&mut self, scroll: Scroll, cx: &App) {
        if self.by_rows() {
            self.scroll_rows(scroll, cx);
            return;
        }
        let line = self.buf.line_of(self.vim.cursor) as f32;
        let visible = self.visible_lines.max(1) as f32;
        match scroll {
            Scroll::Center => self.scroll_top = (line - (visible / 2.).floor()).max(0.),
            Scroll::Top => self.scroll_top = line,
            Scroll::Bottom => self.scroll_top = (line - visible + 1.).max(0.),
            Scroll::Lines(n) => {
                let max = self.buf.last_line() as f32;
                self.scroll_top = (self.scroll_top.round() + n as f32).clamp(0., max);
                // `ctrl-e`/`ctrl-y`: the cursor is dragged along if the view
                // would leave it behind, or nearer its edge than the scroll
                // margin, where the edge is not the file's.
                let last = self.buf.last_line();
                let top = self.scroll_top as usize;
                let bottom = top + self.visible_lines.max(1) - 1;
                let margin = self.scroll_margin(cx);
                let low = if top == 0 { top } else { top + margin };
                let high = if bottom >= last {
                    last
                } else {
                    bottom - margin
                };
                let cursor_line = self.buf.line_of(self.vim.cursor);
                let target = cursor_line.clamp(low.min(high), high);
                if target != cursor_line {
                    let col = self.buf.offset_to_point(self.vim.cursor).col;
                    let offset = self.buf.point_to_offset(med_core::Point::new(target, col));
                    self.vim.cursor = offset;
                }
            }
        }
    }

    /// Housekeeping after anything that may have edited or moved: feed the
    /// edits to the syntax tree and the language server, keep the cursor on
    /// screen, repaint.
    fn after_change(&mut self, cx: &mut Context<Self>) {
        self.keep_preview_text(cx);
        // What `Tab` went through, and what was being composed, were for
        // the command line just closed.
        if self.vim.cmdline().is_none() {
            self.cmdline_completion = None;
            self.cmdline_preedit = None;
            self.cmdline_caret = None;
        }
        loop {
            // Marks follow the edits made here as they do the engine's own.
            self.vim.sync(&self.buf);
            let edits = self.buf.take_edits();
            if edits.is_empty() {
                break;
            }
            if let Some(syntax) = &mut self.syntax {
                syntax.update(&self.buf, &edits);
            }
            for edit in &edits {
                for d in &mut self.diagnostics {
                    shift_range(&mut d.start, &mut d.end, edit);
                }
                for hint in &mut self.inlay_hints {
                    let mut end = hint.offset;
                    shift_range(&mut hint.offset, &mut end, edit);
                }
                for (_, group) in &mut self.decorations {
                    for d in group {
                        shift_range(&mut d.start, &mut d.end, edit);
                    }
                }
                for fold in &mut self.folds {
                    shift_fold(fold, edit);
                }
                // A token the edit touches is no longer known; those
                // after it move with the text.
                let delta = edit.new_end_char as isize - edit.old_end_char as isize;
                self.semantic_spans.retain_mut(|(range, _)| {
                    if range.end <= edit.start_char {
                        true
                    } else if range.start >= edit.old_end_char {
                        range.start = (range.start as isize + delta) as usize;
                        range.end = (range.end as isize + delta) as usize;
                        true
                    } else {
                        false
                    }
                });
            }
            if let (Some(lsp), Some(path)) = (&self.lsp, &self.path) {
                self.lsp_version += 1;
                let position = |p: med_core::Point| lsp::Position::new(p.row as u32, p.col as u32);
                let changes = edits
                    .iter()
                    .map(|edit| lsp::TextDocumentContentChangeEvent {
                        range: Some(lsp::Range::new(
                            position(edit.start_utf16),
                            position(edit.old_end_utf16),
                        )),
                        range_length: None,
                        text: edit.new_text.clone(),
                    })
                    .collect();
                let (buf, version) = (&self.buf, self.lsp_version);
                lsp.update(cx, |store, _| {
                    store.change(path, version, changes, || buf.text())
                });
            }
            if let Some(session) = &mut self.snippet {
                for edit in &edits {
                    for (at, stop) in session.stops.iter_mut().enumerate() {
                        for range in stop {
                            shift_place(range, edit, at == session.current);
                        }
                    }
                }
            }
            // The other ranges of the place typed in follow it: more edits,
            // seen to in turn.
            if !self.mirror_snippet() {
                break;
            }
        }
        // A place found in a closed fold, or a change made there, opens
        // it: what the cursor is on is never hidden.
        if !self.folds.is_empty() {
            self.refold();
            self.unfold_line(self.buf.line_of(self.vim.cursor));
            if !self.by_rows() {
                self.top_row = 0;
            }
        }
        // The snippet is done with once its places are left.
        if self
            .snippet
            .as_ref()
            .is_some_and(|s| self.vim.mode != Mode::Insert || !s.contains(self.vim.cursor))
        {
            self.snippet = None;
        }
        if self.vim.mode != Mode::Insert {
            self.marked = None;
            self.completion = None;
            self.signature = None;
        }
        self.ensure_cursor_visible(cx);
        self.follow_cursor_in_preview(cx);
        self.report_pending(cx);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    fn display_col(&self, offset: usize) -> usize {
        let line = self.buf.line_of(offset);
        let tab = self.vim_lang.tab_width.max(1);
        let ls = self.buf.line_start(line);
        let mut col = 0;
        for c in self.buf.slice(ls..offset).chars() {
            col += cell_width(c, col, tab);
        }
        // Hints before it take their cells too; tabs stop where they would
        // without them, as they are laid out.
        let hints: usize = self
            .hints_between(ls, offset)
            .iter()
            .map(|hint| {
                hint.text
                    .chars()
                    .map(|c| cell_width(c, 0, tab))
                    .sum::<usize>()
            })
            .sum();
        col + hints
    }

    fn ensure_cursor_visible(&mut self, cx: &App) {
        if self.by_rows() {
            let visible = self.visible_lines.max(1);
            let margin = self.scroll_margin(cx);
            let cursor = self.place_of(self.vim.cursor);
            let top = self.top();
            match self.rows_between(top, cursor, visible) {
                Some(rows) if rows >= margin && rows + margin < visible => {}
                Some(rows) if rows >= margin => {
                    self.set_top(self.rows_back(cursor, visible - 1 - margin));
                }
                // Above the view, or nearer its top than the margin.
                Some(_) => self.set_top(self.rows_back(cursor, margin)),
                None if cursor < top => self.set_top(self.rows_back(cursor, margin)),
                None => self.set_top(self.rows_back(cursor, visible - 1 - margin)),
            }
            if self.wrap_cells.is_some() {
                self.scroll_left = px(0.);
                return;
            }
        } else {
            let line = self.buf.line_of(self.vim.cursor);
            let visible = self.visible_lines.max(1);
            let margin = self.scroll_margin(cx);
            let top = self.scroll_top;
            if (line as f32) < top + margin as f32 {
                self.scroll_top = line.saturating_sub(margin) as f32;
            } else if line as f32 > top + (visible - 1 - margin) as f32 {
                self.scroll_top = (line + margin + 1).saturating_sub(visible) as f32;
            }
        }
        if let Some(m) = &self.metrics {
            let x = m.char_width * self.display_col(self.vim.cursor) as f32;
            let margin = m.char_width * 4.;
            if x < self.scroll_left + margin {
                self.scroll_left = (x - margin).max(px(0.));
            } else if x + margin > self.scroll_left + m.text_width {
                self.scroll_left = x + margin - m.text_width;
            }
        }
    }

    // ---- language server features --------------------------------------

    fn goto_definition(&mut self, cx: &mut Context<Self>) {
        self.goto_location(LspStore::definition, "definitions", cx);
    }

    fn goto_declaration(&mut self, cx: &mut Context<Self>) {
        self.goto_location(LspStore::declaration, "declarations", cx);
    }

    fn goto_type_definition(&mut self, cx: &mut Context<Self>) {
        self.goto_location(LspStore::type_definition, "type definitions", cx);
    }

    fn goto_implementation(&mut self, cx: &mut Context<Self>) {
        self.goto_location(LspStore::implementation, "implementations", cx);
    }

    /// Ask the language server where something of the symbol under the
    /// cursor is (its definition, its type, its implementations) and go
    /// there, as a jump; when there are several, list them to choose from.
    fn goto_location<F>(
        &mut self,
        ask: impl FnOnce(&LspStore, &Path, lsp::Position) -> Option<F>,
        what: &'static str,
        cx: &mut Context<Self>,
    ) where
        F: Future<Output = anyhow::Result<Option<lsp::GotoDefinitionResponse>>> + 'static,
    {
        let (Some(lsp), Some(path)) = (&self.lsp, &self.path) else {
            return;
        };
        let position = self.lsp_position(self.vim.cursor);
        let Some(request) = ask(lsp.read(cx), path, position) else {
            cx.emit(EditorEvent::Message("No language server".into()));
            return;
        };
        let from = self.vim.cursor;
        self.request_task = Some(cx.spawn(async move |this, cx| {
            let found = match request.await {
                Ok(Some(response)) => crate::lsp::locations(response),
                _ => Vec::new(),
            };
            this.update(cx, |_, cx| match found.as_slice() {
                [] => cx.emit(EditorEvent::Message(format!("No {what} found"))),
                [only] => match uri_to_path(&only.uri) {
                    Some(path) => {
                        cx.emit(EditorEvent::PushJump(from));
                        cx.emit(EditorEvent::OpenLocation {
                            path,
                            position: only.range.start,
                        });
                    }
                    None => cx.emit(EditorEvent::Message(format!("No {what} found"))),
                },
                _ => cx.emit(EditorEvent::Locations {
                    what,
                    places: found
                        .into_iter()
                        .map(|location| Place {
                            location,
                            label: None,
                        })
                        .collect(),
                }),
            })
            .ok();
        }));
    }

    fn find_references(&mut self, cx: &mut Context<Self>) {
        let (Some(lsp), Some(path)) = (&self.lsp, &self.path) else {
            return;
        };
        let position = self.lsp_position(self.vim.cursor);
        let Some(request) = lsp.read(cx).references(path, position) else {
            cx.emit(EditorEvent::Message("No language server".into()));
            return;
        };
        self.request_task = Some(cx.spawn(async move |this, cx| {
            let locations = request.await.ok().flatten().unwrap_or_default();
            this.update(cx, |_, cx| {
                if locations.is_empty() {
                    cx.emit(EditorEvent::Message("No references found".into()));
                } else {
                    cx.emit(EditorEvent::Locations {
                        what: "references",
                        places: locations
                            .into_iter()
                            .map(|location| Place {
                                location,
                                label: None,
                            })
                            .collect(),
                    });
                }
            })
            .ok();
        }));
    }

    /// List what calls the function under the cursor (`incoming`), or
    /// what it calls, to go to one.
    fn show_calls(&mut self, incoming: bool, cx: &mut Context<Self>) {
        let (Some(lsp), Some(path)) = (&self.lsp, &self.path) else {
            return;
        };
        let position = self.lsp_position(self.vim.cursor);
        let Some(request) = lsp.read(cx).calls(path, position, incoming) else {
            cx.emit(EditorEvent::Message("No language server".into()));
            return;
        };
        let what = if incoming { "callers" } else { "calls" };
        self.request_task = Some(cx.spawn(async move |this, cx| {
            let found = request.await;
            this.update(cx, |_, cx| match found {
                Ok(places) if places.is_empty() => {
                    cx.emit(EditorEvent::Message(format!("No {what} found")))
                }
                Ok(places) => cx.emit(EditorEvent::Locations { what, places }),
                Err(error) => cx.emit(EditorEvent::Message(format!("{error:#}"))),
            })
            .ok();
        }));
    }

    /// List the symbols of the file, as the language server sees them, to
    /// go to one.
    fn document_symbols(&mut self, cx: &mut Context<Self>) {
        let (Some(lsp), Some(path)) = (&self.lsp, &self.path) else {
            return;
        };
        let Some(uri) = crate::lsp::path_to_uri(path) else {
            return;
        };
        let Some(request) = lsp.read(cx).document_symbols(path) else {
            cx.emit(EditorEvent::Message("No language server".into()));
            return;
        };
        self.request_task = Some(cx.spawn(async move |this, cx| {
            let places = match request.await {
                Ok(Some(response)) => crate::lsp::symbol_places(response, &uri),
                _ => Vec::new(),
            };
            this.update(cx, |_, cx| {
                if places.is_empty() {
                    cx.emit(EditorEvent::Message("No symbols found".into()));
                } else {
                    cx.emit(EditorEvent::Locations {
                        what: "symbols",
                        places,
                    });
                }
            })
            .ok();
        }));
    }

    fn show_hover(&mut self, cx: &mut Context<Self>) {
        let cursor = self.vim.cursor;
        let diagnostics: Vec<String> = self
            .diagnostics
            .iter()
            .filter(|d| d.start <= cursor && cursor < d.end.max(d.start + 1))
            .map(|d| d.message.clone())
            .collect();
        let local = (!diagnostics.is_empty()).then(|| diagnostics.join("\n\n"));
        self.hover = local.clone().map(SharedString::from);
        cx.notify();

        let (Some(lsp), Some(path)) = (&self.lsp, &self.path) else {
            return;
        };
        let Some(request) = lsp.read(cx).hover(path, self.lsp_position(cursor)) else {
            return;
        };
        self.request_task = Some(cx.spawn(async move |this, cx| {
            let Some(mut text) = hovers_markdown(request.await) else {
                return;
            };
            if let Some(local) = local {
                text = format!("{local}\n\n---\n\n{text}");
            }
            this.update(cx, |this, cx| {
                if this.vim.cursor == cursor && !text.trim().is_empty() {
                    this.hover = Some(text.into());
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn jump_to_diagnostic(&mut self, forward: bool, cx: &mut Context<Self>) {
        let cursor = self.vim.cursor;
        let target = if forward {
            self.diagnostics
                .iter()
                .find(|d| d.start > cursor)
                .or(self.diagnostics.first())
        } else {
            self.diagnostics
                .iter()
                .rev()
                .find(|d| d.start < cursor)
                .or(self.diagnostics.last())
        };
        match target.map(|d| (d.start, d.message.clone())) {
            Some((start, message)) => {
                self.vim.set_cursor(start, &self.buf);
                let first_line = message.lines().next().unwrap_or_default().to_string();
                cx.emit(EditorEvent::Message(first_line));
            }
            None => cx.emit(EditorEvent::Message("No diagnostics".into())),
        }
    }

    // ---- for programs ---------------------------------------------------

    pub fn diagnostics(&self) -> &[Diag] {
        &self.diagnostics
    }

    /// The offset of a line and a column, both from 0; beyond the text it
    /// is the nearest place in it.
    pub fn offset_at_place(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.buf.last_line());
        self.buf.line_start(line) + col.min(self.buf.line_len(line))
    }

    /// The first and the last line on screen, from 0.
    pub fn visible_lines(&self) -> (usize, usize) {
        if self.by_rows() {
            return (self.top().0, self.bottom().0);
        }
        let first = (self.scroll_top.max(0.) as usize).min(self.buf.last_line());
        let last = first + self.visible_lines.max(1) - 1;
        (first, last.min(self.buf.last_line()))
    }

    /// The lines standing at the top of the view as it was last laid out,
    /// the outermost scope's first, and where each was drawn.
    pub fn sticky_lines(&self) -> Vec<(usize, Bounds<Pixels>)> {
        self.sticky
            .iter()
            .map(|header| (header.line, header.bounds))
            .collect()
    }

    /// Move the cursor, keeping it on screen.
    pub fn move_cursor(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.vim.place_cursor(offset, &self.buf);
        self.after_change(cx);
    }

    /// Put `text` in place of `range`, as one step to undo. A cursor in
    /// or at what is replaced ends up after the new text.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let max = self.buf.len_chars();
        let (start, end) = (range.start.min(max), range.end.min(max));
        let cursor = self.vim.cursor;
        self.buf.edit(start..end, text);
        let new_end = start + text.chars().count();
        let cursor = if cursor > end {
            cursor + new_end - end
        } else if cursor >= start {
            new_end
        } else {
            cursor
        };
        self.vim.place_cursor(cursor, &self.buf);
        self.completion = None;
        self.after_change(cx);
    }

    /// Give a group the decorations it is to have from now on; none takes
    /// the group away.
    pub(crate) fn set_decorations(
        &mut self,
        group: SharedString,
        decorations: Vec<Decoration>,
        cx: &mut Context<Self>,
    ) {
        self.decorations.retain(|(known, _)| *known != group);
        if !decorations.is_empty() {
            self.decorations.push((group, decorations));
        }
        cx.notify();
    }

    /// The lines a group's decorations are on, from 0, in the order they
    /// were given.
    pub fn decoration_lines(&self, group: &str) -> Vec<usize> {
        let group = self.decorations.iter().find(|(known, _)| known == group);
        group.map_or_else(Vec::new, |(_, decorations)| {
            let line = |d: &Decoration| self.buf.line_of(d.start.min(self.buf.max_offset()));
            decorations.iter().map(line).collect()
        })
    }

    // ---- edits from the language server --------------------------------

    /// Make the edits a language server asked for in this file. How many
    /// of them changed anything.
    pub fn apply_text_edits(&mut self, edits: &[lsp::TextEdit], cx: &mut Context<Self>) -> usize {
        let (applied, cursor) = apply_edits(&mut self.buf, edits, self.vim.cursor);
        if applied > 0 {
            self.vim.place_cursor(cursor, &self.buf);
            self.completion = None;
            self.after_change(cx);
        }
        applied
    }

    /// Give what is under the cursor another name, wherever it is used.
    fn rename(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = name.trim();
        if name.is_empty() {
            cx.emit(EditorEvent::Message("Rename to what? :rename name".into()));
            return;
        }
        let (Some(lsp), Some(path)) = (&self.lsp, &self.path) else {
            return;
        };
        let position = self.lsp_position(self.vim.cursor);
        let Some(request) = lsp.read(cx).rename(path, position, name.to_string()) else {
            cx.emit(EditorEvent::Message("No language server".into()));
            return;
        };
        self.request_task = Some(cx.spawn(async move |this, cx| {
            let result = request.await;
            this.update(cx, |_, cx| match result {
                Ok(Some(edit)) => cx.emit(EditorEvent::WorkspaceEdit(edit)),
                Ok(None) => cx.emit(EditorEvent::Message("Nothing to rename here".into())),
                Err(error) => cx.emit(EditorEvent::Message(format!("{error:#}"))),
            })
            .ok();
        }));
    }

    fn formatting_request(
        &self,
        cx: &App,
    ) -> Option<impl Future<Output = anyhow::Result<Option<Vec<lsp::TextEdit>>>> + use<>> {
        let (lsp, path) = (self.lsp.as_ref()?, self.path.as_ref()?);
        let width = self.tab_width(cx);
        lsp.read(cx)
            .formatting(path, width as u32, self.vim_lang.expand_tab)
    }

    /// Have the language server format the file.
    fn format(&mut self, cx: &mut Context<Self>) {
        let Some(request) = self.formatting_request(cx) else {
            cx.emit(EditorEvent::Message("No language server".into()));
            return;
        };
        let version = self.buf.version();
        self.request_task = Some(cx.spawn(async move |this, cx| {
            let result = request.await;
            this.update(cx, |this, cx| {
                let message = match result {
                    // What it formatted is no longer what is here.
                    Ok(_) if this.buf.version() != version => return,
                    Ok(edits) => match this.apply_text_edits(&edits.unwrap_or_default(), cx) {
                        0 => "Already formatted".to_string(),
                        _ => "Formatted".to_string(),
                    },
                    Err(error) => format!("{error:#}"),
                };
                cx.emit(EditorEvent::Message(message));
            })
            .ok();
        }));
    }

    /// Format the file before going on, for a moment at most: a server
    /// that takes its time does not hold up writing the file.
    pub fn format_now(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(request) = self.formatting_request(cx) else {
            return false;
        };
        let executor = cx.foreground_executor().clone();
        match executor.block_with_timeout(FORMAT_TIMEOUT, request) {
            Ok(Ok(Some(edits))) => {
                self.apply_text_edits(&edits, cx);
                true
            }
            _ => false,
        }
    }

    /// Ask what can be done about what is under the cursor, or selected.
    fn code_action(&mut self, cx: &mut Context<Self>) {
        let (Some(lsp), Some(path)) = (&self.lsp, &self.path) else {
            return;
        };
        let cursor = self.vim.cursor;
        let (start, end) = self.vim.selection(&self.buf).unwrap_or((cursor, cursor));
        let range = lsp::Range::new(self.lsp_position(start), self.lsp_position(end));
        let Some(request) = lsp.read(cx).code_actions(path, range) else {
            cx.emit(EditorEvent::Message("No language server".into()));
            return;
        };
        let version = self.buf.version();
        self.request_task = Some(cx.spawn(async move |this, cx| {
            let actions = request.await;
            let offered = |(_, action): &(ServerName, lsp::CodeActionOrCommand)| match action {
                lsp::CodeActionOrCommand::CodeAction(action) => action.disabled.is_none(),
                lsp::CodeActionOrCommand::Command(_) => true,
            };
            let actions: Vec<_> = actions
                .into_iter()
                .filter(offered)
                .map(|(server, action)| (Some(server), action))
                .collect();
            this.update(cx, |this, cx| {
                if this.buf.version() != version {
                    return;
                }
                if actions.is_empty() {
                    cx.emit(EditorEvent::Message("No code actions here".into()));
                    return;
                }
                this.menu = Some(ActionMenu {
                    actions,
                    selected: 0,
                });
                cx.notify();
            })
            .ok();
        }));
    }

    /// The titles of the code actions on offer, if the menu is up.
    pub fn code_action_titles(&self) -> Vec<String> {
        let title = |(_, action): &(Option<ServerName>, lsp::CodeActionOrCommand)| match action {
            lsp::CodeActionOrCommand::CodeAction(action) => action.title.clone(),
            lsp::CodeActionOrCommand::Command(command) => command.title.clone(),
        };
        self.menu
            .as_ref()
            .map_or_else(Vec::new, |menu| menu.actions.iter().map(title).collect())
    }

    /// Keys while the code actions are listed: they all go to the list.
    fn menu_key(&mut self, key: &Key, cx: &mut Context<Self>) -> bool {
        let Some(menu) = &mut self.menu else {
            return false;
        };
        let count = menu.actions.len();
        match key {
            Key::Char('j') | Key::Down | Key::Ctrl('n') | Key::Tab => {
                menu.selected = (menu.selected + 1) % count;
            }
            Key::Char('k') | Key::Up | Key::Ctrl('p') | Key::BackTab => {
                menu.selected = (menu.selected + count - 1) % count;
            }
            Key::Enter => self.accept_action(cx),
            Key::Char(digit @ '1'..='9') if (*digit as usize - '1' as usize) < count => {
                menu.selected = *digit as usize - '1' as usize;
                self.accept_action(cx);
            }
            _ => self.menu = None,
        }
        cx.notify();
        true
    }

    fn accept_action(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.take() else {
            return;
        };
        let Some(action) = menu.actions.into_iter().nth(menu.selected) else {
            return;
        };
        let (server, action) = action;
        let (edit, command) = match action {
            lsp::CodeActionOrCommand::CodeAction(action) => (action.edit, action.command),
            lsp::CodeActionOrCommand::Command(command) => (None, Some(command)),
        };
        if let Some(edit) = edit {
            cx.emit(EditorEvent::WorkspaceEdit(edit));
        }
        let Some(command) = command else {
            return;
        };
        // One med runs itself.
        match crate::lsp::client_command(&command) {
            Some(Ok(crate::lsp::ClientCommand::Locations(locations))) => {
                cx.emit(EditorEvent::Locations {
                    what: "references",
                    places: locations
                        .into_iter()
                        .map(|location| Place {
                            location,
                            label: None,
                        })
                        .collect(),
                });
                return;
            }
            Some(Ok(crate::lsp::ClientCommand::Run { command, cwd })) => {
                cx.emit(EditorEvent::Run { command, cwd });
                return;
            }
            Some(Err(problem)) => {
                cx.emit(EditorEvent::Message(problem));
                return;
            }
            None => {}
        }
        // What the command edits comes back through the server.
        if let (Some(lsp), Some(path)) = (&self.lsp, &self.path) {
            lsp.read(cx)
                .execute_command(path, server.as_deref(), command);
        }
    }

    /// List the lenses of the file to run one: the commands a language
    /// server puts above a function or a type, such as running a test.
    /// The one nearest above the cursor is chosen first.
    fn code_lens(&mut self, cx: &mut Context<Self>) {
        let (Some(lsp), Some(path)) = (&self.lsp, &self.path) else {
            return;
        };
        let Some(request) = lsp.read(cx).code_lenses(path) else {
            cx.emit(EditorEvent::Message("No language server".into()));
            return;
        };
        let line = self.buf.line_of(self.vim.cursor) as u32;
        let version = self.buf.version();
        self.request_task = Some(cx.spawn(async move |this, cx| {
            let lenses = request.await;
            this.update(cx, |this, cx| {
                if this.buf.version() != version {
                    return;
                }
                if lenses.is_empty() {
                    cx.emit(EditorEvent::Message("No lenses in this file".into()));
                    return;
                }
                let selected = lenses
                    .iter()
                    .rposition(|(_, lens)| lens.range.start.line <= line)
                    .unwrap_or(0);
                let actions = lenses
                    .into_iter()
                    .filter_map(|(server, lens)| {
                        let mut command = lens.command?;
                        command.title = format!("{}: {}", lens.range.start.line + 1, command.title);
                        Some((Some(server), lsp::CodeActionOrCommand::Command(command)))
                    })
                    .collect();
                this.menu = Some(ActionMenu { actions, selected });
                cx.notify();
            })
            .ok();
        }));
    }

    /// Ask for the signature of what is being called at the cursor.
    fn request_signature(&mut self, cx: &mut Context<Self>) {
        let (Some(lsp), Some(path)) = (&self.lsp, &self.path) else {
            return;
        };
        let position = self.lsp_position(self.vim.cursor);
        let Some(request) = lsp.read(cx).signature_help(path, position) else {
            return;
        };
        let version = self.buf.version();
        self.signature_task = Some(cx.spawn(async move |this, cx| {
            let help = request.await.ok().flatten();
            this.update(cx, |this, cx| {
                if this.buf.version() == version && this.vim.mode == Mode::Insert {
                    this.signature = help.and_then(signature_of);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Put up what a language server would have answered with.
    #[cfg(test)]
    pub(crate) fn offer(
        &mut self,
        actions: Vec<lsp::CodeActionOrCommand>,
        signature: Option<lsp::SignatureHelp>,
        cx: &mut Context<Self>,
    ) {
        self.menu = (!actions.is_empty()).then_some(ActionMenu {
            actions: actions.into_iter().map(|action| (None, action)).collect(),
            selected: 0,
        });
        self.signature = signature.and_then(signature_of);
        cx.notify();
    }

    /// The signature being shown, and the parameter the cursor is at.
    pub fn signature_text(&self) -> Option<(&str, Option<&str>)> {
        let signature = self.signature.as_ref()?;
        let active = signature
            .active
            .clone()
            .and_then(|range| signature.label.get(range));
        Some((signature.label.as_str(), active))
    }

    // ---- completion ----------------------------------------------------

    fn word_start(&self, cursor: usize) -> usize {
        let line_start = self.buf.line_start(self.buf.line_of(cursor));
        let mut start = cursor;
        while start > line_start && is_ident(self.buf.char_at(start - 1)) {
            start -= 1;
        }
        start
    }

    fn request_completion(&mut self, trigger: Option<char>, cx: &mut Context<Self>) {
        let (Some(lsp), Some(path)) = (&self.lsp, &self.path) else {
            return;
        };
        let position = self.lsp_position(self.vim.cursor);
        let Some(request) = lsp.read(cx).completion(path, position, trigger) else {
            return;
        };
        let version = self.buf.version();
        self.completion_task = Some(cx.spawn(async move |this, cx| {
            let (ranks, items): (Vec<usize>, Vec<lsp::CompletionItem>) =
                request.await.into_iter().unzip();
            if items.is_empty() {
                return;
            }
            this.update(cx, |this, cx| {
                if this.buf.version() != version || this.vim.mode != Mode::Insert {
                    return;
                }
                let start = this.word_start(this.vim.cursor);
                this.completion = Some(CompletionMenu {
                    items,
                    ranks,
                    filtered: Vec::new(),
                    selected: 0,
                    start,
                });
                this.refresh_completion();
                cx.notify();
            })
            .ok();
        }));
    }

    // ---- the places of a snippet ------------------------------------------

    /// Make the other ranges of the place being filled in have the text of
    /// the one typed in, if they do not. Whether there was anything to do.
    fn mirror_snippet(&mut self) -> bool {
        let Some(session) = &self.snippet else {
            return false;
        };
        let stop = &session.stops[session.current];
        let Some(typed) = stop.first() else {
            return false;
        };
        let text = self.buf.slice_string(typed.clone());
        let mut stale: Vec<Range<usize>> = stop[1..]
            .iter()
            .filter(|range| self.buf.slice_string((*range).clone()) != text)
            .cloned()
            .collect();
        if stale.is_empty() {
            return false;
        }
        // The last first, for the ones before to stay where they are.
        stale.sort_by_key(|range| std::cmp::Reverse(range.start));
        let len = text.chars().count() as isize;
        for range in stale {
            if range.end <= self.vim.cursor {
                let cursor = self.vim.cursor as isize + len - range.len() as isize;
                self.vim.cursor = cursor.max(0) as usize;
            }
            self.buf.edit(range, &text);
        }
        true
    }

    /// Go `step` places on in the snippet, or back. At its last place it
    /// is done with.
    fn snippet_jump(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some(session) = &mut self.snippet else {
            return;
        };
        let last = session.stops.len() - 1;
        let next = (session.current as isize + step).clamp(0, last as isize) as usize;
        session.current = next;
        session.fresh = true;
        let target = session.stops[next]
            .first()
            .map_or(self.vim.cursor, |r| r.start);
        if next == last {
            self.snippet = None;
        }
        self.vim.cursor = target;
        self.completion = None;
        self.after_change(cx);
        // What is being called follows the cursor to its next argument.
        if self.signature.is_some() {
            self.request_signature(cx);
        }
    }

    /// `Tab` and `S-Tab` while a snippet is being filled in, and nothing
    /// else wants them. Returns true if consumed.
    fn snippet_key(&mut self, key: &Key, cx: &mut Context<Self>) -> bool {
        if self.snippet.is_none() || self.vim.mode != Mode::Insert {
            return false;
        }
        match key {
            Key::Tab => self.snippet_jump(1, cx),
            Key::BackTab => self.snippet_jump(-1, cx),
            _ => return false,
        }
        cx.notify();
        true
    }

    /// What is typed (`key`, or text when `None`) at the start of a
    /// placeholder the snippet put there takes its place: the placeholder
    /// goes first. Returns true if that was all the key was to do, as a
    /// backspace's is.
    fn type_over_placeholder(&mut self, key: Option<&Key>, cx: &mut Context<Self>) -> bool {
        let Some(session) = &mut self.snippet else {
            return false;
        };
        if self.vim.mode != Mode::Insert || !session.fresh {
            return false;
        }
        let typed = session.stops[session.current].first().cloned();
        let Some(range) = typed.filter(|r| !r.is_empty() && r.start == self.vim.cursor) else {
            return false;
        };
        let consumed = match key {
            None | Some(Key::Char(_) | Key::Text(_)) => false,
            Some(Key::Backspace | Key::Delete) => true,
            Some(_) => return false,
        };
        session.fresh = false;
        self.buf.edit(range, "");
        self.after_change(cx);
        consumed
    }

    /// The places of the snippet being filled in, and which one the
    /// cursor is at.
    pub fn snippet_places(&self) -> Option<(usize, Vec<Vec<Range<usize>>>)> {
        self.snippet
            .as_ref()
            .map(|session| (session.current, session.stops.clone()))
    }

    /// Open the menu with `items`, as an answer from a server would.
    #[cfg(test)]
    pub(crate) fn show_completions(&mut self, items: Vec<lsp::CompletionItem>) {
        let start = self.word_start(self.vim.cursor);
        self.completion = Some(CompletionMenu {
            ranks: vec![0; items.len()],
            items,
            filtered: Vec::new(),
            selected: 0,
            start,
        });
        self.refresh_completion();
    }

    /// Re-filter the open menu against what is typed now; close it when the
    /// cursor has left the word or nothing matches.
    fn refresh_completion(&mut self) {
        let Some(menu) = &mut self.completion else {
            return;
        };
        let cursor = self.vim.cursor;
        if self.vim.mode != Mode::Insert
            || cursor < menu.start
            || self.buf.line_of(cursor) != self.buf.line_of(menu.start)
        {
            self.completion = None;
            return;
        }
        let typed = self.buf.slice_string(menu.start..cursor);
        if !typed.chars().all(is_ident) {
            self.completion = None;
            return;
        }
        // blink.cmp's matcher, with its default of one typo per four
        // typed characters. An empty needle matches everything at score 0.
        let haystacks: Vec<&str> = menu
            .items
            .iter()
            .map(|item| item.filter_text.as_deref().unwrap_or(&item.label))
            .collect();
        let config = frizbee::Config {
            max_typos: Some((typed.len() / 4) as u16),
            ..frizbee::Config::default()
        };
        let matches = frizbee::Matcher::new(typed.as_str(), &config).match_list(&haystacks);
        // Best score first; among equals the server asked first, then that
        // server's `sort_text`: one server's cannot be weighed against
        // another's.
        let mut scored: Vec<(std::cmp::Reverse<u16>, usize, &str, usize)> = matches
            .into_iter()
            .map(|m| {
                let index = m.index as usize;
                let item = &menu.items[index];
                let order = item.sort_text.as_deref().unwrap_or(&item.label);
                (std::cmp::Reverse(m.score), menu.ranks[index], order, index)
            })
            .collect();
        scored.sort();
        let filtered: Vec<usize> = scored.into_iter().map(|(_, _, _, i)| i).take(200).collect();
        if filtered.is_empty() {
            self.completion = None;
        } else {
            menu.selected = menu.selected.min(filtered.len() - 1);
            menu.filtered = filtered;
        }
    }

    /// Keys the completion menu takes for itself. Returns true if consumed.
    fn completion_key(&mut self, key: &Key, cx: &mut Context<Self>) -> bool {
        let Some(menu) = &mut self.completion else {
            return false;
        };
        let count = menu.filtered.len();
        match key {
            Key::Ctrl('n') | Key::Down => menu.selected = (menu.selected + 1) % count,
            Key::Ctrl('p') | Key::Up | Key::BackTab => {
                menu.selected = (menu.selected + count - 1) % count
            }
            Key::Tab | Key::Enter => self.accept_completion(cx),
            Key::Ctrl('e') => self.completion = None,
            _ => return false,
        }
        cx.notify();
        true
    }

    fn accept_completion(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.completion.take() else {
            return;
        };
        let Some(item) = menu.filtered.get(menu.selected).map(|&i| &menu.items[i]) else {
            return;
        };
        let cursor = self.vim.cursor;
        let (start, text) = match &item.text_edit {
            Some(lsp::CompletionTextEdit::Edit(edit)) => (
                self.offset_from_lsp(edit.range.start),
                edit.new_text.clone(),
            ),
            Some(lsp::CompletionTextEdit::InsertAndReplace(edit)) => (
                self.offset_from_lsp(edit.insert.start),
                edit.new_text.clone(),
            ),
            None => (
                menu.start,
                item.insert_text
                    .clone()
                    .unwrap_or_else(|| item.label.clone()),
            ),
        };
        let start = start.min(cursor);
        // A snippet is its text, with the cursor at the first place to
        // fill in.
        let (text, at, places) = if item.insert_text_format == Some(lsp::InsertTextFormat::SNIPPET)
        {
            let line = self.buf.line_string(self.buf.line_of(start));
            let indent: String = line
                .chars()
                .take_while(|c| matches!(c, ' ' | '\t'))
                .collect();
            let unit = self.vim_lang.indent_unit();
            let expanded = crate::snippet::reindent(crate::snippet::expand(&text), &indent, &unit);
            let at = expanded.cursor();
            let places = expanded.has_places().then_some(expanded.stops);
            (expanded.text, at, places)
        } else {
            let at = text.chars().count();
            (text, at, None)
        };
        // Resolve extra edits (auto-imports) before the buffer shifts.
        let mut extra: Vec<(usize, usize, String)> = item
            .additional_text_edits
            .iter()
            .flatten()
            .map(|edit| {
                (
                    self.offset_from_lsp(edit.range.start),
                    self.offset_from_lsp(edit.range.end),
                    edit.new_text.clone(),
                )
            })
            .collect();
        extra.sort_by_key(|edit| std::cmp::Reverse(edit.0));

        self.buf.edit(start..cursor, &text);
        let mut new_cursor = start + at;
        for (s, e, text) in extra {
            if e <= start {
                self.buf.edit(s..e, &text);
                new_cursor = new_cursor + text.chars().count() - (e - s);
            }
        }
        self.vim.cursor = new_cursor;
        self.after_change(cx);
        // Its places, to go through with `Tab`; ones of a snippet put in
        // before are done with.
        if let Some(stops) = places {
            let base = new_cursor - at;
            let stops = stops
                .into_iter()
                .map(|stop| {
                    stop.into_iter()
                        .map(|r| base + r.start..base + r.end)
                        .collect()
                })
                .collect();
            self.snippet = Some(SnippetSession {
                stops,
                current: 0,
                fresh: true,
            });
            if self.mirror_snippet() {
                self.after_change(cx);
            }
        }
        // A call put in with the cursor in its parentheses: what it takes.
        let before = new_cursor.checked_sub(1).map(|at| self.buf.char_at(at));
        let has_signature = match (&self.lsp, &self.path, before) {
            (Some(lsp), Some(path), Some(c)) => lsp.read(cx).signature_triggers(path).contains(&c),
            _ => false,
        };
        if has_signature {
            self.request_signature(cx);
        }
    }

    // ---- input ---------------------------------------------------------

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = translate_key(&event.keystroke, self.vim.mode) else {
            return;
        };
        cx.stop_propagation();
        self.press(key, cx);
    }

    /// A key press as the user makes it: the menus get first refusal, then
    /// completion on the `:` line, then the vim engine.
    pub fn press(&mut self, key: Key, cx: &mut Context<Self>) {
        if !self.menu_key(&key, cx)
            && !self.completion_key(&key, cx)
            && !self.snippet_key(&key, cx)
            && !self.cmdline_complete(&key, cx)
            && !self.type_over_placeholder(Some(&key), cx)
        {
            // With no snippet to go back through, as it was: a tab.
            let key = match key {
                Key::BackTab if self.vim.mode == Mode::Insert => Key::Tab,
                key => key,
            };
            self.handle_key(key, cx);
        }
    }

    /// `Tab` and `S-Tab` on the `:` line. Returns true if consumed.
    fn cmdline_complete(&mut self, key: &Key, cx: &mut Context<Self>) -> bool {
        let forward = match key {
            Key::Tab => true,
            Key::BackTab => false,
            _ => return false,
        };
        let Some((':', line)) = self.vim.cmdline() else {
            return false;
        };
        let line = line.to_string();
        let previous = self.cmdline_completion.take();
        let commands = cx.global::<Commands>();
        // With nothing to complete, the key does nothing: a tab in a
        // command is never what was meant.
        if let Some((shown, state)) = crate::cmdline::Completion::step(
            previous,
            &line,
            forward,
            self.root.as_deref(),
            commands,
        ) {
            self.cmdline_completion = state;
            self.vim.set_cmdline_text(shown);
            self.after_change(cx);
        }
        true
    }

    /// Labels of the completion menu's entries, best match first.
    pub fn completion_labels(&self) -> Vec<String> {
        self.completion.as_ref().map_or_else(Vec::new, |menu| {
            menu.filtered
                .iter()
                .map(|&index| menu.items[index].label.clone())
                .collect()
        })
    }

    /// The hover popup's markdown, if it is showing.
    pub fn hover_text(&self) -> Option<&str> {
        self.hover.as_deref()
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if matches!(self.vim.mode, Mode::Insert | Mode::Command) {
            self.with_vim(cx, |vim, ctx| vim.paste(&text, ctx));
            self.after_change(cx);
        } else {
            self.feed_keys([Key::Char('"'), Key::Char('+'), Key::Char('P')], cx);
        }
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if self.vim.mode.is_visual() {
            self.handle_key(Key::Char('y'), cx);
        }
    }

    fn offset_at(&self, position: Point<Pixels>, window: &Window) -> Option<usize> {
        let m = self.metrics.as_ref()?;
        // The line, and the row of it, under the pointer.
        let (line, row) = if self.by_rows() {
            let rows = ((position.y - m.text_origin.y) / m.line_height).max(0.) as usize;
            self.rows_on(self.top(), rows)
        } else {
            let row = (position.y - m.text_origin.y) / m.line_height + self.scroll_top;
            ((row.max(0.) as usize).min(self.buf.last_line()), 0)
        };
        let breaks = self.breaks(line);
        let (display, offsets, _) = self.display_line(line);
        let run = TextRun {
            len: display.len(),
            font: m.font.clone(),
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped = window
            .text_system()
            .shape_line(display.into(), m.font_size, &[run], None);
        // The x in the line laid out on one row: where the row starts in it.
        let row_x = shaped.x_for_index(offsets[breaks[row]] as usize);
        let x = (position.x - m.text_origin.x + self.scroll_left).max(px(0.)) + row_x;
        let index = shaped.closest_index_for_x(x) as u32;
        let col = offsets.partition_point(|&b| b <= index).saturating_sub(1);
        // Past the end of a row is its last char, not the next row's first.
        let col = match breaks.get(row + 1) {
            Some(&next) => col.min(next - 1),
            None => col,
        };
        Some(self.buf.line_start(line) + col)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        // On a scope's first line standing at the top: to that line, put
        // where it stood.
        let header = self
            .sticky
            .iter()
            .find(|header| header.bounds.contains(&event.position))
            .map(|header| (header.line, header.row));
        if let Some((line, row)) = header {
            self.hover = None;
            self.completion = None;
            self.menu = None;
            let blanks = self
                .buf
                .line(line)
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .count();
            let offset = self.buf.line_start(line) + blanks.min(self.buf.line_len(line));
            self.vim.set_cursor(offset, &self.buf);
            self.set_top(self.rows_back((line, 0), row));
            self.after_change(cx);
            return;
        }
        let Some(offset) = self.offset_at(event.position, window) else {
            return;
        };
        self.hover = None;
        self.completion = None;
        self.menu = None;
        match event.click_count {
            2 => {
                if let Some((s, e)) = med_vim::motion::word_under_cursor(&self.buf, offset) {
                    self.vim.select(s, e - 1, false, &mut self.buf);
                }
            }
            3 => self.vim.select(offset, offset, true, &mut self.buf),
            _ => {
                self.vim.set_cursor(offset, &self.buf);
                self.drag_anchor = Some(self.vim.cursor);
            }
        }
        self.after_change(cx);
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(anchor) = self.drag_anchor else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag_anchor = None;
            return;
        }
        let Some(offset) = self.offset_at(event.position, window) else {
            return;
        };
        if offset != anchor || self.vim.mode.is_visual() {
            self.vim.select(anchor, offset, false, &mut self.buf);
            self.after_change(cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.drag_anchor = None;
    }

    fn on_scroll_wheel(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = &self.metrics else {
            return;
        };
        let delta = event.delta.pixel_delta(m.line_height);
        if self.by_rows() {
            if self.wrap_cells.is_none() {
                self.scroll_left = (self.scroll_left - delta.x).max(px(0.));
            }
            // By whole rows, what is left over kept for the next turn.
            self.wheel_rows -= delta.y / m.line_height;
            let rows = self.wheel_rows.trunc();
            self.wheel_rows -= rows;
            let top = self.top();
            let top = if rows < 0. {
                self.rows_back(top, -rows as usize)
            } else {
                self.rows_on(top, rows as usize)
            };
            self.set_top(top);
            self.hover = None;
            cx.notify();
            return;
        }
        let max = self.buf.last_line() as f32;
        self.scroll_top = (self.scroll_top - delta.y / m.line_height).clamp(0., max);
        self.scroll_left = (self.scroll_left - delta.x).max(px(0.));
        self.hover = None;
        cx.notify();
    }

    // ---- painting ------------------------------------------------------

    fn layout_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Frame {
        let settings = cx.global::<Settings>().clone();
        let relative_number = self.relative_number(cx);
        let cursor_line_shown = self.cursor_line(cx);
        let color_columns = self.color_columns(cx);
        let theme = cx.theme();
        let (bg, fg, muted) = (theme.background, theme.foreground, theme.muted_foreground);
        let selection_bg = theme.selection;
        let caret = theme.caret;
        let highlight = theme.highlight_theme.clone();
        let line_number_color = highlight.style.editor_line_number.unwrap_or(muted);
        let active_line_bg = highlight
            .style
            .editor_active_line
            .unwrap_or(theme.secondary.opacity(0.5));
        // A shade of the text over whatever is behind, the cursor's line
        // too: darker in a light theme, lighter in a dark one.
        let color_column_bg = fg.opacity(0.07);
        let (guide_color, block_guide_color) = (fg.opacity(0.12), fg.opacity(0.35));
        // A closed fold: behind the count of its lines, and its sign.
        let fold_bg = fg.opacity(0.1);
        let fold_sign = SharedString::new_static("▸");
        let indent_guides = self.indent_guides(cx);
        let sticky_scroll = self.sticky_scroll(cx);
        let sticky_edge_color = theme.border;
        let diag_colors = [theme.danger, theme.warning, theme.info, muted];
        let (git_added, git_modified, git_deleted) = (theme.green, theme.blue, theme.red);
        let git_hunks = match self.git_gutter(cx) {
            true => self.git_hunks.clone(),
            false => Vec::new(),
        };
        let search_bg = theme.warning.opacity(0.35);
        // What `motion.flash` found: the place `enter` goes to stands out
        // among the others, and a label from everything.
        let flash_target_bg = theme.warning.opacity(0.7);
        let (label_bg, label_fg) = (theme.danger, theme.background);
        let scrollbar_color = theme.scrollbar_thumb;
        let family = settings
            .font_family
            .clone()
            .unwrap_or_else(|| theme.mono_font_family.clone());

        let base_font = font(family);
        let font_size = px(settings.font_size);
        let line_height = px((settings.font_size * 1.5).round());
        let text_system = window.text_system().clone();
        let plain = |len: usize, color: Hsla| TextRun {
            len,
            font: base_font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let char_width = text_system
            .shape_line("0".into(), font_size, &[plain(1, fg)], None)
            .width;

        let buf = &self.buf;
        let line_count = buf.line_count();
        let digits = line_count.to_string().len().max(3);
        let gutter_width = char_width * (digits as f32 + 3.);
        let text_origin = point(bounds.left() + gutter_width, bounds.top());
        let text_width = (bounds.size.width - gutter_width).max(px(0.));
        let visible = ((bounds.size.height / line_height).floor() as usize).max(1);
        let text_bounds = Bounds::new(text_origin, size(text_width, bounds.size.height));

        // Wrapped, a row has as many whole cells as fit.
        let wrap_cells = self
            .wrap(cx)
            .then(|| ((text_width / char_width).floor() as usize).max(1));
        let first_layout = self.visible_lines != visible
            || self.metrics.is_none()
            || self.wrap_cells != wrap_cells;
        self.visible_lines = visible;
        self.wrap_cells = wrap_cells;
        // Not where lines wrap: rows are as wide as their text.
        self.show_hints = self.inlay_hints_on(cx) && wrap_cells.is_none();
        self.metrics = Some(Metrics {
            text_origin,
            text_width,
            line_height,
            char_width,
            font_size,
            font: base_font.clone(),
        });
        self.scroll_top = self.scroll_top.clamp(0., (line_count - 1) as f32);
        let by_rows = self.by_rows();
        if by_rows {
            // Whole lines and rows of them: the wheel keeps its own part.
            // The first is on screen, not in a closed fold.
            self.scroll_top = self.scroll_top.floor();
            let (top, _) = self.top();
            if self.hidden.is_hidden(top) {
                self.set_top((self.hidden.shown(top), 0));
            }
            self.top_row = self.top_row.min(self.rows(self.top().0) - 1);
            if wrap_cells.is_some() {
                self.scroll_left = px(0.);
            }
        } else {
            self.top_row = 0;
        }
        if first_layout {
            self.ensure_cursor_visible(cx);
        }
        let buf = &self.buf;

        let first = self.scroll_top.floor() as usize;
        let hidden = &self.hidden;
        // The lines that may be drawn: those not in closed folds, from the
        // first, as many as the view could show.
        let mut shown = vec![first];
        while shown.len() < visible + 2 {
            match hidden.next(shown[shown.len() - 1], line_count - 1) {
                Some(next) => shown.push(next),
                None => break,
            }
        }
        let last = shown[shown.len() - 1] + 1;
        // Of lines that follow each other: the hidden ones not looked at.
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for &line in &shown {
            match spans.last_mut() {
                Some(span) if span.1 + 1 == line => span.1 = line,
                _ => spans.push((line, line)),
            }
        }
        let y0 = match by_rows {
            true => bounds.top() - line_height * self.top_row as f32,
            false => bounds.top() - line_height * self.scroll_top.fract(),
        };
        let scroll_left = self.scroll_left;

        let (mut captures, mut styles) = match &self.syntax {
            Some(syntax) => (
                spans
                    .iter()
                    .flat_map(|&(from, to)| {
                        let start = buf.char_to_byte(buf.line_start(from));
                        let end = buf.char_to_byte(buf.line_end(to));
                        syntax.captures(buf, start..end)
                    })
                    .collect(),
                syntax
                    .capture_names()
                    .iter()
                    .map(|name| highlight.style(name))
                    .collect::<Vec<Option<HighlightStyle>>>(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        // The tokens the language server told apart, over the syntax
        // tree's: drawn after its captures, they win.
        let char_start = buf.line_start(first);
        let char_end = buf.line_end(last - 1);
        let mut token_styles: Vec<(&str, u32)> = Vec::new();
        for (range, name) in &self.semantic_spans {
            if range.end <= char_start || range.start >= char_end {
                continue;
            }
            let index = match token_styles.iter().find(|(known, _)| known == name) {
                Some((_, index)) => *index,
                None => {
                    let Some(style) = highlight.style(name) else {
                        continue;
                    };
                    let index = styles.len() as u32;
                    styles.push(Some(style));
                    token_styles.push((name, index));
                    index
                }
            };
            captures.push(crate::syntax::Capture {
                range: buf.char_to_byte(range.start)..buf.char_to_byte(range.end),
                index,
            });
        }

        let focused = self.focus_handle.is_focused(window);
        let mode = self.vim.mode;
        let cursor = self.vim.cursor;
        let cursor_line = buf.line_of(cursor);
        let block_cursor = focused && (mode == Mode::Normal || mode.is_visual());
        let selections = self.vim.selections(buf);
        let search = {
            let shared = &cx.global::<VimGlobal>().0;
            shared
                .search
                .as_ref()
                .filter(|_| shared.hlsearch)
                .map(|s| s.regex.clone())
        };
        let tab_width = self.vim_lang.tab_width;
        let flash = self.vim.flash();
        // Decorations, with the colors they come to in this theme.
        struct Mark {
            start: usize,
            end: usize,
            whole_line: bool,
            sign: Option<SharedString>,
            text: Option<SharedString>,
            bg: Option<Hsla>,
            color: Option<Hsla>,
        }
        let mut marks: Vec<Mark> = self
            .decorations
            .iter()
            .flat_map(|(_, group)| group)
            .map(|d| Mark {
                start: d.start,
                end: d.end,
                whole_line: d.whole_line,
                sign: d.sign.clone(),
                text: d.text.clone(),
                bg: d.bg.map(|color| color.of(theme)),
                color: d.color.map(|color| color.of(theme)),
            })
            .collect();
        // The places of a snippet still to fill in: the one the cursor is
        // at as if selected.
        if let Some(session) = &self.snippet {
            let last = session.stops.len() - 1;
            for (at, stop) in session.stops.iter().enumerate().take(last) {
                for range in stop.iter().filter(|r| !r.is_empty()) {
                    marks.push(Mark {
                        start: range.start,
                        end: range.end,
                        whole_line: false,
                        sign: None,
                        text: None,
                        bg: Some(if at == session.current {
                            theme.selection
                        } else {
                            theme.list_hover
                        }),
                        color: None,
                    });
                }
            }
        }
        // The places of the name under the cursor: brighter where it is
        // written.
        marks.extend(self.doc_highlights.iter().map(|(range, write)| Mark {
            start: range.start,
            end: range.end,
            whole_line: false,
            sign: None,
            text: None,
            bg: Some(if *write {
                theme.list_active
            } else {
                theme.list_hover
            }),
            color: None,
        }));

        let mut frame = Frame {
            bounds,
            text_bounds,
            background: bg,
            line_height,
            back: Vec::new(),
            text_back: Vec::new(),
            lines: Vec::new(),
            gutter: Vec::new(),
            front: Vec::new(),
            labels: Vec::new(),
            sticky: Vec::new(),
            sticky_edge: None,
            scrollbar: None,
        };
        for band in column_bands(&color_columns, text_bounds, char_width, scroll_left) {
            frame.text_back.push(fill(band, color_column_bg));
        }
        // Indent guides: the lines' indents, and a blank one's from those
        // around it, looked for no further than this from the view.
        // Closed folds are a line each, as on screen.
        let guides = indent_guides.then(|| {
            let from = hidden.up(first, GUIDE_REACH);
            let to = hidden.down(last - 1, GUIDE_REACH, line_count - 1);
            let mut lines = vec![from];
            while let Some(next) = hidden.next(lines[lines.len() - 1], to) {
                lines.push(next);
            }
            let raw: Vec<Option<usize>> = lines
                .iter()
                .map(|&line| indent_cells(&buf.line_string(line), tab_width))
                .collect();
            let indents = line_indents(&raw);
            let block = lines
                .binary_search(&cursor_line)
                .ok()
                .and_then(|at| block_guide(&indents, at, tab_width));
            (lines, indents, block)
        });

        // Each line drawn: where its first row is, and how many it has.
        let mut drawn: Vec<(usize, Pixels, usize)> = Vec::new();
        let mut y = y0;
        for &line in &shown {
            if y >= bounds.bottom() {
                break;
            }
            let ls = buf.line_start(line);
            let text = buf.line_string(line);
            let (mut display, mut offsets, hint_bytes) = self.display_line(line);
            let nchars = offsets.len() - 1;
            let le = ls + nchars;
            let line_byte = buf.char_to_byte(ls);
            // The labels of syntax nodes stand in the text, before a node
            // and after it, like brackets: the outer ones outside.
            let mut labels: Vec<(usize, char)> = Vec::new();
            for view in flash.iter().filter(|view| view.nodes) {
                let on_line = |at: usize| (ls..=le).contains(&at).then(|| at - ls);
                let labelled = |m: &FlashMatch| m.label.map(|label| (*m, label));
                for (m, label) in view.matches.iter().filter_map(labelled) {
                    // A node that ends with its line: before the line break.
                    let ends_line = m.end > m.start && buf.char_at(m.end - 1) == '\n';
                    let end = if ends_line { m.end - 1 } else { m.end };
                    labels.extend(on_line(end).map(|col| (col, label)));
                }
                for (m, label) in view.matches.iter().rev().filter_map(labelled) {
                    labels.extend(on_line(m.start).map(|col| (col, label)));
                }
            }
            labels.sort_by_key(|&(col, _)| col);
            let label_bytes = insert_labels(&mut display, &mut offsets, &labels);

            // Which capture styles each byte of the line.
            let mut capture_at = vec![u32::MAX; text.len()];
            for capture in &captures {
                let s = capture.range.start.max(line_byte);
                let e = capture.range.end.min(line_byte + text.len());
                if s < e {
                    capture_at[s - line_byte..e - line_byte].fill(capture.index);
                }
            }
            // Most severe diagnostic over each char (0 = none).
            let mut severity_at = vec![0u8; nchars + 1];
            let mut line_diag: Option<&Diag> = None;
            for d in &self.diagnostics {
                let d_end = d.end.max(d.start + 1);
                if d.start > le || d_end <= ls {
                    continue;
                }
                if d.start >= ls && line_diag.is_none_or(|best| d.severity < best.severity) {
                    line_diag = Some(d);
                }
                for i in d.start.max(ls)..d_end.min(le + 1) {
                    let slot = &mut severity_at[i - ls];
                    if *slot == 0 || d.severity < *slot {
                        *slot = d.severity;
                    }
                }
            }

            // The decorations that touch this line, and the color each
            // char of it is given by one.
            let line_marks: Vec<&Mark> = marks
                .iter()
                .filter(|m| match m.whole_line {
                    true => m.start >= ls && m.start <= le,
                    false => m.start <= le && m.end > ls,
                })
                .collect();
            let mut color_at: Vec<Option<Hsla>> = vec![None; nchars + 1];
            for mark in line_marks.iter().filter(|m| !m.whole_line) {
                if let Some(color) = mark.color {
                    for slot in &mut color_at[mark.start.max(ls) - ls..mark.end.min(le) - ls] {
                        *slot = Some(color);
                    }
                }
            }
            // While `motion.flash` looks for text, or a find has marked
            // where it could go next, the text is dimmed but for those
            // places.
            let found: Vec<&FlashMatch> = flash
                .iter()
                .filter(|view| !view.nodes)
                .flat_map(|view| view.matches)
                .filter(|m| m.start >= ls && m.end <= le)
                .collect();
            if let Some(view) = &flash {
                let (from, to) = view.dim;
                for (i, slot) in color_at.iter_mut().enumerate() {
                    if (from..to).contains(&(ls + i)) {
                        *slot = Some(muted);
                    }
                }
                for m in &found {
                    color_at[m.start - ls..m.end - ls].fill(Some(fg));
                }
            }

            let label_run = |len: u32| {
                let mut run = plain(len as usize, label_fg);
                run.font.weight = FontWeight::BOLD;
                run
            };
            let mut runs: Vec<TextRun> = Vec::new();
            let mut last_key = None;
            for (i, (byte, _)) in text.char_indices().enumerate() {
                if hint_bytes[i] > 0 {
                    runs.push(plain(hint_bytes[i] as usize, muted));
                    last_key = None;
                }
                if label_bytes[i] > 0 {
                    runs.push(label_run(label_bytes[i]));
                    last_key = None;
                }
                let len =
                    (offsets[i + 1] - offsets[i] - label_bytes[i + 1] - hint_bytes[i + 1]) as usize;
                let offset = ls + i;
                let inverse = block_cursor && offset == cursor;
                let marked = self.marked.as_ref().is_some_and(|m| m.contains(&offset));
                let key = (
                    capture_at[byte],
                    severity_at[i],
                    marked,
                    inverse,
                    color_at[i],
                );
                if last_key == Some(key) {
                    if let Some(run) = runs.last_mut() {
                        run.len += len;
                    }
                    continue;
                }
                last_key = Some(key);
                let style = styles.get(key.0 as usize).copied().flatten();
                let color = key.4.or(style.and_then(|s| s.color)).unwrap_or(fg);
                let mut run = plain(len, color);
                if let Some(style) = style {
                    if let Some(weight) = style.font_weight {
                        run.font.weight = weight;
                    }
                    if let Some(font_style) = style.font_style {
                        run.font.style = font_style;
                    }
                }
                if inverse {
                    run.color = bg;
                }
                if key.1 > 0 {
                    run.underline = Some(UnderlineStyle {
                        thickness: px(1.),
                        color: Some(diag_colors[(key.1 - 1) as usize]),
                        wavy: true,
                    });
                } else if marked {
                    run.underline = Some(UnderlineStyle {
                        thickness: px(1.),
                        color: Some(fg),
                        wavy: false,
                    });
                }
                runs.push(run);
            }
            if hint_bytes[nchars] > 0 {
                runs.push(plain(hint_bytes[nchars] as usize, muted));
            }
            if label_bytes[nchars] > 0 {
                runs.push(label_run(label_bytes[nchars]));
            }
            let shaped = text_system.shape_line(display.into(), font_size, &runs, None);
            let x_of = |col: usize| shaped.x_for_index(offsets[col.min(nchars)] as usize);
            let starts = match wrap_cells {
                Some(cells) => wrap_breaks(&text, tab_width, cells)
                    .into_iter()
                    .map(&x_of)
                    .collect(),
                None => vec![scroll_left],
            };
            let fold = Fold {
                starts,
                left: text_origin.x,
                top: y,
                line_height,
            };
            let span = |from: usize, to: usize, through_newline: bool, color: Hsla| {
                let x1 = x_of(from);
                let mut x2 = x_of(to);
                if through_newline {
                    x2 += char_width;
                }
                fold.spans(x1, x2)
                    .into_iter()
                    .map(move |cell| fill(cell, color))
            };
            // The whole width of the view, on every row of the line.
            let whole_line = |color: Hsla| -> Vec<PaintQuad> {
                (0..fold.rows())
                    .map(|row| {
                        let origin = point(bounds.left(), fold.row_top(row));
                        fill(
                            Bounds::new(origin, size(bounds.size.width, line_height)),
                            color,
                        )
                    })
                    .collect()
            };

            if cursor_line_shown && line == cursor_line && !mode.is_visual() {
                frame.back.extend(whole_line(active_line_bg));
            }
            if let Some((lines, indents, block)) = &guides
                && let Ok(at) = lines.binary_search(&line)
            {
                let indent = indents[at];
                for column in (0..indent).step_by(tab_width.max(1)) {
                    let in_block = block.is_some_and(|(c, first, last)| {
                        c == column && (first..=last).contains(&at)
                    });
                    let color = if in_block {
                        block_guide_color
                    } else {
                        guide_color
                    };
                    let x = char_width * column as f32;
                    if let Some(cell) = fold.spans(x, x + px(1.)).into_iter().next() {
                        frame.text_back.push(fill(cell, color));
                    }
                }
            }
            for mark in &line_marks {
                let Some(color) = mark.bg else {
                    continue;
                };
                if mark.whole_line {
                    frame.back.extend(whole_line(color));
                } else {
                    let (a, b) = (mark.start.max(ls), mark.end.min(le + 1));
                    if a < b {
                        let through = b > le;
                        frame
                            .text_back
                            .extend(span(a - ls, (b - ls).min(nchars), through, color));
                    }
                }
            }
            if let Some(regex) = &search {
                for m in regex.find_iter(&text) {
                    if m.is_empty() {
                        continue;
                    }
                    let from = text[..m.start()].chars().count();
                    let to = from + text[m.start()..m.end()].chars().count();
                    frame.text_back.extend(span(from, to, false, search_bg));
                }
            }
            for m in &found {
                let color = if m.target { flash_target_bg } else { search_bg };
                frame
                    .text_back
                    .extend(span(m.start - ls, m.end - ls, false, color));
                // Its label, over the character after it.
                let Some(label) = m.label else {
                    continue;
                };
                let origin = fold.at(x_of(m.end - ls));
                let cell = Bounds::new(origin, size(char_width, line_height));
                frame.front.push(fill(cell, label_bg));
                let mut run = plain(label.len_utf8(), label_fg);
                run.font.weight = FontWeight::BOLD;
                let label =
                    text_system.shape_line(label.to_string().into(), font_size, &[run], None);
                frame.labels.push((origin, label));
            }
            // Behind the labels that stand in the text.
            for (col, &bytes) in label_bytes
                .iter()
                .enumerate()
                .filter(|(_, bytes)| **bytes > 0)
            {
                let from = shaped.x_for_index((offsets[col] - bytes) as usize);
                for cell in fold.spans(from, x_of(col)) {
                    frame.text_back.push(fill(cell, label_bg));
                }
            }
            for &(s, e) in &selections {
                let (a, b) = (s.max(ls), e.min(le + 1));
                if a < b {
                    frame.text_back.extend(span(
                        a - ls,
                        (b - ls).min(nchars),
                        b > le,
                        selection_bg,
                    ));
                }
            }
            if line == cursor_line {
                let col = cursor - ls;
                let x = x_of(col);
                let width = if col < nchars {
                    (x_of(col + 1) - x).max(px(2.))
                } else {
                    char_width
                };
                let cell = Bounds::new(fold.at(x), size(width, line_height));
                if block_cursor {
                    frame.text_back.push(fill(cell, caret));
                } else if focused && mode == Mode::Insert {
                    frame.front.push(fill(
                        Bounds::new(cell.origin, size(px(2.), line_height)),
                        caret,
                    ));
                } else {
                    frame
                        .front
                        .push(outline(cell, caret.opacity(0.7), BorderStyle::default()));
                }
            }

            // Inline diagnostic after the end of the line.
            let mut text_end = shaped.width;
            for (row, start) in fold.starts.iter().enumerate() {
                // Wrapped, each row shows its part of the line and no more.
                let clip = (fold.rows() > 1).then(|| {
                    let origin = point(text_origin.x, fold.row_top(row));
                    // To where the next row starts: the text area can be
                    // a part of a cell wider, which would show its start.
                    let width = match fold.starts.get(row + 1) {
                        Some(next) => *next - *start,
                        None => text_width,
                    };
                    Bounds::new(origin, size(width, line_height))
                });
                let origin = point(text_origin.x - *start, fold.row_top(row));
                frame.lines.push((origin, shaped.clone(), clip));
            }
            // A closed fold's first line: how many lines it hides.
            let folded = hidden.around(line + 1).map(|(_, end)| end - line);
            if let Some(lines) = folded {
                let words = match lines {
                    1 => "⋯ 1 line".to_string(),
                    n => format!("⋯ {n} lines"),
                };
                let run = plain(words.len(), muted);
                let shaped = text_system.shape_line(words.into(), font_size, &[run], None);
                let x = text_end + char_width * 1.5;
                let origin = fold.at(x);
                let pill = Bounds::new(
                    point(origin.x - char_width * 0.5, origin.y),
                    size(shaped.width + char_width, line_height),
                );
                frame
                    .text_back
                    .push(fill(pill, fold_bg).corner_radii(line_height * 0.25));
                text_end = x + shaped.width;
                frame.lines.push((origin, shaped, None));
            }
            // What programs have to say of the line, after its end.
            for mark in &line_marks {
                let Some(words) = mark.text.as_ref().filter(|_| mark.whole_line) else {
                    continue;
                };
                let words: String = words
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(200)
                    .collect();
                let run = plain(words.len(), mark.color.unwrap_or(muted));
                let shaped = text_system.shape_line(words.into(), font_size, &[run], None);
                let x = text_end + char_width * 3.;
                text_end = x + shaped.width;
                frame.lines.push((fold.at(x), shaped, None));
            }
            if let Some(d) = line_diag {
                let color = diag_colors[(d.severity - 1) as usize];
                let message: String = d
                    .message
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(160)
                    .collect();
                let run = plain(message.len(), color.opacity(0.75));
                let shaped = text_system.shape_line(message.into(), font_size, &[run], None);
                let x = text_end + char_width * 3.;
                frame.lines.push((fold.at(x), shaped, None));
            }

            // Gutter: line number, tinted by the worst diagnostic. Counted
            // from the cursor's, a closed fold is a line.
            let number = match relative_number && line != cursor_line {
                true => {
                    let away = hidden.between(line.min(cursor_line), line.max(cursor_line));
                    format!("{away:>digits$}")
                }
                false => line_number(line, cursor_line, relative_number, digits),
            };
            let worst = severity_at.iter().copied().filter(|s| *s > 0).min();
            let color = match worst {
                Some(severity) => diag_colors[(severity - 1) as usize],
                None if line == cursor_line => fg,
                None => line_number_color,
            };
            let run = plain(number.len(), color);
            let shaped = text_system.shape_line(number.into(), font_size, &[run], None);
            frame
                .gutter
                .push((point(bounds.left() + char_width * 1.5, y), shaped));
            if let Some(severity) = worst {
                frame.back.push(fill(
                    Bounds::new(point(bounds.left(), y), size(px(3.), line_height)),
                    diag_colors[(severity - 1) as usize],
                ));
            }
            // A program's sign, in front of the number: the first there is.
            let sign = line_marks
                .iter()
                .filter(|m| m.whole_line)
                .find_map(|m| Some((m.sign.as_ref()?, m.color)))
                .or_else(|| folded.map(|_| (&fold_sign, Some(muted))));
            if let Some((sign, color)) = sign {
                let sign: String = sign.chars().filter(|c| !c.is_control()).take(1).collect();
                let run = plain(sign.len(), color.unwrap_or(fg));
                let shaped = text_system.shape_line(sign.into(), font_size, &[run], None);
                frame
                    .gutter
                    .push((point(bounds.left() + char_width * 0.5, y), shaped));
            }
            // What git has of the line: a bar between the number and the
            // text down a line added or changed, and a tick where lines
            // were taken out.
            let marks = git_marks(&git_hunks, line, line_count.saturating_sub(1));
            let git_x = bounds.left() + gutter_width - char_width;
            if let Some(kind) = marks.line {
                let color = match kind {
                    HunkKind::Added => git_added,
                    _ => git_modified,
                };
                let height = line_height * fold.rows() as f32;
                frame.back.push(fill(
                    Bounds::new(point(git_x, y), size(px(3.), height)),
                    color,
                ));
            }
            let tick = |y: Pixels| {
                let bounds = Bounds::new(point(git_x, y - px(1.5)), size(char_width * 0.6, px(3.)));
                fill(bounds, git_deleted)
            };
            if marks.deleted_above {
                frame.back.push(tick(y));
            }
            if marks.deleted_below {
                frame.back.push(tick(y + line_height * fold.rows() as f32));
            }
            drawn.push((line, y, fold.rows()));
            y += line_height * fold.rows() as f32;
        }

        // Sticky scroll: the first lines of the scopes the top of the
        // view is in, standing over it.
        let mut sticky = Vec::new();
        if let Some(syntax) = self.syntax.as_ref().filter(|_| sticky_scroll) {
            let line_under = |y: Pixels| {
                drawn
                    .iter()
                    .find(|(_, top, rows)| y >= *top && y < *top + line_height * *rows as f32)
                    .map(|(line, ..)| *line)
            };
            let under = |row: usize| {
                let top = bounds.top() + line_height * row as f32;
                let first = line_under(top)?;
                let last = line_under(top + line_height - px(1.)).unwrap_or(first);
                Some((first, last))
            };
            let max = STICKY_MAX.min(visible / 3);
            let scopes = sticky_scopes(max, cursor_line, under, |line| syntax.scopes(buf, line));
            for (row, &(start, end)) in scopes.iter().enumerate() {
                // The tree can be a change behind the text for a paint.
                let line = start.min(buf.last_line());
                // As the line after its scope comes up, pushed up by it,
                // under the one before.
                let mut top = bounds.top() + line_height * row as f32;
                if let Some((_, y, rows)) = drawn.iter().find(|(l, ..)| *l == end) {
                    top = top.min(*y + line_height * (*rows as f32 - 1.));
                }
                let text = buf.line_string(line);
                let (display, offsets, hint_bytes) = self.display_line(line);
                let nchars = offsets.len() - 1;
                let line_byte = buf.char_to_byte(buf.line_start(line));
                let mut capture_at = vec![u32::MAX; text.len()];
                for capture in syntax.captures(buf, line_byte..line_byte + text.len()) {
                    let s = capture.range.start.max(line_byte);
                    let e = capture.range.end.min(line_byte + text.len());
                    if s < e {
                        capture_at[s - line_byte..e - line_byte].fill(capture.index);
                    }
                }
                let mut runs: Vec<TextRun> = Vec::new();
                for (i, (byte, _)) in text.char_indices().enumerate() {
                    if hint_bytes[i] > 0 {
                        runs.push(plain(hint_bytes[i] as usize, muted));
                    }
                    let len = (offsets[i + 1] - offsets[i] - hint_bytes[i + 1]) as usize;
                    let style = styles.get(capture_at[byte] as usize).copied().flatten();
                    let mut run = plain(len, style.and_then(|s| s.color).unwrap_or(fg));
                    if let Some(style) = style {
                        if let Some(weight) = style.font_weight {
                            run.font.weight = weight;
                        }
                        if let Some(font_style) = style.font_style {
                            run.font.style = font_style;
                        }
                    }
                    runs.push(run);
                }
                if hint_bytes[nchars] > 0 {
                    runs.push(plain(hint_bytes[nchars] as usize, muted));
                }
                let shaped = text_system.shape_line(display.into(), font_size, &runs, None);
                let number = line_number(line, cursor_line, relative_number, digits);
                let run = plain(number.len(), line_number_color);
                let number = text_system.shape_line(number.into(), font_size, &[run], None);
                let row_bounds = Bounds::new(
                    point(bounds.left(), top),
                    size(bounds.size.width, line_height),
                );
                frame.sticky.push(StickyRow {
                    bounds: row_bounds,
                    number: (point(bounds.left() + char_width * 1.5, top), number),
                    text: (point(text_origin.x - scroll_left, top), shaped),
                });
                sticky.push(StickyHeader {
                    line,
                    row,
                    bounds: row_bounds,
                });
            }
            // Each over the one after it, and a line under them.
            frame.sticky.reverse();
            if let Some(bottom) = frame.sticky.iter().map(|row| row.bounds.bottom()).max() {
                frame.sticky_edge = Some(fill(
                    Bounds::new(
                        point(bounds.left(), bottom - px(1.)),
                        size(bounds.size.width, px(1.)),
                    ),
                    sticky_edge_color,
                ));
            }
        }

        // A plain scroll position indicator.
        if line_count > visible {
            let height = bounds.size.height;
            let thumb = (height * (visible as f32 / line_count as f32)).max(px(24.));
            let top = (height - thumb) * (self.scroll_top / (line_count - 1) as f32);
            frame.scrollbar = Some(fill(
                Bounds::new(
                    point(bounds.right() - px(6.), bounds.top() + top),
                    size(px(4.), thumb),
                ),
                scrollbar_color,
            ));
        }
        self.sticky = sticky;
        frame
    }

    /// Window position of the top-left corner of the cell at `offset`, as of
    /// the last paint.
    pub fn cell_origin(&self, offset: usize) -> Option<Point<Pixels>> {
        let m = self.metrics.as_ref()?;
        if self.by_rows() {
            let place = self.place_of(offset);
            let top = self.top();
            // Near the view, or nowhere to show it.
            let near = self.visible_lines.max(1) * 2;
            let rows = match self.rows_between(top, place, near) {
                Some(rows) => rows as f32,
                None => -(self.rows_between(place, top, near)? as f32),
            };
            let row_col = self.display_col(self.row_start(place));
            let x = m.char_width * (self.display_col(offset) - row_col) as f32 - self.scroll_left;
            let y = m.line_height * rows;
            return Some(point(m.text_origin.x + x, m.text_origin.y + y));
        }
        let line = self.buf.line_of(offset) as f32;
        let x = m.char_width * self.display_col(offset) as f32 - self.scroll_left;
        let y = m.line_height * (line - self.scroll_top);
        Some(point(m.text_origin.x + x, m.text_origin.y + y))
    }

    /// Window position just below the cursor, for popups.
    fn popup_origin(&self) -> Option<Point<Pixels>> {
        let line_height = self.metrics.as_ref()?.line_height;
        let origin = self.cell_origin(self.vim.cursor)?;
        Some(point(origin.x, origin.y + line_height))
    }

    fn render_completion(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        let menu = self.completion.as_ref()?;
        let origin = self.popup_origin()?;
        let theme = cx.theme();
        let first = (menu.selected + 1).saturating_sub(COMPLETION_ROWS);
        let rows = menu
            .filtered
            .iter()
            .enumerate()
            .skip(first)
            .take(COMPLETION_ROWS)
            .map(|(row, &index)| {
                let item = &menu.items[index];
                let detail = item.detail.clone().unwrap_or_default();
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .h(px(22.))
                    .when(row == menu.selected, |el| el.bg(theme.list_active))
                    .child(div().flex_shrink_0().child(item.label.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    )
            })
            .collect::<Vec<_>>();
        Some(
            deferred(
                anchored().position(origin).snap_to_window().child(
                    div()
                        .occlude()
                        .w(px(420.))
                        .py_1()
                        .bg(theme.popover)
                        .text_color(theme.popover_foreground)
                        .border_1()
                        .border_color(theme.border)
                        .rounded_md()
                        .shadow_md()
                        .text_size(px(12.))
                        .font_family(theme.mono_font_family.clone())
                        .overflow_hidden()
                        .children(rows),
                ),
            )
            .with_priority(1),
        )
    }

    fn render_menu(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        let menu = self.menu.as_ref()?;
        let origin = self.popup_origin()?;
        let theme = cx.theme();
        let titles = self.code_action_titles();
        let rows = titles.into_iter().enumerate().map(|(row, title)| {
            let number = if row < 9 {
                format!("{}", row + 1)
            } else {
                " ".to_string()
            };
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_2()
                .h(px(22.))
                .when(row == menu.selected, |el| el.bg(theme.list_active))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(theme.muted_foreground)
                        .child(number),
                )
                .child(div().whitespace_nowrap().child(title))
        });
        Some(
            deferred(
                anchored().position(origin).snap_to_window().child(
                    div()
                        .occlude()
                        .max_w(px(560.))
                        .py_1()
                        .bg(theme.popover)
                        .text_color(theme.popover_foreground)
                        .border_1()
                        .border_color(theme.border)
                        .rounded_md()
                        .shadow_md()
                        .text_size(px(12.))
                        .overflow_hidden()
                        .children(rows),
                ),
            )
            .with_priority(1),
        )
    }

    /// The signature of the call being typed, over the line it is on.
    fn render_signature(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        let signature = self.signature.as_ref()?;
        let origin = self.cell_origin(self.vim.cursor)?;
        let theme = cx.theme();
        let label = &signature.label;
        let (before, active, after) = match signature.active.clone() {
            Some(range) if label.get(range.clone()).is_some() => (
                label[..range.start].to_string(),
                label[range.clone()].to_string(),
                label[range.end..].to_string(),
            ),
            _ => (label.clone(), String::new(), String::new()),
        };
        Some(
            deferred(
                anchored()
                    .anchor(Anchor::BottomLeft)
                    .position(origin)
                    .snap_to_window()
                    .child(
                        div()
                            .occlude()
                            .flex()
                            .max_w(px(640.))
                            .px_2()
                            .py_1()
                            .bg(theme.popover)
                            .text_color(theme.muted_foreground)
                            .border_1()
                            .border_color(theme.border)
                            .rounded_md()
                            .shadow_md()
                            .text_size(px(12.))
                            .font_family(theme.mono_font_family.clone())
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(before)
                            .child(
                                div()
                                    .text_color(theme.popover_foreground)
                                    .font_weight(FontWeight::BOLD)
                                    .child(active),
                            )
                            .child(after),
                    ),
            )
            .with_priority(1),
        )
    }

    fn render_hover(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        let text = self.hover.clone()?;
        let origin = self.popup_origin()?;
        let theme = cx.theme();
        Some(
            deferred(
                anchored().position(origin).snap_to_window().child(
                    div()
                        .id("hover")
                        .occlude()
                        .max_w(px(640.))
                        .max_h(px(320.))
                        .overflow_y_scroll()
                        .p_2()
                        .bg(theme.popover)
                        .text_color(theme.popover_foreground)
                        .border_1()
                        .border_color(theme.border)
                        .rounded_md()
                        .shadow_md()
                        .text_size(px(12.))
                        .child(TextView::markdown("hover-text", text)),
                ),
            )
            .with_priority(1),
        )
    }
}

/// Everything one paint needs, computed during prepaint.
struct Frame {
    bounds: Bounds<Pixels>,
    text_bounds: Bounds<Pixels>,
    background: Hsla,
    line_height: Pixels,
    /// Full-width decorations behind everything (current line, gutter marks).
    back: Vec<PaintQuad>,
    /// Decorations behind the text, clipped to the text area.
    text_back: Vec<PaintQuad>,
    /// Text, and the row it is cut down to where it is a wrapped line's.
    lines: Vec<(Point<Pixels>, ShapedLine, Option<Bounds<Pixels>>)>,
    gutter: Vec<(Point<Pixels>, ShapedLine)>,
    /// Drawn over the text: bar cursor, scroll indicator, and what is
    /// behind a label.
    front: Vec<PaintQuad>,
    /// The labels of `motion.flash`, over all of it.
    labels: Vec<(Point<Pixels>, ShapedLine)>,
    /// Sticky scroll's lines, over the text and the gutter: the innermost
    /// scope's first, as each is over the one after it.
    sticky: Vec<StickyRow>,
    sticky_edge: Option<PaintQuad>,
    /// The scroll position indicator, over everything.
    scrollbar: Option<PaintQuad>,
}

/// A line of sticky scroll's, the whole width of the view.
struct StickyRow {
    bounds: Bounds<Pixels>,
    number: (Point<Pixels>, ShapedLine),
    text: (Point<Pixels>, ShapedLine),
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
                window.with_content_mask(
                    Some(ContentMask {
                        bounds: self.text_bounds,
                    }),
                    |window| {
                        for quad in self.text_back {
                            window.paint_quad(quad);
                        }
                        for (origin, line, clip) in &self.lines {
                            let mut paint = |window: &mut Window| {
                                let _ = line.paint(
                                    *origin,
                                    line_height,
                                    TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                );
                            };
                            match clip {
                                Some(bounds) => window.with_content_mask(
                                    Some(ContentMask { bounds: *bounds }),
                                    paint,
                                ),
                                None => paint(window),
                            }
                        }
                        for quad in self.front {
                            window.paint_quad(quad);
                        }
                        for (origin, line) in &self.labels {
                            let _ =
                                line.paint(*origin, line_height, TextAlign::Left, None, window, cx);
                        }
                    },
                );
                for (origin, line) in &self.gutter {
                    let _ = line.paint(*origin, line_height, TextAlign::Left, None, window, cx);
                }
                for row in &self.sticky {
                    window.paint_quad(fill(row.bounds, self.background));
                    let (origin, number) = &row.number;
                    let _ = number.paint(*origin, line_height, TextAlign::Left, None, window, cx);
                    let clip = self.text_bounds.intersect(&row.bounds);
                    window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
                        let (origin, text) = &row.text;
                        let _ = text.paint(*origin, line_height, TextAlign::Left, None, window, cx);
                    });
                }
                for quad in self.sticky_edge.into_iter().chain(self.scrollbar) {
                    window.paint_quad(quad);
                }
            },
        );
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // `set-option` refreshes the windows: a new tab width shows at once.
        self.vim_lang.tab_width = self.tab_width(cx);
        self.vim_lang.wrap = self.wrap_cells;
        self.request_hints(cx);
        self.request_tokens(cx);
        self.request_highlights(cx);
        self.request_git_diff(cx);
        let mut key_context = KeyContext::default();
        key_context.add("Editor");
        key_context.set(
            "vim_mode",
            match self.vim.mode {
                Mode::Normal if self.vim.has_pending() => "pending",
                Mode::Normal => "normal",
                Mode::Insert => "insert",
                Mode::Visual | Mode::VisualLine | Mode::VisualBlock => "visual",
                Mode::Command => "command",
            },
        );
        if let Some(preview) = &self.preview {
            return self.render_preview(preview, key_context, cx);
        }
        let prepaint_entity = cx.entity();
        let paint_entity = cx.entity();
        let focus_handle = self.focus_handle.clone();

        div()
            .id("editor")
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context(key_context)
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll(cx.listener(Self::on_scroll_wheel))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        prepaint_entity
                            .update(cx, |editor, cx| editor.layout_frame(bounds, window, cx))
                    },
                    move |_, frame, window, cx| {
                        let text_bounds = frame.text_bounds;
                        frame.paint(window, cx);
                        window.handle_input(
                            &focus_handle,
                            ElementInputHandler::new(text_bounds, paint_entity),
                            cx,
                        );
                    },
                )
                .size_full(),
            )
            .children(self.render_completion(cx))
            .children(self.render_menu(cx))
            .children(self.render_signature(cx))
            .children(self.render_hover(cx))
            .into_any_element()
    }
}

// ---- input method support ------------------------------------------------

impl Editor {
    fn utf16_to_char(&self, offset: usize) -> usize {
        let rope = self.buf.rope();
        rope.utf16_cu_to_char(offset.min(rope.len_utf16_cu()))
    }

    fn char_to_utf16(&self, offset: usize) -> usize {
        let rope = self.buf.rope();
        rope.char_to_utf16_cu(offset.min(rope.len_chars()))
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.utf16_to_char(range.start)..self.utf16_to_char(range.end)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.char_to_utf16(range.start)..self.char_to_utf16(range.end)
    }

    /// The command line's cursor was drawn at `bounds`: the input method
    /// puts its window there, and is told again when that moves. Only
    /// while `:` is typed, as the caret is forgotten when it is done.
    pub(crate) fn place_cmdline_caret(&mut self, bounds: Bounds<Pixels>, window: &mut Window) {
        if self.vim.cmdline().is_some() && self.cmdline_caret != Some(bounds) {
            self.cmdline_caret = Some(bounds);
            window.invalidate_character_coordinates();
        }
    }

    /// While `:` is typed the input method works on the command line, not
    /// the buffer: its text with the composition put in at the cursor, and
    /// where the composition starts, in UTF-16 units of that text.
    fn cmdline_composed(&self) -> Option<(String, usize)> {
        if self.vim.mode != Mode::Command {
            return None;
        }
        let (_, text) = self.vim.cmdline()?;
        let at = self
            .vim
            .cmdline_cursor()
            .unwrap_or(text.len())
            .min(text.len());
        let (head, tail) = text.split_at(at);
        let preedit = self.cmdline_preedit.as_deref().unwrap_or_default();
        Some((format!("{head}{preedit}{tail}"), utf16_len(head)))
    }
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The byte offset in `text` that `units` UTF-16 units in come to.
fn byte_of_utf16(text: &str, units: usize) -> usize {
    let mut seen = 0;
    for (at, c) in text.char_indices() {
        if seen >= units {
            return at;
        }
        seen += c.len_utf16();
    }
    text.len()
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        if let Some((line, _)) = self.cmdline_composed() {
            let start = byte_of_utf16(&line, range_utf16.start);
            let end = byte_of_utf16(&line, range_utf16.end).max(start);
            adjusted_range.replace(utf16_len(&line[..start])..utf16_len(&line[..end]));
            return Some(line[start..end].to_string());
        }
        let range = self.range_from_utf16(&range_utf16);
        adjusted_range.replace(self.range_to_utf16(&range));
        Some(self.buf.slice_string(range))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if let Some((_, at)) = self.cmdline_composed() {
            let caret = at + utf16_len(self.cmdline_preedit.as_deref().unwrap_or_default());
            return Some(UTF16Selection {
                range: caret..caret,
                reversed: false,
            });
        }
        let cursor = self.vim.cursor;
        Some(UTF16Selection {
            range: self.range_to_utf16(&(cursor..cursor)),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        if let Some((_, at)) = self.cmdline_composed() {
            let preedit = self.cmdline_preedit.as_deref()?;
            return Some(at..at + utf16_len(preedit));
        }
        self.marked.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        // Composition on the `:` line is kept as it stands, as it is in
        // the buffer.
        if let Some(preedit) = self.cmdline_preedit.take()
            && self.vim.mode == Mode::Command
        {
            self.type_text(&preedit, cx);
        }
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.vim.mode {
            Mode::Insert => {
                // Drop the composition (or the range the IME names), then
                // let the text through as if typed.
                let range = range_utf16
                    .map(|r| self.range_from_utf16(&r))
                    .or(self.marked.take());
                if let Some(range) = range
                    && !range.is_empty()
                {
                    self.buf.delete(range.clone());
                    self.vim.cursor = range.start;
                }
                self.marked = None;
                self.type_text(text, cx);
            }
            Mode::Command => {
                // What is committed takes the composition's place.
                self.cmdline_preedit = None;
                self.type_text(text, cx);
            }
            _ => {}
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.vim.mode == Mode::Command {
            // Shown on the `:` line, and given to it only when committed.
            self.cmdline_preedit = (!new_text.is_empty()).then(|| new_text.to_string());
            cx.notify();
            return;
        }
        if self.vim.mode != Mode::Insert {
            return;
        }
        let cursor = self.vim.cursor;
        let range = range_utf16
            .map(|r| self.range_from_utf16(&r))
            .or(self.marked.clone())
            .unwrap_or(cursor..cursor);
        self.buf.edit(range.clone(), new_text);
        let len = new_text.chars().count();
        self.marked = (len > 0).then(|| range.start..range.start + len);
        // The IME's selection is in UTF-16 units relative to the new text.
        let caret = new_selected_range
            .map(|sel| {
                let mut units = 0;
                new_text
                    .chars()
                    .take_while(|c| {
                        let before = units;
                        units += c.len_utf16();
                        before < sel.end
                    })
                    .count()
            })
            .unwrap_or(len);
        self.vim.cursor = range.start + caret.min(len);
        self.completion = None;
        self.after_change(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if self.cmdline_composed().is_some() {
            // Never the buffer's cursor: the window would sit up in the
            // text, or in the corner of the screen when that is out of view.
            return Some(self.cmdline_caret.unwrap_or_else(|| {
                Bounds::new(element_bounds.bottom_left(), size(px(1.), px(0.)))
            }));
        }
        let m = self.metrics.as_ref()?;
        let offset = self.utf16_to_char(range_utf16.start);
        let origin = self.cell_origin(offset)?;
        Some(Bounds::new(origin, size(m.char_width, m.line_height)))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        if self.cmdline_composed().is_some() {
            return None;
        }
        self.offset_at(point, window)
            .map(|offset| self.char_to_utf16(offset))
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        // Outside insert mode keys are commands, not text; keeping the input
        // method out of it stops it swallowing them.
        matches!(self.vim.mode, Mode::Insert | Mode::Command)
    }
}

pub(crate) fn register(commands: &mut Commands) {
    use Handler::Editor as E;
    commands.register(
        "fold.toggle",
        "Open the closed fold on the cursor's line, or close the fold it is in",
        E(|editor, _, cx| editor.fold_toggle(cx)),
    );
    commands.register(
        "fold.open",
        "Open the closed fold on the cursor's line",
        E(|editor, _, cx| {
            if !editor.fold_open(cx) {
                cx.emit(EditorEvent::Message("No closed fold here".into()));
            }
        }),
    );
    commands.register(
        "fold.close",
        "Close the fold the cursor's line is in, or the one around a closed one",
        E(|editor, _, cx| editor.fold_close(cx)),
    );
    commands.register(
        "fold.open_all",
        "Open every fold",
        E(|editor, _, cx| editor.fold_open_all(cx)),
    );
    commands.register(
        "fold.close_all",
        "Close every fold, by the syntax tree or else by indent",
        E(|editor, _, cx| editor.fold_close_all(cx)),
    );
    commands.register(
        "editor.goto_definition",
        "Go to the definition of the symbol under the cursor",
        E(|editor, _, cx| editor.goto_definition(cx)),
    );
    commands.register(
        "editor.goto_declaration",
        "Go to the declaration of the symbol under the cursor",
        E(|editor, _, cx| editor.goto_declaration(cx)),
    );
    commands.register(
        "editor.goto_type_definition",
        "Go to the definition of the type of the symbol under the cursor",
        E(|editor, _, cx| editor.goto_type_definition(cx)),
    );
    commands.register(
        "editor.goto_implementation",
        "Go to the implementations of the symbol under the cursor",
        E(|editor, _, cx| editor.goto_implementation(cx)),
    );
    commands.register(
        "editor.code_lens",
        "List the lenses of the file, such as running a test, to run one",
        E(|editor, _, cx| editor.code_lens(cx)),
    );
    commands.register(
        "editor.incoming_calls",
        "List what calls the function under the cursor",
        E(|editor, _, cx| editor.show_calls(true, cx)),
    );
    commands.register(
        "editor.outgoing_calls",
        "List what the function under the cursor calls",
        E(|editor, _, cx| editor.show_calls(false, cx)),
    );
    commands.register(
        "editor.document_symbols",
        "List the symbols of the file",
        E(|editor, _, cx| editor.document_symbols(cx)),
    );
    commands.register(
        "editor.references",
        "List the references to the symbol under the cursor",
        E(|editor, _, cx| editor.find_references(cx)),
    );
    commands.register(
        "editor.hover",
        "Show documentation for the symbol under the cursor",
        E(|editor, _, cx| editor.show_hover(cx)),
    );
    commands.register(
        "editor.next_diagnostic",
        "Go to the next diagnostic",
        E(|editor, _, cx| editor.jump_to_diagnostic(true, cx)),
    );
    commands.register(
        "editor.prev_diagnostic",
        "Go to the previous diagnostic",
        E(|editor, _, cx| editor.jump_to_diagnostic(false, cx)),
    );
    commands.register(
        "editor.next_hunk",
        "Go to the next change from HEAD that the gutter marks",
        E(|editor, _, cx| editor.jump_to_hunk(true, cx)),
    );
    commands.register(
        "editor.prev_hunk",
        "Go to the previous change from HEAD that the gutter marks",
        E(|editor, _, cx| editor.jump_to_hunk(false, cx)),
    );
    commands.register(
        "editor.complete",
        "Ask the language server for completions",
        E(|editor, _, cx| editor.request_completion(None, cx)),
    );
    commands.register(
        "editor.rename",
        "Rename the symbol under the cursor to the name given",
        E(|editor, args, cx| editor.rename(&args.arg, cx)),
    );
    commands.register(
        "editor.code_action",
        "List what the language server can do here, to choose from",
        E(|editor, _, cx| editor.code_action(cx)),
    );
    commands.register(
        "editor.format",
        "Have the language server format the file",
        E(|editor, _, cx| editor.format(cx)),
    );
    commands.register(
        "editor.signature_help",
        "Show the signature of what is being called",
        E(|editor, _, cx| editor.request_signature(cx)),
    );
    commands.register(
        "editor.paste",
        "Paste from the system clipboard",
        E(|editor, _, cx| editor.paste(cx)),
    );
    commands.register(
        "editor.copy",
        "Copy the selection to the system clipboard",
        E(|editor, _, cx| editor.copy(cx)),
    );
}

#[cfg(test)]
mod tests {
    use super::{Fold, expand_tabs, hovers_markdown, insert_labels, insert_texts};
    use gpui::{point, px};
    use lsp_types as lsp;

    /// What each server says is shown apart, in the order they were
    /// asked; one that said nothing leaves no gap.
    #[test]
    fn hovers_of_every_server_are_shown() {
        let markup = |value: &str| lsp::Hover {
            contents: lsp::HoverContents::Markup(lsp::MarkupContent {
                kind: lsp::MarkupKind::Markdown,
                value: value.into(),
            }),
            range: None,
        };
        let hovers = vec![
            markup("```ts\nconst a: number\n```"),
            markup("  "),
            lsp::Hover {
                contents: lsp::HoverContents::Scalar(lsp::MarkedString::LanguageString(
                    lsp::LanguageString {
                        language: "css".into(),
                        value: ".flex { display: flex; }".into(),
                    },
                )),
                range: None,
            },
        ];
        assert_eq!(
            hovers_markdown(hovers).as_deref(),
            Some("```ts\nconst a: number\n```\n\n---\n\n```css\n.flex { display: flex; }\n```")
        );
        assert_eq!(hovers_markdown(vec![markup("")]), None);
        assert_eq!(hovers_markdown(Vec::new()), None);
    }

    #[test]
    fn a_span_is_cut_where_the_rows_are() {
        let fold = Fold {
            starts: vec![px(0.), px(50.), px(100.)],
            left: px(10.),
            top: px(200.),
            line_height: px(20.),
        };
        let spans: Vec<_> = fold
            .spans(px(40.), px(120.))
            .iter()
            .map(|b| {
                (
                    f32::from(b.left()),
                    f32::from(b.top()),
                    f32::from(b.size.width),
                )
            })
            .collect();
        // The end of the first row, all of the second, the start of the last.
        assert_eq!(
            spans,
            [(50., 200., 10.), (10., 220., 50.), (10., 240., 20.)]
        );
        // Within a row, and at a row's start: on that row.
        assert_eq!(fold.spans(px(60.), px(70.)).len(), 1);
        assert_eq!(fold.at(px(50.)), point(px(10.), px(220.)));
        // Past the last row's start: on the last row, however far.
        assert_eq!(fold.at(px(300.)), point(px(210.), px(240.)));
    }

    #[test]
    fn hints_stand_before_labels_between_the_characters() {
        // `ab`: a hint before `b`, and a flash label there too.
        let (mut display, mut offsets) = expand_tabs("ab", 4);
        let hints = insert_texts(&mut display, &mut offsets, &[(1, ": T")]);
        let labels = insert_labels(&mut display, &mut offsets, &[(1, 'x')]);
        assert_eq!(display, "a: Txb");
        assert_eq!((&hints, &labels), (&vec![0, 3, 0], &vec![0, 1, 0]));
        // `b` is past both; `a` alone between offsets 0 and 1.
        assert_eq!(offsets, [0, 5, 6]);
        assert_eq!(offsets[1] - offsets[0] - hints[1] - labels[1], 1);
    }

    #[test]
    fn labels_stand_between_the_characters() {
        // A tab, then `ab`: the labels go before `a`, two of them, and
        // after `b`, at the end of the line.
        let (mut display, mut offsets) = expand_tabs("\tab", 4);
        assert_eq!(
            (display.as_str(), offsets.as_slice()),
            ("    ab", &[0, 4, 5, 6][..])
        );
        let before = insert_labels(&mut display, &mut offsets, &[(1, 'x'), (1, 'y'), (3, 'z')]);
        assert_eq!(display, "    xyabz");
        // Each column is where its character is, past its labels.
        assert_eq!(offsets, [0, 6, 7, 9]);
        assert_eq!(before, [0, 2, 0, 1]);

        // A label of more than a byte, and none at all.
        let (mut display, mut offsets) = expand_tabs("ab", 4);
        let before = insert_labels(&mut display, &mut offsets, &[(0, 'é')]);
        assert_eq!(
            (display.as_str(), offsets.as_slice()),
            ("éab", &[2, 3, 4][..])
        );
        assert_eq!(before, [2, 0, 0]);
        assert_eq!(insert_labels(&mut display, &mut offsets, &[]), [0, 0, 0]);
        assert_eq!(display, "éab");
    }
}
