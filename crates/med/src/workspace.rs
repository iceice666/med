//! The window's root view: tabs of editors, the status bar, and the ex
//! commands that act on them.

use crate::commands::{Args, Commands, Handler, RunCommand};
use crate::editor::{Editor, EditorEvent};
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::git_view::{GitEvent, GitView};
use crate::lsp::{LspEvent, LspStore, uri_to_path};
use crate::panel::Layouts;
use crate::picker::{Item, Picker, PickerEvent, PickerMode};
use gpui::component::ActiveTheme as _;
use gpui::prelude::*;
use gpui::*;
use lsp_types as lsp;
use med_vim::Mode;
use std::path::{Path, PathBuf};

pub struct Workspace {
    root: PathBuf,
    focus_handle: FocusHandle,
    lsp: Entity<LspStore>,
    pub(crate) tabs: Vec<Entity<Editor>>,
    pub(crate) active: usize,
    pub(crate) file_tree: Entity<FileTree>,
    show_tree: bool,
    pub(crate) git: Entity<GitView>,
    pub(crate) show_git: bool,
    pub(crate) picker: Option<Entity<Picker>>,
    _picker_subscription: Option<Subscription>,
    /// One-line feedback shown in the status bar until the next key.
    pub(crate) message: Option<SharedString>,
    /// Places `ctrl-o` / `ctrl-i` walk through.
    jumps: Vec<(PathBuf, usize)>,
    jump_index: usize,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Workspace {
    pub fn new(
        root: PathBuf,
        files: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let lsp_enabled = cx.global::<crate::Settings>().lsp;
        let lsp = cx.new(|_| LspStore::new(root.clone(), lsp_enabled));
        let file_tree = cx.new(|cx| FileTree::new(root.clone(), cx));
        let git = cx.new(|cx| GitView::new(root.clone(), cx));

        // Closing the window must not silently drop unsaved work.
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            weak.update(cx, |this, cx| this.can_quit(cx))
                .unwrap_or(true)
        });

