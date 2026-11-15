//! The window's root view: tabs of editors, the status bar, the panels
//! programs define, and the ex commands that act on them.

use crate::commands::{Args, Commands, Handler, RunCommand};
use crate::editor::{Editor, EditorEvent};
use crate::lsp::{LspEvent, LspStore, uri_to_path};
use crate::script;
use crate::user_panel::{At, PanelView, UserPanel, UserPanels};
use gpui::component::ActiveTheme as _;
use gpui::prelude::*;
use gpui::*;
use lsp_types as lsp;
use med_vim::Mode;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Workspace {
    root: PathBuf,
    focus_handle: FocusHandle,
    lsp: Entity<LspStore>,
    pub(crate) tabs: Vec<Entity<Editor>>,
    pub(crate) active: usize,
    /// The views of the panels, made as each is first drawn or asked for.
    user_panels: HashMap<SharedString, Entity<PanelView>>,
    /// The branch the project is on, for the status bar.
    branch: Option<SharedString>,
    _branch_task: Option<Task<()>>,
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

        // Closing the window must not silently drop unsaved work.
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            weak.update(cx, |this, cx| this.can_quit(cx))
                .unwrap_or(true)
        });

        let subscriptions = vec![
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
                    this.sync_with_disk(window, cx);
                }
            }),
        ];
        let mut this = Self {
            root,
            focus_handle: cx.focus_handle(),
            lsp,
            tabs: Vec::new(),
            active: 0,
            user_panels: HashMap::new(),
            branch: None,
            _branch_task: None,
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
        // What the init file had to say, or what was wrong with it.
        this.message = script::take_message(cx).map(SharedString::from);
        this.read_branch(cx);
        // The panels have a project to show.
        script::run_hook("opened", Vec::new(), window, cx);
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
    /// command): reload clean buffers, and tell the panels.
    fn sync_with_disk(&mut self, window: &Window, cx: &mut Context<Self>) {
        for tab in self.tabs.clone() {
            tab.update(cx, |editor, cx| editor.reload_if_changed(cx));
        }
        self.files_changed(window, cx);
    }

    /// What is on disk is no longer what it was.
    fn files_changed(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.read_branch(cx);
        script::run_hook("files-changed", Vec::new(), window, cx);
    }

    /// Ask git which branch the project is on.
    fn read_branch(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let branch = cx.background_spawn(async move {
            let head = med_git::Repo::discover(&root)?.head().ok()?;
            Some(head.name).filter(|name| !name.is_empty())
        });
        self._branch_task = Some(cx.spawn(async move |this, cx| {
            let branch = branch.await.map(SharedString::from);
            this.update(cx, |this, cx| {
                this.branch = branch;
                cx.notify();
            })
            .ok();
        }));
    }

    pub(crate) fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn save_active(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
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
                self.files_changed(window, cx);
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
            EditorEvent::Pending(pending) => {
                let pending = pending.as_ref().map(script::editor_pending);
                script::keys_pending(pending.unwrap_or(med_lisp::Value::Nil), window, cx);
            }
            EditorEvent::Changed => cx.notify(),
            EditorEvent::Message(message) => self.set_message(message.clone(), cx),
            EditorEvent::Ex(command) => self.run_ex(command, window, cx),
            EditorEvent::Command { name, args } => self.run_command(name, args, window, cx),
            EditorEvent::OpenLocation { path, position } => {
                self.open_location(path, *position, window, cx)
            }
            EditorEvent::References(locations) => {
                let places: Vec<(PathBuf, lsp::Position)> = locations
                    .iter()
                    .filter_map(|location| {
                        Some((uri_to_path(&location.uri)?, location.range.start))
                    })
                    .collect();
                // The line of each is shown, when the file is at hand.
                let list = med_lisp::Value::list(places.iter().map(|(path, start)| {
                    let (line, column) = (start.line as usize, start.character as usize);
                    crate::host::location(&self.root, path.clone(), line, column)
                }));
                // A program lists them to choose from. With none to, the
                // first is as good as any.
                if !script::run_hook("locations", vec![list], window, cx)
                    && let Some((path, start)) = places.first()
                {
                    self.open_location(path, *start, window, cx);
                    let count = places.len();
                    self.set_message(format!("The first of {count} references"), cx);
                }
            }
            EditorEvent::PushJump(offset) => {
                if let Some(path) = editor.read(cx).path.clone() {
                    self.push_jump(path, *offset);
                }
            }
        }
    }

    // ---- panels --------------------------------------------------------

    /// The view of the panel `name`, made if this is the first it is
    /// asked for.
    pub(crate) fn panel_view(
        &mut self,
        name: &SharedString,
        cx: &mut Context<Self>,
    ) -> Entity<PanelView> {
        let workspace = cx.weak_entity();
        self.user_panels
            .entry(name.clone())
            .or_insert_with(|| cx.new(|cx| PanelView::new(name.clone(), workspace, cx)))
            .clone()
    }

    /// Panels were put up or away. One that went away with the keyboard
    /// gives it back to where it was, if that is a panel still up, or else
    /// to the editor.
    pub(crate) fn panels_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panels = cx.global::<UserPanels>();
        let shown: Vec<Entity<PanelView>> = self
            .user_panels
            .iter()
            .filter(|(name, _)| panels.is_shown(name))
            .map(|(_, view)| view.clone())
            .collect();
        let gone = self
            .user_panels
            .iter()
            .find(|(name, view)| !panels.is_shown(name) && view.read(cx).has_focus(window, cx));
        if let Some((_, view)) = gone {
            let back = view.clone().update(cx, |view, _| view.take_return());
            match back.filter(|back| shown.iter().any(|view| view.read(cx).owns(back, cx))) {
                Some(back) => back.focus(window, cx),
                None => self.focus_editor(window, cx),
            }
        }
        cx.notify();
    }

    /// Open `path` and go to a line and column of it, counted from 0.
    /// Where the editor was is remembered, so that ctrl-o gets back.
    pub(crate) fn open_at(
        &mut self,
        path: &Path,
        position: Option<(usize, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let here = self.active_editor().and_then(|editor| {
            let editor = editor.read(cx);
            Some((editor.path.clone()?, editor.vim.cursor))
        });
        if let Some((path, offset)) = here {
            self.push_jump(path, offset);
        }
        match self.open_path(path, window, cx) {
            Some(editor) => {
                if let Some((line, col)) = position {
                    editor.update(cx, |editor, cx| editor.goto_line_col(line, col, cx));
                }
                true
            }
            None => {
                self.focus_editor(window, cx);
                false
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
            Some(Handler::Script(function)) => {
                script::run_command(function, args.clone(), window, cx);
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
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock => theme.warning,
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
        .children(editor.vim.recording().map(|register| {
            div()
                .text_color(theme.danger)
                .child(format!("recording @{register}"))
        }))
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
            self.branch
                .as_ref()
                .map(|name| div().text_color(theme.success).child(format!(" {name}"))),
        )
        .child(div().text_color(muted).child(editor.language_name()))
        .child(format!("{line}:{col}"))
    }
}

impl Workspace {
    /// The panels that are up, each with its view.
    fn shown_user_panels(&mut self, cx: &mut Context<Self>) -> Vec<(UserPanel, Entity<PanelView>)> {
        let panels = cx.global::<UserPanels>();
        let shown: Vec<UserPanel> = panels.shown().cloned().collect();
        let defined: Vec<SharedString> = panels.names().map(SharedString::new).collect();
        self.user_panels.retain(|name, _| defined.contains(name));
        shown
            .into_iter()
            .map(|panel| {
                let view = self.panel_view(&panel.name, cx);
                (panel, view)
            })
            .collect()
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let user_panels = self.shown_user_panels(cx);
        let theme = cx.theme();
        let (popover, popover_foreground) = (theme.popover, theme.popover_foreground);
        let line = theme.border;
        // A sidebar, with a line between it and the editor.
        let sidebars = |at: At| {
            user_panels
                .iter()
                .filter(move |(panel, _)| panel.at == at)
                .map(move |(panel, view)| {
                    div()
                        .w(px(panel.width))
                        .flex_shrink_0()
                        .h_full()
                        .border_color(line)
                        .when(at == At::Left, |el| el.border_r_1())
                        .when(at == At::Right, |el| el.border_l_1())
                        // A click in it leaves the keyboard where it was.
                        .on_mouse_down(MouseButton::Left, |_, window, _| {
                            window.prevent_default();
                        })
                        .child(view.clone())
                })
        };
        let (border, muted, overlay) = (theme.border, theme.muted_foreground, theme.overlay);
        let (background, foreground) = (theme.background, theme.foreground);

        // A panel that takes the editor's place: the last one put up.
        let full = user_panels
            .iter()
            .rev()
            .find(|(panel, _)| panel.at == At::Full);
        let body = if let Some((_, view)) = full {
            div().flex_1().min_h_0().child(view.clone())
        } else {
            let center = match self.active_editor() {
                Some(editor) => div().flex_1().min_h_0().child(editor.clone()),
                None => div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(muted)
                    .child("No file open  ·  :e name opens one"),
            };
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .children(sidebars(At::Left))
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
                .children(sidebars(At::Right))
        };

        // At the bottom: across the window, as tall as what is in it, and
        // there whatever takes the editor's place.
        let bottoms = user_panels
            .iter()
            .filter(|(panel, _)| panel.at == At::Bottom)
            .map(|(_, view)| {
                div()
                    .w_full()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(line)
                    // A click in it leaves the keyboard where it was.
                    .on_mouse_down(MouseButton::Left, |_, window, _| {
                        window.prevent_default();
                    })
                    .child(view.clone())
            })
            .collect::<Vec<_>>();

        // A popup: over everything, as tall as what is in it, and put away
        // by a click beside it.
        let popups = user_panels
            .iter()
            .filter(|(panel, _)| panel.at == At::Popup)
            .map(|(panel, view)| {
                let name = panel.name.clone();
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .justify_center()
                    .items_start()
                    .pt(px(72.))
                    .bg(overlay.opacity(0.25))
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.global_mut::<UserPanels>().hide(&name);
                            // The keyboard stays where it was, unless the
                            // popup had it.
                            window.prevent_default();
                            this.panels_changed(window, cx);
                        }),
                    )
                    .child(
                        div()
                            .w(px(panel.width))
                            .bg(popover)
                            .text_color(popover_foreground)
                            .border_1()
                            .border_color(border)
                            .rounded_md()
                            .overflow_hidden()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(view.clone()),
                    )
            })
            .collect::<Vec<_>>();

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
            // Escape puts a popup away before any view gets to see it.
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && cx.global_mut::<UserPanels>().hide_popups() {
                    cx.stop_propagation();
                    this.panels_changed(window, cx);
                }
            }))
            .child(body)
            .children(bottoms)
            .child(self.render_status(cx))
            .children(popups)
    }
}

