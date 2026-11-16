//! The editor view: one buffer, its vim state, and everything needed to
//! draw it and feed it input.

use crate::commands::{Args, Commands, Handler};
use crate::lsp::{LspEvent, LspStore, uri_to_path};
use crate::syntax::{self, LangInfo, Syntax};
use crate::{Settings, VimGlobal};
use anyhow::Context as _;
use gpui::component::ActiveTheme as _;
use gpui::component::text::{RangeHighlight, TextView, TextViewState};
use gpui::prelude::*;
use gpui::*;
use lsp_types as lsp;
use med_core::Buffer;
use med_vim::{Clipboard, Ctx, Effect, FlashMatch, Key, Mode, PendingKeys, Scroll, Viewport, Vim};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const COMPLETION_ROWS: usize = 10;

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
    References(Vec<lsp::Location>),
    /// Edits a language server asked for, in this file or in others.
    WorkspaceEdit(lsp::WorkspaceEdit),
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

struct CompletionMenu {
    items: Vec<lsp::CompletionItem>,
    /// Indices into `items` that match what has been typed, best first.
    filtered: Vec<usize>,
    selected: usize,
    /// Start of the word being completed.
    start: usize,
}

/// The code actions on offer, to choose one from.
struct ActionMenu {
    actions: Vec<lsp::CodeActionOrCommand>,
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
    /// IME composition in progress, as a char range of the buffer.
    marked: Option<Range<usize>>,
    diagnostics: Vec<Diag>,
    /// What programs marked on the text, by the group they gave.
    decorations: Vec<(SharedString, Vec<Decoration>)>,
    completion: Option<CompletionMenu>,
    completion_task: Option<Task<()>>,
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

/// Cells a char takes at display column `col`: a tab to the next stop,
/// CJK and other wide glyphs about two, the rest one.
fn cell_width(c: char, col: usize, tab_width: usize) -> usize {
    match c {
        '\t' => tab_width - col % tab_width,
        c if (c as u32) >= 0x1100 => 2,
        _ => 1,
    }
}

/// Where a line's screen rows start, as char columns: `[0]` for a line
/// on one row. With `cells` to a row, a char that does not fit where it
/// is goes to the next row whole, and a line that fills its last row has
/// one more, empty, for the place after its end.
pub(crate) fn wrap_breaks(text: &str, tab_width: usize, cells: usize) -> Vec<usize> {
    let tab_width = tab_width.max(1);
    let cells = cells.max(1);
    let mut breaks = vec![0];
    let (mut col, mut used) = (0, 0);
    let mut chars = 0;
    for (i, c) in text.chars().enumerate() {
        let width = cell_width(c, col, tab_width);
        if used > 0 && used + width > cells {
            breaks.push(i);
            used = 0;
        }
        col += width;
        used += width;
        chars = i + 1;
    }
    if used >= cells {
        breaks.push(chars);
    }
    breaks
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
        // Back through what the command line completes to.
        "tab" if m.shift && mode == Mode::Command => Some(Key::BackTab),
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
            marked: None,
            diagnostics: Vec::new(),
            decorations: Vec::new(),
            completion: None,
            completion_task: None,
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
            scratch: None,
            preview: None,
            _subscriptions: Vec::new(),
        };
        if let Some(lsp) = this.lsp.clone() {
            this._subscriptions
                .push(cx.subscribe(&lsp, |this, store, event: &LspEvent, cx| {
                    if let LspEvent::Diagnostics(path) = event
                        && this.path.as_deref() == Some(path.as_path())
                    {
                        let diagnostics = store.read(cx).diagnostics(path).to_vec();
                        this.set_diagnostics(&diagnostics);
                        cx.notify();
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
        self.lang = syntax::lang_for_path(&path);
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
        };
        self.mtime = mtime(&path);
        self.diagnostics.clear();
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

    /// The empty buffer that stands in when no file is open: no file, no
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
        if self.wrap_cells.is_none() {
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
        if self.wrap_cells.is_some() {
            self.set_top(self.rows_back(self.place_of(self.vim.cursor), half));
            return;
        }
        let line = self.buf.line_of(self.vim.cursor);
        self.scroll_top = line.saturating_sub(half) as f32;
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
            (Some(lsp), Some(path)) => lsp.read(cx).completion_triggers(path).contains(&last),
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

    // ---- rows of wrapped lines ------------------------------------------
    //
    // Where lines wrap, a place on screen is a line and a row of it, and
    // the view starts at one. Unwrapped, every line is one row.

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

    /// The line and row an offset is on.
    fn place_of(&self, offset: usize) -> (usize, usize) {
        let line = self.buf.line_of(offset);
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
            if n == 0 || line == last {
                break;
            }
            line += 1;
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
            if n == 0 || line == 0 {
                break;
            }
            line -= 1;
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
        if to < from || to.0 > from.0 + limit {
            return None;
        }
        // A line at a time: a long line is broken into rows once.
        let (mut rows, mut row) = (0, from.1);
        for line in from.0..to.0 {
            rows += self.rows(line) - row;
            row = 0;
            if rows > limit {
                return None;
            }
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
                let end = (self.buf.last_line(), self.rows(self.buf.last_line()) - 1);
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
        if self.wrap_cells.is_some() {
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
        // What `Tab` went through was for the command line just closed.
        if self.vim.cmdline().is_none() {
            self.cmdline_completion = None;
        }
        // Marks follow the edits made here as they do the engine's own.
        self.vim.sync(&self.buf);
        let edits = self.buf.take_edits();
        if !edits.is_empty() {
            if let Some(syntax) = &mut self.syntax {
                syntax.update(&self.buf, &edits);
            }
            for edit in &edits {
                for d in &mut self.diagnostics {
                    shift_range(&mut d.start, &mut d.end, edit);
                }
                for (_, group) in &mut self.decorations {
                    for d in group {
                        shift_range(&mut d.start, &mut d.end, edit);
                    }
                }
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
        let mut col = 0;
        for c in self.buf.slice(self.buf.line_start(line)..offset).chars() {
            col += cell_width(c, col, tab);
        }
        col
    }

    fn ensure_cursor_visible(&mut self, cx: &App) {
        if self.wrap_cells.is_some() {
            self.scroll_left = px(0.);
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
            return;
        }
        let line = self.buf.line_of(self.vim.cursor);
        let visible = self.visible_lines.max(1);
        let margin = self.scroll_margin(cx);
        let top = self.scroll_top;
        if (line as f32) < top + margin as f32 {
            self.scroll_top = line.saturating_sub(margin) as f32;
        } else if line as f32 > top + (visible - 1 - margin) as f32 {
            self.scroll_top = (line + margin + 1).saturating_sub(visible) as f32;
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
        let (Some(lsp), Some(path)) = (&self.lsp, &self.path) else {
            return;
        };
        let position = self.lsp_position(self.vim.cursor);
        let Some(request) = lsp.read(cx).definition(path, position) else {
            cx.emit(EditorEvent::Message("No language server".into()));
            return;
        };
        let from = self.vim.cursor;
        self.request_task = Some(cx.spawn(async move |this, cx| {
            let location = match request.await {
                Ok(Some(lsp::GotoDefinitionResponse::Scalar(location))) => Some(location),
                Ok(Some(lsp::GotoDefinitionResponse::Array(locations))) => {
                    locations.into_iter().next()
                }
                Ok(Some(lsp::GotoDefinitionResponse::Link(links))) => {
                    links.into_iter().next().map(|link| lsp::Location {
                        uri: link.target_uri,
                        range: link.target_selection_range,
                    })
                }
                _ => None,
            };
            this.update(cx, |_, cx| {
                match location.and_then(|l| Some((uri_to_path(&l.uri)?, l.range.start))) {
                    Some((path, position)) => {
                        cx.emit(EditorEvent::PushJump(from));
                        cx.emit(EditorEvent::OpenLocation { path, position });
                    }
                    None => cx.emit(EditorEvent::Message("No definition found".into())),
                }
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
                    cx.emit(EditorEvent::References(locations));
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
            let Ok(Some(hover)) = request.await else {
                return;
            };
            let mut text = hover_markdown(hover);
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
        if self.wrap_cells.is_some() {
            return (self.top().0, self.bottom().0);
        }
        let first = (self.scroll_top.max(0.) as usize).min(self.buf.last_line());
        let last = first + self.visible_lines.max(1) - 1;
        (first, last.min(self.buf.last_line()))
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
            let actions = request.await.ok().flatten().unwrap_or_default();
            let offered = |action: &lsp::CodeActionOrCommand| match action {
                lsp::CodeActionOrCommand::CodeAction(action) => action.disabled.is_none(),
                lsp::CodeActionOrCommand::Command(_) => true,
            };
            let actions: Vec<_> = actions.into_iter().filter(offered).collect();
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
        let title = |action: &lsp::CodeActionOrCommand| match action {
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
        let (edit, command) = match action {
            lsp::CodeActionOrCommand::CodeAction(action) => (action.edit, action.command),
            lsp::CodeActionOrCommand::Command(command) => (None, Some(command)),
        };
        if let Some(edit) = edit {
            cx.emit(EditorEvent::WorkspaceEdit(edit));
        }
        // What the command edits comes back through the server.
        if let (Some(command), Some(lsp), Some(path)) = (command, &self.lsp, &self.path) {
            lsp.read(cx).execute_command(path, command);
        }
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
            actions,
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
            let Ok(Some(response)) = request.await else {
                return;
            };
            let items = match response {
                lsp::CompletionResponse::Array(items) => items,
                lsp::CompletionResponse::List(list) => list.items,
            };
            this.update(cx, |this, cx| {
                if this.buf.version() != version || this.vim.mode != Mode::Insert {
                    return;
                }
                let start = this.word_start(this.vim.cursor);
                this.completion = Some(CompletionMenu {
                    items,
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
        // Best score first; the server's `sort_text` breaks ties.
        let mut scored: Vec<(std::cmp::Reverse<u16>, &str, usize)> = matches
            .into_iter()
            .map(|m| {
                let index = m.index as usize;
                let item = &menu.items[index];
                let order = item.sort_text.as_deref().unwrap_or(&item.label);
                (std::cmp::Reverse(m.score), order, index)
            })
            .collect();
        scored.sort();
        let filtered: Vec<usize> = scored.into_iter().map(|(_, _, i)| i).take(200).collect();
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
            Key::Ctrl('p') | Key::Up => menu.selected = (menu.selected + count - 1) % count,
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
        let mut new_cursor = start + text.chars().count();
        for (s, e, text) in extra {
            if e <= start {
                self.buf.edit(s..e, &text);
                new_cursor = new_cursor + text.chars().count() - (e - s);
            }
        }
        self.vim.cursor = new_cursor;
        self.after_change(cx);
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
            && !self.cmdline_complete(&key, cx)
        {
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
        let (line, row) = if self.wrap_cells.is_some() {
            let rows = ((position.y - m.text_origin.y) / m.line_height).max(0.) as usize;
            self.rows_on(self.top(), rows)
        } else {
            let row = (position.y - m.text_origin.y) / m.line_height + self.scroll_top;
            ((row.max(0.) as usize).min(self.buf.last_line()), 0)
        };
        let breaks = self.breaks(line);
        let (display, offsets) = expand_tabs(&self.buf.line_string(line), self.vim_lang.tab_width);
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
        if self.wrap_cells.is_some() {
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
        let diag_colors = [theme.danger, theme.warning, theme.info, muted];
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
        self.metrics = Some(Metrics {
            text_origin,
            text_width,
            line_height,
            char_width,
            font_size,
            font: base_font.clone(),
        });
        self.scroll_top = self.scroll_top.clamp(0., (line_count - 1) as f32);
        if wrap_cells.is_some() {
            // Whole lines and rows of them: the wheel keeps its own part.
            self.scroll_top = self.scroll_top.floor();
            self.top_row = self.top_row.min(self.rows(self.top().0) - 1);
            self.scroll_left = px(0.);
        } else {
            self.top_row = 0;
        }
        if first_layout {
            self.ensure_cursor_visible(cx);
        }
        let buf = &self.buf;

        let first = self.scroll_top.floor() as usize;
        let last = (first + visible + 2).min(line_count);
        let y0 = match wrap_cells {
            Some(_) => bounds.top() - line_height * self.top_row as f32,
            None => bounds.top() - line_height * self.scroll_top.fract(),
        };
        let scroll_left = self.scroll_left;

        let byte_start = buf.char_to_byte(buf.line_start(first));
        let byte_end = buf.char_to_byte(buf.line_end(last - 1));
        let (captures, styles) = match &self.syntax {
            Some(syntax) => (
                syntax.captures(buf, byte_start..byte_end),
                syntax
                    .capture_names()
                    .iter()
                    .map(|name| highlight.style(name))
                    .collect::<Vec<Option<HighlightStyle>>>(),
            ),
            None => (Vec::new(), Vec::new()),
        };

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
        let marks: Vec<Mark> = self
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
        };
        for band in column_bands(&color_columns, text_bounds, char_width, scroll_left) {
            frame.text_back.push(fill(band, color_column_bg));
        }

        let mut y = y0;
        for line in first..last {
            if y >= bounds.bottom() {
                break;
            }
            let ls = buf.line_start(line);
            let text = buf.line_string(line);
            let (mut display, mut offsets) = expand_tabs(&text, tab_width);
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
                if label_bytes[i] > 0 {
                    runs.push(label_run(label_bytes[i]));
                    last_key = None;
                }
                let len = (offsets[i + 1] - offsets[i] - label_bytes[i + 1]) as usize;
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

            // Gutter: line number, tinted by the worst diagnostic.
            let number = line_number(line, cursor_line, relative_number, digits);
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
                .find_map(|m| Some((m.sign.as_ref()?, m.color)));
            if let Some((sign, color)) = sign {
                let sign: String = sign.chars().filter(|c| !c.is_control()).take(1).collect();
                let run = plain(sign.len(), color.unwrap_or(fg));
                let shaped = text_system.shape_line(sign.into(), font_size, &[run], None);
                frame
                    .gutter
                    .push((point(bounds.left() + char_width * 0.5, y), shaped));
            }
            y += line_height * fold.rows() as f32;
        }

        // A plain scroll position indicator.
        if line_count > visible {
            let height = bounds.size.height;
            let thumb = (height * (visible as f32 / line_count as f32)).max(px(24.));
            let top = (height - thumb) * (self.scroll_top / (line_count - 1) as f32);
            frame.front.push(fill(
                Bounds::new(
                    point(bounds.right() - px(6.), bounds.top() + top),
                    size(px(4.), thumb),
                ),
                scrollbar_color,
            ));
        }
        frame
    }

    /// Window position of the top-left corner of the cell at `offset`, as of
    /// the last paint.
    pub fn cell_origin(&self, offset: usize) -> Option<Point<Pixels>> {
        let m = self.metrics.as_ref()?;
        if self.wrap_cells.is_some() {
            let place = self.place_of(offset);
            let top = self.top();
            // Near the view, or nowhere to show it.
            let near = self.visible_lines.max(1) * 2;
            let rows = match self.rows_between(top, place, near) {
                Some(rows) => rows as f32,
                None => -(self.rows_between(place, top, near)? as f32),
            };
            let row_col = self.display_col(self.row_start(place));
            let x = m.char_width * (self.display_col(offset) - row_col) as f32;
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
            },
        );
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // `set-option` refreshes the windows: a new tab width shows at once.
        self.vim_lang.tab_width = self.tab_width(cx);
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
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
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
        let cursor = self.vim.cursor;
        Some(UTF16Selection {
            range: self.range_to_utf16(&(cursor..cursor)),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
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
            Mode::Command => self.type_text(text, cx),
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
        _element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
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
        "editor.goto_definition",
        "Go to the definition of the symbol under the cursor",
        E(|editor, _, cx| editor.goto_definition(cx)),
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
    use super::{Fold, expand_tabs, insert_labels};
    use gpui::{point, px};

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