        let subscriptions = vec![
            cx.subscribe_in(&file_tree, window, Self::on_file_tree_event),
            cx.subscribe_in(&git, window, Self::on_git_event),
            cx.subscribe(&lsp, |_, _, event: &LspEvent, cx| {
                if matches!(event, LspEvent::Status) {
                    cx.notify();
                }
            }),
            cx.observe_window_appearance(window, |_, window, cx| {
                gpui::component::Theme::sync_system_appearance(Some(window), cx);
            }),
            // Pick up files changed behind our back when coming to the front.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.sync_with_disk(cx);
                }
            }),
        ];
        let mut this = Self {
            root,
            focus_handle: cx.focus_handle(),
            lsp,
            tabs: Vec::new(),
            active: 0,
            file_tree,
            show_tree: true,
            git,
            show_git: false,
            picker: None,
            _picker_subscription: None,
            message: None,
            jumps: Vec::new(),
            jump_index: 0,
            _subscriptions: subscriptions,
        };
        for file in files {
            this.open_path(&file, window, cx);
        }
        if this.tabs.is_empty() {
            this.focus_handle.focus(window, cx);
        } else {
            this.activate(0, window, cx);
        }
        // A layout file that was not used should not go unnoticed, nor
        // what the init file had to say.
        let problem = cx.global::<Layouts>().problems.first().cloned();
        this.message = problem
            .or_else(|| crate::script::take_message(cx))
            .map(SharedString::from);
        this
    }

    pub(crate) fn active_editor(&self) -> Option<&Entity<Editor>> {
        self.tabs.get(self.active)
    }

    fn any_dirty(&self, cx: &App) -> bool {
        self.tabs.iter().any(|tab| tab.read(cx).is_dirty())
    }

    /// False (with an explanation in the status bar) while there is unsaved
    /// work.
    fn can_quit(&mut self, cx: &mut Context<Self>) -> bool {
        if self.any_dirty(cx) {
            self.set_message("Unsaved changes: :wa to save, :qa! to discard", cx);
            false
        } else {
            true
        }
    }

    /// Files may have changed underneath us (another program, a git
    /// command): reload clean buffers and the tree.
    fn sync_with_disk(&mut self, cx: &mut Context<Self>) {
        for tab in self.tabs.clone() {
            tab.update(cx, |editor, cx| editor.reload_if_changed(cx));
        }
        self.file_tree.update(cx, |tree, cx| tree.refresh(cx));
    }

    fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_editor() {
            Some(editor) => editor.read(cx).focus_handle(cx).focus(window, cx),
            None => self.focus_handle.focus(window, cx),
        }
        cx.notify();
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn set_message(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.message = Some(message.into());
        cx.notify();
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    // ---- tabs ----------------------------------------------------------

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            self.active = 0;
            window.set_window_title("med");
            self.focus_handle.focus(window, cx);
            cx.notify();
            return;
        }
        self.active = index.min(self.tabs.len() - 1);
        let editor = self.tabs[self.active].clone();
        editor.read(cx).focus_handle(cx).focus(window, cx);
        let title = match &editor.read(cx).path {
            Some(path) => format!("{} — med", self.relative(path)),
            None => "med".to_string(),
        };
        window.set_window_title(&title);
        cx.notify();
    }

    /// Open `path` in a tab (or switch to the tab that already has it).
    pub fn open_path(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Editor>> {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        let path = path.canonicalize().unwrap_or(path);
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.read(cx).path.as_deref() == Some(path.as_path()))
        {
            self.activate(index, window, cx);
            return self.tabs.get(index).cloned();
        }
        let text = match std::fs::read(&path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => {
                    self.set_message(
                        format!("{}: not a UTF-8 text file", self.relative(&path)),
                        cx,
                    );
                    return None;
                }
            },
            // A path that does not exist yet is a new file.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                self.set_message(format!("{}: {error}", self.relative(&path)), cx);
                return None;
            }
        };
        let lsp = self.lsp.clone();
        let editor = cx.new(|cx| Editor::new(Some(path), &text, Some(lsp), cx));
        self.add_tab(editor.clone(), window, cx);
        Some(editor)
    }

    fn add_tab(&mut self, editor: Entity<Editor>, window: &mut Window, cx: &mut Context<Self>) {
        self._subscriptions
            .push(cx.subscribe_in(&editor, window, Self::on_editor_event));
        let index = (self.active + 1).min(self.tabs.len());
        self.tabs.insert(index, editor);
        self.activate(index, window, cx);
    }

    fn close_tab(
        &mut self,
        index: usize,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(editor) = self.tabs.get(index).cloned() else {
            return true;
        };
        if !force && editor.read(cx).is_dirty() {
            self.set_message("No write since last change (add ! to override)", cx);
            return false;
        }
        editor.update(cx, |editor, cx| editor.close(cx));
        self.tabs.remove(index);
        let next = if index < self.active || self.active >= self.tabs.len() {
            self.active.saturating_sub(1)
        } else {
            self.active
        };
        self.activate(next, window, cx);
        true
    }

    fn cycle_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.tabs.len() as isize;
        if count > 0 {
            let index = (self.active as isize + delta).rem_euclid(count) as usize;
            self.activate(index, window, cx);
        }
    }

    fn save_active(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(editor) = self.active_editor().cloned() else {
            return false;
        };
        match editor.update(cx, |editor, cx| editor.save(cx)) {
            Ok(()) => {
                let editor = editor.read(cx);
                let name = editor
                    .path
                    .as_deref()
                    .map(|p| self.relative(p))
                    .unwrap_or_default();
                let lines = editor.buf.line_count();
                self.set_message(format!("\"{name}\" {lines}L written"), cx);
                self.file_tree.update(cx, |tree, cx| tree.refresh(cx));
                true
            }
            Err(error) => {
                self.set_message(format!("{error:#}"), cx);
                false
            }
        }
    }

    // ---- jump list -----------------------------------------------------

    fn push_jump(&mut self, path: PathBuf, offset: usize) {
        self.jumps.truncate(self.jump_index);
        if self.jumps.last() != Some(&(path.clone(), offset)) {
            self.jumps.push((path, offset));
        }
        if self.jumps.len() > 100 {
            self.jumps.remove(0);
        }
        self.jump_index = self.jumps.len();
    }

    fn jump(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let here = self.active_editor().and_then(|editor| {
            let editor = editor.read(cx);
            Some((editor.path.clone()?, editor.vim.cursor))
        });
        let target = if back {
            if self.jump_index == 0 {
                return;
            }
            // Leaving the newest position: remember it so ctrl-i can return.
            if self.jump_index == self.jumps.len()
                && let Some(here) = here
            {
                self.jumps.push(here);
            }
            self.jump_index -= 1;
            self.jumps[self.jump_index].clone()
        } else {
            if self.jump_index + 1 >= self.jumps.len() {
                return;
            }
            self.jump_index += 1;
            self.jumps[self.jump_index].clone()
        };
        if let Some(editor) = self.open_path(&target.0, window, cx) {
            editor.update(cx, |editor, cx| editor.goto_offset(target.1, cx));
        }
    }

    // ---- editor events -------------------------------------------------

    fn on_editor_event(
        &mut self,
        editor: &Entity<Editor>,
        event: &EditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            // Cleared before the key runs, not after: the events a key
            // causes arrive ahead of its `Changed`, and one of them may be
            // the message to show.
            EditorEvent::KeyPress => self.message = None,
            EditorEvent::Changed => cx.notify(),
            EditorEvent::Message(message) => self.set_message(message.clone(), cx),
            EditorEvent::Ex(command) => self.run_ex(command, window, cx),
            EditorEvent::Command { name, args } => self.run_command(name, args, window, cx),
            EditorEvent::OpenLocation { path, position } => {
                self.open_location(path, *position, window, cx)
            }
            EditorEvent::References(locations) => {
                let items = locations
                    .iter()
                    .filter_map(|location| {
                        let path = uri_to_path(&location.uri)?;
                        let start = location.range.start;
                        // Show the referencing line when the file is at hand.
                        let line = std::fs::read_to_string(&path)
                            .ok()
                            .and_then(|text| {
                                text.lines().nth(start.line as usize).map(str::to_string)
                            })
                            .unwrap_or_default();
                        Some(Item {
                            detail: format!("{}:{}", self.relative(&path), start.line + 1).into(),
                            label: line.trim().to_string().into(),
                            position: Some((start.line as usize, start.character as usize)),
                            path,
                        })
                    })
                    .collect();
                self.open_picker(PickerMode::List(items), window, cx);
            }
            EditorEvent::PushJump(offset) => {
                if let Some(path) = editor.read(cx).path.clone() {
                    self.push_jump(path, *offset);
                }
            }
        }
    }

    // ---- panels --------------------------------------------------------

    fn on_file_tree_event(
        &mut self,
        _: &Entity<FileTree>,
        event: &FileTreeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            FileTreeEvent::Open(path) => {
                self.open_path(path, window, cx);
            }
            FileTreeEvent::Dismiss => self.focus_editor(window, cx),
            FileTreeEvent::Command(name) => self.run_command(name, &Args::default(), window, cx),
        }
    }

    fn on_git_event(
        &mut self,
        _: &Entity<GitView>,
        event: &GitEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            GitEvent::Close => {
                self.show_git = false;
                self.focus_editor(window, cx);
            }
            GitEvent::OpenFile(path) => {
                self.show_git = false;
                if self.open_path(path, window, cx).is_none() {
                    self.focus_editor(window, cx);
                }
            }
            GitEvent::Changed => self.sync_with_disk(cx),
            GitEvent::Command(name) => self.run_command(name, &Args::default(), window, cx),
        }
    }

    fn toggle_git(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_git = !self.show_git;
        if self.show_git {
            self.close_picker(cx);
            self.git.update(cx, |git, cx| git.refresh(cx));
            self.git.read(cx).focus_handle(cx).focus(window, cx);
            cx.notify();
        } else {
            self.focus_editor(window, cx);
        }
    }

    fn toggle_file_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_tree = !self.show_tree;
        if !self.show_tree && self.file_tree.read(cx).focus_handle(cx).is_focused(window) {
            self.focus_editor(window, cx);
        }
        cx.notify();
    }

    /// Jump into the file tree — or back out of it if already there.
    fn focus_file_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.file_tree.read(cx).focus_handle(cx);
        if self.show_tree && handle.is_focused(window) {
            self.focus_editor(window, cx);
            return;
        }
        self.show_tree = true;
        self.show_git = false;
        if let Some(path) = self.active_editor().and_then(|e| e.read(cx).path.clone()) {
            self.file_tree.update(cx, |tree, cx| tree.reveal(&path, cx));
        }
        handle.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn open_picker(
        &mut self,
        mode: PickerMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.root.clone();
        let picker = cx.new(|cx| Picker::new(mode, root, window, cx));
        self._picker_subscription = Some(cx.subscribe_in(&picker, window, Self::on_picker_event));
        self.picker = Some(picker);
        self.show_git = false;
        cx.notify();
    }

    fn close_picker(&mut self, cx: &mut Context<Self>) {
        self.picker = None;
        self._picker_subscription = None;
        cx.notify();
    }

    fn on_picker_event(
        &mut self,
        _: &Entity<Picker>,
        event: &PickerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_picker(cx);
        match event {
            PickerEvent::Dismiss => self.focus_editor(window, cx),
            PickerEvent::Confirm(item) => {
                // Remember where we came from so ctrl-o gets back.
                let here = self.active_editor().and_then(|editor| {
                    let editor = editor.read(cx);
                    Some((editor.path.clone()?, editor.vim.cursor))
                });
                if let Some((path, offset)) = here {
                    self.push_jump(path, offset);
                }
                match self.open_path(&item.path, window, cx) {
                    Some(editor) => {
                        if let Some((line, col)) = item.position {
                            editor.update(cx, |editor, cx| editor.goto_line_col(line, col, cx));
                        }
                    }
                    None => self.focus_editor(window, cx),
                }
            }
        }
    }

    pub fn open_location(
        &mut self,
        path: &Path,
        position: lsp::Position,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.open_path(path, window, cx) {
            editor.update(cx, |editor, cx| editor.goto_position(position, cx));
        }
    }

    // ---- commands ------------------------------------------------------

    /// Run a command by name on the view it belongs to.
    pub fn run_command(
        &mut self,
        name: &str,
        args: &Args,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match cx.global::<Commands>().handler(name) {
            Some(Handler::Workspace(run)) => run(self, args, window, cx),
            Some(Handler::Editor(run)) => {
                if let Some(editor) = self.active_editor().cloned() {
                    editor.update(cx, |editor, cx| run(editor, args, cx));
                }
            }
            Some(Handler::FileTree(run)) => {
                self.file_tree.update(cx, |tree, cx| run(tree, args, cx));
            }
            Some(Handler::Git(run)) => {
                let git = self.git.clone();
                git.update(cx, |git, cx| run(git, args, window, cx));
            }
            Some(Handler::Script(function)) => {
                crate::script::run_command(function, args.clone(), window, cx);
            }
            None => self.set_message(format!("Unknown command: {name}"), cx),
        }
    }

    /// An ex command line the vim engine left to us: `name[!] [argument]`.
    fn run_ex(&mut self, command: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (name, arg) = match command.split_once(char::is_whitespace) {
            Some((name, arg)) => (name, arg.trim()),
            None => (command, ""),
        };
        let (name, bang) = match name.strip_suffix('!') {
            Some(name) => (name, true),
            None => (name, false),
        };
        match cx.global::<Commands>().resolve_ex(name) {
            Some(name) => {
                let args = Args {
                    count: None,
                    arg: arg.to_string(),
                    bang,
                };
                self.run_command(&name, &args, window, cx);
            }
            None => self.set_message(format!("Not an editor command: {command}"), cx),
        }
    }

    // ---- rendering -----------------------------------------------------

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, tab_bar, active_bg, muted, fg) = (
            theme.border,
            theme.tab_bar,
            theme.background,
            theme.muted_foreground,
            theme.foreground,
        );
        div()
            .id("tabs")
            .flex()
            .flex_shrink_0()
            .h(px(32.))
            .bg(tab_bar)
            .border_b_1()
            .border_color(border)
            .overflow_x_scroll()
            .children(self.tabs.iter().enumerate().map(|(index, tab)| {
                let editor = tab.read(cx);
                let active = index == self.active;
                div()
                    .id(("tab", index))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h_full()
                    .border_r_1()
                    .border_color(border)
                    .text_color(if active { fg } else { muted })
                    .when(active, |el| el.bg(active_bg))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate(index, window, cx);
                    }))
                    .child(editor.title())
                    .child(
                        div()
                            .id(("tab-close", index))
                            .w(px(14.))
                            .text_color(muted)
                            .hover(|el| el.text_color(fg))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(index, false, window, cx);
                            }))
                            .child(if editor.is_dirty() { "●" } else { "×" }),
                    )
            }))
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, border) = (theme.foreground, theme.muted_foreground, theme.border);
        let bar = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_3()
            .h(px(26.))
            .px_2()
            .bg(theme.status_bar)
            .border_t_1()
            .border_color(border)
            .text_size(px(12.))
            .font_family(theme.mono_font_family.clone());
        let Some(editor) = self.active_editor().map(|e| e.read(cx)) else {
            return bar.child(
                div()
                    .text_color(muted)
                    .child(self.message.clone().unwrap_or_else(|| "med".into())),
            );
        };

        let mode = editor.vim.mode;
        let mode_color = match mode {
            Mode::Normal => theme.info,
            Mode::Insert => theme.success,
            Mode::Visual | Mode::VisualLine => theme.warning,
            Mode::Command => theme.primary,
        };
        let name = match &editor.path {
            Some(path) => self.relative(path),
            None => "[No Name]".to_string(),
        };
        let (line, col) = editor.cursor_position();
        let (errors, warnings) = self.lsp.read(cx).diagnostic_counts();
        let server = editor
            .path
            .as_deref()
            .and_then(|path| self.lsp.read(cx).status(path));

        // The command line takes over the left side while it is open.
        let left = match editor.vim.cmdline() {
            Some((kind, text)) => div()
                .flex_1()
                .min_w_0()
                .text_color(fg)
                .child(format!("{kind}{text}▏")),
            None => {
                let hint = self
                    .message
                    .clone()
                    .or_else(|| {
                        editor.diagnostic_at_cursor().map(|d| {
                            d.message
                                .lines()
                                .next()
                                .unwrap_or_default()
                                .to_string()
                                .into()
                        })
                    })
                    .unwrap_or_default();
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .gap_3()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(format!(
                        "{name}{}",
                        if editor.is_dirty() { " [+]" } else { "" }
                    ))
                    .child(div().text_color(muted).child(hint))
            }
        };

        bar.child(
            div()
                .px_2()
                .rounded_sm()
                .bg(mode_color)
                .text_color(theme.background)
                .child(mode.label()),
        )
        .child(left)
        .child(
            div()
                .text_color(theme.warning)
                .child(editor.vim.pending_display()),
        )
        .when(errors + warnings > 0, |el| {
            el.child(
                div()
                    .flex()
                    .gap_2()
                    .child(div().text_color(theme.danger).child(format!("E:{errors}")))
                    .child(
                        div()
                            .text_color(theme.warning)
                            .child(format!("W:{warnings}")),
                    ),
            )
        })
        .children(server.map(|s| div().text_color(muted).child(s)))
        .children(
            self.git
                .read(cx)
                .branch_name()
                .filter(|name| !name.is_empty())
                .map(|name| div().text_color(theme.success).child(format!(" {name}"))),
        )
        .child(div().text_color(muted).child(editor.language_name()))
        .child(format!("{line}:{col}"))
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted, overlay) = (theme.border, theme.muted_foreground, theme.overlay);
        let (background, foreground) = (theme.background, theme.foreground);

        let body = if self.show_git {
            div().flex_1().min_h_0().child(self.git.clone())
        } else {
            let center = match self.active_editor() {
                Some(editor) => div().flex_1().min_h_0().child(editor.clone()),
                None => div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(muted)
                    .child("No file open  ·  ⌘P find file  ·  ⌘⇧F search  ·  ⌘⇧G git"),
            };
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .when(self.show_tree, |el| {
                    el.child(
                        div()
                            .w(px(260.))
                            .flex_shrink_0()
                            .h_full()
                            .border_r_1()
                            .border_color(border)
                            .child(self.file_tree.clone()),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .flex_col()
                        .when(!self.tabs.is_empty(), |el| el.child(self.render_tabs(cx)))
                        .child(center),
                )
        };

        let picker = self.picker.clone().map(|picker| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .justify_center()
                .pt(px(72.))
                .bg(overlay.opacity(0.25))
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.close_picker(cx);
                        this.focus_editor(window, cx);
                    }),
                )
                .child(
                    div()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(picker),
                )
        });

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(background)
            .text_color(foreground)
            .text_size(px(13.))
            .track_focus(&self.focus_handle)
            .key_context("Workspace")
            .on_action(cx.listener(|this, action: &RunCommand, window, cx| {
                this.run_command(&action.0, &Args::default(), window, cx);
            }))
            .child(body)
            .child(self.render_status(cx))
            .children(picker)
    }
}