pub(crate) fn register(commands: &mut Commands) {
    use Handler::Workspace as W;
    commands.register(
        "workspace.save",
        "Write the file; given a name, write it under that name",
        W(|ws, args, window, cx| {
            if !args.arg.is_empty() {
                let path = ws.root.join(&args.arg);
                if let Some(editor) = ws.active_editor().cloned() {
                    editor.update(cx, |editor, cx| editor.set_path(path, cx));
                }
            }
            ws.save_active(window, cx);
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
            if ws.save_active(window, cx) {
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
        "workspace.sync",
        "Read again the files that changed on disk",
        W(|ws, _, window, cx| ws.sync_with_disk(window, cx)),
    );
    commands.register(
        "workspace.focus_editor",
        "Go back to the file being edited",
        W(|ws, _, window, cx| ws.focus_editor(window, cx)),
    );
    commands.register(
        "workspace.eval",
        "Evaluate Lisp, as in :eval (bind 'normal \"Q\" \"workspace.quit\")",
        W(|_, args, window, cx| script::eval_from(args.arg.clone(), window, cx)),
    );
    commands.register(
        "workspace.reload_init",
        "Run the init file again",
        W(|ws, _, window, cx| {
            let ran = script::reload_init(cx);
            let message = script::take_message(cx).unwrap_or_else(|| match ran {
                Some(path) => format!("Ran {}", path.display()),
                None => "There is no init.lisp".to_string(),
            });
            ws.set_message(message, cx);
            // The panels it defined anew have a project to show.
            script::run_hook("opened", Vec::new(), window, cx);
        }),
    );
}
