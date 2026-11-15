//! The editor view: one buffer, its vim state, and everything needed to
//! draw it and feed it input.

use crate::commands::{Args, Commands, Handler};
use crate::lsp::{LspEvent, LspStore, uri_to_path};
use crate::syntax::{self, LangInfo, Syntax};
use crate::{Settings, VimGlobal};
use anyhow::Context as _;
use gpui::component::ActiveTheme as _;
use gpui::component::text::TextView;
use gpui::prelude::*;
use gpui::*;
use lsp_types as lsp;
use med_core::Buffer;
use med_vim::{Clipboard, Ctx, Effect, Key, Mode, PendingKeys, Scroll, Viewport, Vim};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Lines kept between the cursor and the top/bottom edge.
const SCROLL_OFF: usize = 3;
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
    syntax: Option<Syntax>,
    lsp: Option<Entity<LspStore>>,
    lsp_version: i32,
    /// First visible line; fractional while wheel-scrolling.
    scroll_top: f32,
    scroll_left: Pixels,
    visible_lines: usize,
    metrics: Option<Metrics>,
    /// IME composition in progress, as a char range of the buffer.
    marked: Option<Range<usize>>,
    diagnostics: Vec<Diag>,
    completion: Option<CompletionMenu>,
    completion_task: Option<Task<()>>,
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
    _subscriptions: Vec<Subscription>,
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
            syntax: None,
            lsp,
            lsp_version: 0,
            scroll_top: 0.,
            scroll_left: px(0.),
            visible_lines: 0,
            metrics: None,
            marked: None,
            diagnostics: Vec::new(),
            completion: None,
            completion_task: None,
            key_timeout: None,
            pending_keys: None,
            hover: None,
            request_task: None,
            menu: None,
            signature: None,
            signature_task: None,
            drag_anchor: None,
            mtime: None,
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
            this.set_path(path, cx);
        }
        this
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
        self.vim_lang = med_vim::Lang {
            tab_width: detected
                .map(|d| d.1)
                .or(self.lang.map(|l| l.tab_width))
                .unwrap_or(4),
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
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    pub fn title(&self) -> String {
        match &self.path {
            Some(path) => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            None => "[No Name]".to_string(),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.buf.is_dirty()
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

    pub fn save(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let path = self.path.clone().context("No file name")?;
        if cx
            .try_global::<Settings>()
            .is_some_and(|s| s.format_on_save)
        {
            self.format_now(cx);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, self.buf.to_disk_string())
            .with_context(|| format!("Cannot write {}", path.display()))?;
        self.buf.mark_saved();
        self.mtime = mtime(&path);
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
        let line = self.buf.line_of(self.vim.cursor);
        let half = self.visible_lines.max(20) / 2;
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
        let viewport = Viewport {
            top: self.scroll_top.max(0.) as usize,
            height: self.visible_lines.max(1),
        };
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
                Effect::Scroll(scroll) => self.scroll(scroll),
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

    fn scroll(&mut self, scroll: Scroll) {
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
                // would leave it behind.
                let top = self.scroll_top as usize;
                let bottom = top + self.visible_lines.max(1) - 1;
                let cursor_line = self.buf.line_of(self.vim.cursor);
                let target = cursor_line.clamp(top, bottom.min(self.buf.last_line()));
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
        // Marks follow the edits made here as they do the engine's own.
        self.vim.sync(&self.buf);
        let edits = self.buf.take_edits();
        if !edits.is_empty() {
            if let Some(syntax) = &mut self.syntax {
                syntax.update(&self.buf, &edits);
            }
            for edit in &edits {
                let delta = edit.new_end_char as isize - edit.old_end_char as isize;
                let shift = |offset: usize| (offset as isize + delta).max(0) as usize;
                for d in &mut self.diagnostics {
                    if d.start >= edit.old_end_char {
                        d.start = shift(d.start);
                        d.end = shift(d.end);
                    } else if d.end >= edit.old_end_char {
                        d.end = shift(d.end).max(d.start);
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
        self.ensure_cursor_visible();
        self.report_pending(cx);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    fn display_col(&self, offset: usize) -> usize {
        let line = self.buf.line_of(offset);
        let tab = self.vim_lang.tab_width.max(1);
        let mut col = 0;
        for c in self.buf.slice(self.buf.line_start(line)..offset).chars() {
            col += match c {
                '\t' => tab - col % tab,
                // CJK and other wide glyphs take about two cells.
                c if (c as u32) >= 0x1100 => 2,
                _ => 1,
            };
        }
        col
    }

    fn ensure_cursor_visible(&mut self) {
        let line = self.buf.line_of(self.vim.cursor);
        let visible = self.visible_lines.max(1);
        let margin = SCROLL_OFF.min(visible.saturating_sub(1) / 2);
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
        let lang = &self.vim_lang;
        lsp.read(cx)
            .formatting(path, lang.tab_width as u32, lang.expand_tab)
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
        let needle = typed.to_lowercase();
        let mut scored: Vec<(u8, &str, usize)> = menu
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let text = item.filter_text.as_deref().unwrap_or(&item.label);
                let lower = text.to_lowercase();
                let score = if needle.is_empty() {
                    1
                } else if text.starts_with(&typed) {
                    0
                } else if lower.starts_with(&needle) {
                    1
                } else {
                    // Subsequence match.
                    let mut chars = lower.chars();
                    if needle.chars().all(|n| chars.any(|c| c == n)) {
                        2
                    } else {
                        return None;
                    }
                };
                let order = item.sort_text.as_deref().unwrap_or(&item.label);
                Some((score, order, index))
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

    /// A key press as the user makes it: the completion menu gets first
    /// refusal, then the vim engine.
    pub fn press(&mut self, key: Key, cx: &mut Context<Self>) {
        if !self.menu_key(&key, cx) && !self.completion_key(&key, cx) {
            self.handle_key(key, cx);
        }
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
        let row = (position.y - m.text_origin.y) / m.line_height + self.scroll_top;
        let line = (row.max(0.) as usize).min(self.buf.last_line());
        let x = (position.x - m.text_origin.x + self.scroll_left).max(px(0.));
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
        let index = shaped.closest_index_for_x(x) as u32;
        let col = offsets.partition_point(|&b| b <= index).saturating_sub(1);
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
        let diag_colors = [theme.danger, theme.warning, theme.info, muted];
        let search_bg = theme.warning.opacity(0.35);
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

        let first_layout = self.visible_lines != visible || self.metrics.is_none();
        self.visible_lines = visible;
        self.metrics = Some(Metrics {
            text_origin,
            text_width,
            line_height,
            char_width,
            font_size,
            font: base_font.clone(),
        });
        self.scroll_top = self.scroll_top.clamp(0., (line_count - 1) as f32);
        if first_layout {
            self.ensure_cursor_visible();
        }
        let buf = &self.buf;

        let first = self.scroll_top.floor() as usize;
        let last = (first + visible + 2).min(line_count);
        let y0 = bounds.top() - line_height * self.scroll_top.fract();
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
        };

        for line in first..last {
            let y = y0 + line_height * (line - first) as f32;
            let ls = buf.line_start(line);
            let text = buf.line_string(line);
            let (display, offsets) = expand_tabs(&text, tab_width);
            let nchars = offsets.len() - 1;
            let le = ls + nchars;
            let line_byte = buf.char_to_byte(ls);

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

            let mut runs: Vec<TextRun> = Vec::new();
            let mut last_key = None;
            for (i, (byte, _)) in text.char_indices().enumerate() {
                let len = (offsets[i + 1] - offsets[i]) as usize;
                let offset = ls + i;
                let inverse = block_cursor && offset == cursor;
                let marked = self.marked.as_ref().is_some_and(|m| m.contains(&offset));
                let key = (capture_at[byte], severity_at[i], marked, inverse);
                if last_key == Some(key) {
                    if let Some(run) = runs.last_mut() {
                        run.len += len;
                    }
                    continue;
                }
                last_key = Some(key);
                let style = styles.get(key.0 as usize).copied().flatten();
                let mut run = plain(len, style.and_then(|s| s.color).unwrap_or(fg));
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
            let shaped = text_system.shape_line(display.into(), font_size, &runs, None);
            let x_of = |col: usize| shaped.x_for_index(offsets[col.min(nchars)] as usize);
            let line_x = text_origin.x - scroll_left;
            let span = |from: usize, to: usize, through_newline: bool, color: Hsla| {
                let x1 = x_of(from);
                let mut x2 = x_of(to);
                if through_newline {
                    x2 += char_width;
                }
                fill(
                    Bounds::new(point(line_x + x1, y), size(x2 - x1, line_height)),
                    color,
                )
            };

            if line == cursor_line && !mode.is_visual() {
                frame.back.push(fill(
                    Bounds::new(
                        point(bounds.left(), y),
                        size(bounds.size.width, line_height),
                    ),
                    active_line_bg,
                ));
            }
            if let Some(regex) = &search {
                for m in regex.find_iter(&text) {
                    if m.is_empty() {
                        continue;
                    }
                    let from = text[..m.start()].chars().count();
                    let to = from + text[m.start()..m.end()].chars().count();
                    frame.text_back.push(span(from, to, false, search_bg));
                }
            }
            for &(s, e) in &selections {
                let (a, b) = (s.max(ls), e.min(le + 1));
                if a < b {
                    frame
                        .text_back
                        .push(span(a - ls, (b - ls).min(nchars), b > le, selection_bg));
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
                let cell = Bounds::new(point(line_x + x, y), size(width, line_height));
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
            let text_end = shaped.width;
            frame.lines.push((point(line_x, y), shaped));
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
                frame
                    .lines
                    .push((point(line_x + text_end + char_width * 3., y), shaped));
            }

            // Gutter: line number, tinted by the worst diagnostic.
            let number = format!("{:>digits$}", line + 1);
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
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    gutter: Vec<(Point<Pixels>, ShapedLine)>,
    /// Drawn over the text: bar cursor, scroll indicator.
    front: Vec<PaintQuad>,
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
                        for (origin, line) in &self.lines {
                            let _ =
                                line.paint(*origin, line_height, TextAlign::Left, None, window, cx);
                        }
                        for quad in self.front {
                            window.paint_quad(quad);
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
        let line = self.buf.line_of(offset) as f32;
        let x = m.char_width * self.display_col(offset) as f32 - self.scroll_left;
        let y = m.line_height * (line - self.scroll_top);
        Some(Bounds::new(
            point(m.text_origin.x + x, m.text_origin.y + y),
            size(m.char_width, m.line_height),
        ))
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