pub(crate) fn register(commands: &mut Commands) {
    use Handler::Workspace as W;
    commands.register(
        "workspace.save",
        "Write the file; given a name, write it under that name",
        W(|ws, args, _, cx| {
            if !args.arg.is_empty() {
                let path = ws.root.join(&args.arg);
                if let Some(editor) = ws.active_editor().cloned() {
                    editor.update(cx, |editor, cx| editor.set_path(path, cx));
                }
            }
            ws.save_active(cx);
        }),
    );
    commands.register(
        "workspace.save_all",
        "Write every changed file",
        W(|ws, _, _, cx| {
            let mut saved = 0;
            for tab in ws.tabs.clone() {
                if tab.read(cx).is_dirty() && tab.update(cx, |e, cx| e.save(cx)).is_ok() {
                    saved += 1;
                }
            }
            ws.set_message(format!("{saved} file(s) written"), cx);
        }),
    );
    commands.register(
        "workspace.edit",
        "Open the named file in a tab",
        W(|ws, args, window, cx| {
            if args.arg.is_empty() {
                ws.set_message("No file name", cx);
            } else {
                ws.open_path(Path::new(&args.arg), window, cx);
            }
        }),
    );
    commands.register(
        "workspace.close_tab",
        "Close the tab; with ! even if it has unsaved changes",
        W(|ws, args, window, cx| {
            ws.close_tab(ws.active, args.bang, window, cx);
        }),
    );
    commands.register(
        "workspace.quit",
        "Close the tab, and the application with the last one",
        W(|ws, args, window, cx| {
            ws.close_tab(ws.active, args.bang, window, cx);
            if ws.tabs.is_empty() {
                cx.quit();
            }
        }),
    );
    commands.register(
        "workspace.save_and_quit",
        "Write the file, then close its tab",
        W(|ws, _, window, cx| {
            if ws.save_active(cx) {
                ws.close_tab(ws.active, true, window, cx);
                if ws.tabs.is_empty() {
                    cx.quit();
                }
            }
        }),
    );
    commands.register(
        "workspace.quit_all",
        "Quit; with ! even if there are unsaved changes",
        W(|ws, args, _, cx| {
            if args.bang || ws.can_quit(cx) {
                cx.quit();
            }
        }),
    );
    commands.register(
        "workspace.next_tab",
        "Go to the next tab",
        W(|ws, _, window, cx| ws.cycle_tab(1, window, cx)),
    );
    commands.register(
        "workspace.prev_tab",
        "Go to the previous tab",
        W(|ws, _, window, cx| ws.cycle_tab(-1, window, cx)),
    );
    commands.register(
        "workspace.jump_back",
        "Go back in the jump list",
        W(|ws, _, window, cx| ws.jump(true, window, cx)),
    );
    commands.register(
        "workspace.jump_forward",
        "Go forward in the jump list",
        W(|ws, _, window, cx| ws.jump(false, window, cx)),
    );
    commands.register(
        "workspace.find_file",
        "Find a file by name",
        W(|ws, _, window, cx| ws.open_picker(PickerMode::Files, window, cx)),
    );
    commands.register(
        "workspace.search",
        "Search the project",
        W(|ws, _, window, cx| ws.open_picker(PickerMode::Grep, window, cx)),
    );
    commands.register(
        "workspace.toggle_file_tree",
        "Show or hide the file tree",
        W(|ws, _, window, cx| ws.toggle_file_tree(window, cx)),
    );
    commands.register(
        "workspace.focus_file_tree",
        "Move to the file tree, or back from it",
        W(|ws, _, window, cx| ws.focus_file_tree(window, cx)),
    );
    commands.register(
        "workspace.eval",
        "Evaluate Lisp, as in :eval (bind 'normal \"Q\" \"workspace.quit\")",
        W(|_, args, window, cx| crate::script::eval_from(args.arg.clone(), window, cx)),
    );
    commands.register(
        "workspace.reload_init",
        "Run the init file again",
        W(|ws, _, _, cx| {
            let ran = crate::script::load_init(cx);
            let message = crate::script::take_message(cx).unwrap_or_else(|| match ran {
                Some(path) => format!("Ran {}", path.display()),
                None => "There is no init.lisp".to_string(),
            });
            ws.set_message(message, cx);
        }),
    );
    commands.register(
        "workspace.reload_panels",
        "Read the panel layout files again",
        W(|ws, _, _, cx| {
            crate::panel::load_user_layouts(cx);
            let message = match cx.global::<Layouts>().problems.first() {
                Some(problem) => problem.clone(),
                None => "Panel layouts reloaded".to_string(),
            };
            ws.set_message(message, cx);
        }),
    );
    commands.register(
        "workspace.toggle_git",
        "Open or close the git screen",
        W(|ws, _, window, cx| ws.toggle_git(window, cx)),
    );
}
