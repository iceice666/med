//! A lazygit-style git screen: numbered panels on the left, a preview of
//! the selection on the right, single-key commands.

use crate::commands::{Args, Commands, Handler};
use crate::keymap::{KeySequence, Press, panel_key};
use gpui::component::ActiveTheme as _;
use gpui::component::input::{Escape, Input, InputEvent, InputState};
use gpui::prelude::*;
use gpui::*;
use med_git::{Branch, Commit, FileStatus, Head, Repo, Stash};
use med_vim::Key;
use std::path::PathBuf;

const ROW_HEIGHT: f32 = 22.;
const LOG_LIMIT: usize = 300;

pub enum GitEvent {
    Close,
    OpenFile(PathBuf),
    /// Something on disk may have changed (checkout, pull, discard…).
    Changed,
    /// A key bound to a command that is not the git screen's to run.
    Command(SharedString),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Status,
    Files,
    Branches,
    Commits,
    Stash,
}

impl Panel {
    const ALL: [Panel; 5] = [
        Panel::Status,
        Panel::Files,
        Panel::Branches,
        Panel::Commits,
        Panel::Stash,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    /// The panel's own key table, looked at before `git`.
    fn key_context(self) -> &'static str {
        match self {
            Panel::Status => "git.status",
            Panel::Files => "git.files",
            Panel::Branches => "git.branches",
            Panel::Commits => "git.commits",
            Panel::Stash => "git.stash",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Panel::Status => "Status",
            Panel::Files => "Files",
            Panel::Branches => "Branches",
            Panel::Commits => "Commits",
            Panel::Stash => "Stash",
        }
    }

    fn hints(self) -> &'static str {
        match self {
            Panel::Status => "p pull  P push  f fetch  R refresh",
            Panel::Files => "space stage  a all  c commit  A amend  d discard  s stash  enter open",
            Panel::Branches => "space checkout  n new  d delete",
            Panel::Commits => "J/K scroll diff",
            Panel::Stash => "space apply  g pop  d drop",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PromptKind {
    Commit,
    Amend,
    NewBranch,
}

struct Prompt {
    kind: PromptKind,
    input: Entity<InputState>,
    _subscription: Subscription,
}

type Command = Box<dyn FnOnce(&Repo) -> anyhow::Result<String> + Send>;

struct Confirm {
    question: String,
    label: &'static str,
    command: Command,
}

/// Everything read from the repository in one refresh.
#[derive(Default)]
struct Snapshot {
    head: Head,
    files: Vec<FileStatus>,
    branches: Vec<Branch>,
    commits: Vec<Commit>,
    stashes: Vec<Stash>,
}

pub struct GitView {
    repo: Option<Repo>,
    focus_handle: FocusHandle,
    panel: Panel,
    data: Snapshot,
    selected: [usize; 5],
    scrolls: [UniformListScrollHandle; 5],
    /// The preview on the right, one entry per line.
    preview: Vec<SharedString>,
    preview_title: SharedString,
    preview_scroll: UniformListScrollHandle,
    preview_top: usize,
    message: Option<(SharedString, bool)>,
    busy: Option<&'static str>,
    prompt: Option<Prompt>,
    confirm: Option<Confirm>,
    /// A key sequence in progress.
    keys: KeySequence,
    _refresh: Option<Task<()>>,
    _preview: Option<Task<()>>,
    _command: Option<Task<()>>,
}

impl EventEmitter<GitEvent> for GitView {}

impl Focusable for GitView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn load(repo: &Repo) -> Snapshot {
    Snapshot {
        head: repo.head().unwrap_or_default(),
        files: repo.status().unwrap_or_default(),
        branches: repo.branches().unwrap_or_default(),
        commits: repo.log(None, LOG_LIMIT).unwrap_or_default(),
        stashes: repo.stashes().unwrap_or_default(),
    }
}

impl GitView {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            repo: Repo::discover(&root),
            focus_handle: cx.focus_handle(),
            panel: Panel::Files,
            data: Snapshot::default(),
            selected: [0; 5],
            scrolls: std::array::from_fn(|_| UniformListScrollHandle::new()),
            preview: Vec::new(),
            preview_title: "".into(),
            preview_scroll: UniformListScrollHandle::new(),
            preview_top: 0,
            message: None,
            busy: None,
            prompt: None,
            confirm: None,
            keys: KeySequence::default(),
            _refresh: None,
            _preview: None,
            _command: None,
        };
        this.refresh(cx);
        this
    }

    pub fn branch_name(&self) -> Option<&str> {
        self.repo.as_ref().map(|_| self.data.head.name.as_str())
    }

    fn len(&self, panel: Panel) -> usize {
        match panel {
            Panel::Status => 1,
            Panel::Files => self.data.files.len(),
            Panel::Branches => self.data.branches.len(),
            Panel::Commits => self.data.commits.len(),
            Panel::Stash => self.data.stashes.len(),
        }
    }

    fn current(&self) -> usize {
        self.selected[self.panel.index()]
    }

    pub fn selected_file(&self) -> Option<&FileStatus> {
        self.data.files.get(self.selected[Panel::Files.index()])
    }

    fn selected_branch(&self) -> Option<&Branch> {
        let index = self.selected[Panel::Branches.index()];
        self.data.branches.get(index)
    }

    fn selected_stash(&self) -> Option<&Stash> {
        self.data.stashes.get(self.selected[Panel::Stash.index()])
    }

    /// Re-read everything from git.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let snapshot = cx.background_spawn(async move { load(&repo) });
        self._refresh = Some(cx.spawn(async move |this, cx| {
            let snapshot = snapshot.await;
            this.update(cx, |this, cx| {
                this.data = snapshot;
                for panel in Panel::ALL {
                    let max = this.len(panel).saturating_sub(1);
                    let slot = &mut this.selected[panel.index()];
                    *slot = (*slot).min(max);
                }
                this.load_preview(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn set_preview(&mut self, title: impl Into<SharedString>, text: &str, cx: &mut Context<Self>) {
        self.preview_title = title.into();
        self.preview = text
            .lines()
            .map(|line| SharedString::from(line.replace('\t', "    ")))
            .collect();
        self.preview_top = 0;
        self.preview_scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Show whatever the selection points at in the right-hand pane.
    fn load_preview(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let index = self.current();
        type Job = Box<dyn FnOnce() -> anyhow::Result<String> + Send>;
        let (title, job): (String, Job) = match self.panel {
            Panel::Status => (
                "Uncommitted changes".into(),
                Box::new(move || repo.diff_all()),
            ),
            Panel::Files => match self.data.files.get(index).cloned() {
                Some(file) => (
                    file.path.clone(),
                    Box::new(move || {
                        let mut out = String::new();
                        if file.has_unstaged() {
                            let diff = repo.diff_file(&file, false)?;
                            out.push_str("Unstaged changes\n\n");
                            out.push_str(&diff);
                        }
                        if file.has_staged() {
                            let diff = repo.diff_file(&file, true)?;
                            if !out.is_empty() {
                                out.push('\n');
                            }
                            out.push_str("Staged changes\n\n");
                            out.push_str(&diff);
                        }
                        Ok(out)
                    }),
                ),
                None => ("Files".into(), Box::new(|| Ok("Working tree clean".into()))),
            },
            Panel::Branches => match self.data.branches.get(index).cloned() {
                Some(branch) => (
                    branch.name.clone(),
                    Box::new(move || {
                        let commits = repo.log(Some(&branch.name), 100)?;
                        Ok(commits
                            .iter()
                            .map(|c| {
                                format!("{}  {}  ({}, {})", c.short, c.subject, c.author, c.date)
                            })
                            .collect::<Vec<_>>()
                            .join("\n"))
                    }),
                ),
                None => ("Branches".into(), Box::new(|| Ok(String::new()))),
            },
            Panel::Commits => match self.data.commits.get(index).cloned() {
                Some(commit) => (
                    format!("{} {}", commit.short, commit.subject),
                    Box::new(move || repo.show(&commit.hash)),
                ),
                None => ("Commits".into(), Box::new(|| Ok("No commits yet".into()))),
            },
            Panel::Stash => match self.data.stashes.get(index).cloned() {
                Some(stash) => (
                    stash.message.clone(),
                    Box::new(move || repo.stash_show(stash.index)),
                ),
                None => ("Stash".into(), Box::new(|| Ok("No stashed changes".into()))),
            },
        };
        let work = cx.background_spawn(async move { job() });
        self._preview = Some(cx.spawn(async move |this, cx| {
            let text = work.await.unwrap_or_else(|error| format!("{error:#}"));
            this.update(cx, |this, cx| this.set_preview(title, &text, cx))
                .ok();
        }));
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let len = self.len(self.panel);
        if len == 0 {
            return;
        }
        let index = index.min(len - 1);
        let slot = self.panel.index();
        if self.selected[slot] != index {
            self.selected[slot] = index;
            self.scrolls[slot].scroll_to_item(index, ScrollStrategy::Center);
            self.load_preview(cx);
        }
        cx.notify();
    }

    fn focus_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if self.panel != panel {
            self.panel = panel;
            self.load_preview(cx);
        }
        cx.notify();
    }

    fn scroll_preview(&mut self, delta: isize, cx: &mut Context<Self>) {
        let max = self.preview.len().saturating_sub(1) as isize;
        self.preview_top = (self.preview_top as isize + delta).clamp(0, max.max(0)) as usize;
        self.preview_scroll
            .scroll_to_item_strict(self.preview_top, ScrollStrategy::Top);
        cx.notify();
    }

    /// Run a git command off the main thread, then refresh.
    fn run(
        &mut self,
        label: &'static str,
        command: impl FnOnce(&Repo) -> anyhow::Result<String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        self.busy = Some(label);
        self.message = None;
        let work = cx.background_spawn(async move { command(&repo) });
        self._command = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| {
                this.busy = None;
                this.message = Some(match result {
                    Ok(output) => {
                        let line = output.lines().last().unwrap_or_default().trim().to_string();
                        let text = if line.is_empty() {
                            format!("{label}: done")
                        } else {
                            format!("{label}: {line}")
                        };
                        (text.into(), false)
                    }
                    Err(error) => (format!("{error:#}").into(), true),
                });
                this.refresh(cx);
                cx.emit(GitEvent::Changed);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn ask(
        &mut self,
        question: String,
        label: &'static str,
        command: impl FnOnce(&Repo) -> anyhow::Result<String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.confirm = Some(Confirm {
            question,
            label,
            command: Box::new(command),
        });
        cx.notify();
    }

    fn open_prompt(&mut self, kind: PromptKind, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = match kind {
            PromptKind::Commit => "Commit message",
            PromptKind::Amend => "Amended commit message",
            PromptKind::NewBranch => "New branch name",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        if kind == PromptKind::Amend
            && let Some(last) = self.data.commits.first()
        {
            let subject = last.subject.clone();
            input.update(cx, |input, cx| input.set_value(subject, window, cx));
        }
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit_prompt(window, cx);
                }
            });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.prompt = Some(Prompt {
            kind,
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn close_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn submit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        self.focus_handle.focus(window, cx);
        let text = prompt.input.read(cx).value().trim().to_string();
        if text.is_empty() {
            cx.notify();
            return;
        }
        match prompt.kind {
            PromptKind::Commit => self.run("commit", move |repo| repo.commit(&text, false), cx),
            PromptKind::Amend => self.run("amend", move |repo| repo.commit(&text, true), cx),
            PromptKind::NewBranch => self.run("branch", move |repo| repo.create_branch(&text), cx),
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.modifiers.platform || self.prompt.is_some() {
            return;
        }
        let Some(key) = panel_key(&event.keystroke) else {
            return;
        };
        cx.stop_propagation();

        if let Some(confirm) = self.confirm.take() {
            self.keys.clear();
            if matches!(key, Key::Char('y' | 'Y') | Key::Enter) {
                self.run(confirm.label, confirm.command, cx);
            }
            cx.notify();
            return;
        }

        let contexts = [self.panel.key_context(), "git"];
        let Press::Command(name) = self.keys.press(key, &contexts, cx) else {
            return;
        };
        // While git runs, the only way is out.
        if self.busy.is_some() && name != "git.close" {
            return;
        }
        match cx.global::<Commands>().handler(&name) {
            Some(Handler::Git(run)) => run(self, &Args::default(), window, cx),
            _ => cx.emit(GitEvent::Command(name)),
        }
    }

    fn cycle_panel(&mut self, delta: usize, cx: &mut Context<Self>) {
        let count = Panel::ALL.len();
        self.focus_panel(Panel::ALL[(self.panel.index() + delta) % count], cx);
    }

    /// Stage whatever of the file is unstaged; unstage a fully staged one.
    fn toggle_stage(&mut self, cx: &mut Context<Self>) {
        let Some(file) = self.selected_file().cloned() else {
            return;
        };
        if file.has_unstaged() {
            self.run(
                "stage",
                move |repo| repo.stage(&file.path).map(|_| String::new()),
                cx,
            );
        } else {
            self.run(
                "unstage",
                move |repo| repo.unstage(&file.path).map(|_| String::new()),
                cx,
            );
        }
    }

    fn toggle_stage_all(&mut self, cx: &mut Context<Self>) {
        if self.data.files.iter().any(FileStatus::has_unstaged) {
            self.run(
                "stage all",
                |repo| repo.stage_all().map(|_| String::new()),
                cx,
            );
        } else {
            self.run(
                "unstage all",
                |repo| repo.unstage_all().map(|_| String::new()),
                cx,
            );
        }
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.data.files.iter().any(FileStatus::has_staged) {
            self.open_prompt(PromptKind::Commit, window, cx);
        } else {
            self.message = Some(("Nothing staged to commit".into(), true));
            cx.notify();
        }
    }

    fn discard(&mut self, cx: &mut Context<Self>) {
        let Some(file) = self.selected_file().cloned() else {
            return;
        };
        let question = format!(
            "Discard all changes to {}? This cannot be undone.",
            file.path
        );
        self.ask(
            question,
            "discard",
            move |repo| repo.discard(&file).map(|_| String::new()),
            cx,
        );
    }

    fn open_selected_file(&mut self, cx: &mut Context<Self>) {
        if let (Some(file), Some(repo)) = (self.selected_file(), &self.repo) {
            cx.emit(GitEvent::OpenFile(repo.root.join(&file.path)));
        }
    }

    fn checkout(&mut self, cx: &mut Context<Self>) {
        if let Some(branch) = self.selected_branch().cloned()
            && !branch.current
        {
            self.run("checkout", move |repo| repo.checkout(&branch.name), cx);
        }
    }

    fn delete_branch(&mut self, cx: &mut Context<Self>) {
        if let Some(branch) = self.selected_branch().cloned()
            && !branch.current
        {
            let question = format!("Delete branch {}?", branch.name);
            self.ask(
                question,
                "delete branch",
                move |repo| repo.delete_branch(&branch.name, false),
                cx,
            );
        }
    }

    fn stash_drop(&mut self, cx: &mut Context<Self>) {
        let Some(stash) = self.selected_stash().cloned() else {
            return;
        };
        let question = format!("Drop stash \"{}\"? This cannot be undone.", stash.message);
        self.ask(
            question,
            "stash drop",
            move |repo| repo.stash_drop(stash.index),
            cx,
        );
    }

    // ---- rendering -----------------------------------------------------

    fn render_row(&self, panel: Panel, index: usize, cx: &App) -> Div {
        let theme = cx.theme();
        let (muted, green, red, yellow) = (
            theme.muted_foreground,
            theme.success,
            theme.danger,
            theme.warning,
        );
        let row = div()
            .h(px(ROW_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .whitespace_nowrap();
        match panel {
            Panel::Status => {
                let head = &self.data.head;
                let mut sync = String::new();
                if head.ahead > 0 {
                    sync.push_str(&format!("↑{} ", head.ahead));
                }
                if head.behind > 0 {
                    sync.push_str(&format!("↓{}", head.behind));
                }
                let root = self
                    .repo
                    .as_ref()
                    .and_then(|r| r.root.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                row.child(root)
                    .child(div().text_color(muted).child("→"))
                    .child(div().text_color(green).child(head.name.clone()))
                    .child(div().text_color(yellow).child(sync))
            }
            Panel::Files => {
                let file = &self.data.files[index];
                let index_color = if file.is_untracked() { red } else { green };
                row.child(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .child(div().text_color(index_color).child(file.index.to_string()))
                        .child(div().text_color(red).child(file.worktree.to_string())),
                )
                .child(file.path.clone())
            }
            Panel::Branches => {
                let branch = &self.data.branches[index];
                row.child(
                    div()
                        .w(px(10.))
                        .flex_shrink_0()
                        .text_color(green)
                        .child(if branch.current { "*" } else { "" }),
                )
                .child(
                    div()
                        .when(branch.current, |el| el.text_color(green))
                        .child(branch.name.clone()),
                )
                .child(div().text_color(yellow).child(branch.track.clone()))
                .child(div().text_color(muted).child(branch.date.clone()))
            }
            Panel::Commits => {
                let commit = &self.data.commits[index];
                row.child(
                    div()
                        .flex_shrink_0()
                        .text_color(yellow)
                        .child(commit.short.clone()),
                )
                .when(!commit.refs.is_empty(), |el| {
                    el.child(
                        div()
                            .flex_shrink_0()
                            .text_color(green)
                            .child(format!("({})", commit.refs)),
                    )
                })
                .child(commit.subject.clone())
            }
            Panel::Stash => {
                let stash = &self.data.stashes[index];
                row.child(
                    div()
                        .flex_shrink_0()
                        .text_color(muted)
                        .child(stash.date.clone()),
                )
                .child(stash.message.clone())
            }
        }
    }

    fn render_panel(
        &self,
        panel: Panel,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let focused = self.panel == panel && self.focus_handle.is_focused(window);
        let current = self.panel == panel;
        let (border, accent, muted, active, hover) = (
            theme.border,
            theme.success,
            theme.muted_foreground,
            theme.list_active,
            theme.list_hover,
        );
        let slot = panel.index();
        let len = self.len(panel);
        let position = if len == 0 || panel == Panel::Status {
            String::new()
        } else {
            format!("{} of {}", self.selected[slot] + 1, len)
        };
        let body = div()
            .id(("git-panel", slot))
            .flex()
            .flex_col()
            .min_h_0()
            .border_1()
            .border_color(if current { accent } else { border })
            .rounded_sm()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.focus_handle.focus(window, cx);
                this.focus_panel(panel, cx);
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .justify_between()
                    .px_2()
                    .text_color(if current { accent } else { muted })
                    .child(format!("[{}] {}", slot + 1, panel.title()))
                    .child(position),
            )
            .child(
                uniform_list(
                    ("git-list", slot),
                    len,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                let selected = index == this.selected[slot];
                                this.render_row(panel, index, cx)
                                    .when(selected && focused, |el| el.bg(active))
                                    .when(selected && !focused, |el| el.bg(hover))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scrolls[slot])
                .flex_1()
                .min_h_0(),
            );
        // Give the busier panels more room.
        match panel {
            Panel::Status => body.flex_shrink_0().h(px(ROW_HEIGHT * 2. + 4.)),
            Panel::Files => body.flex_grow(3.).flex_basis(relative(0.)),
            Panel::Branches => body.flex_grow(2.).flex_basis(relative(0.)),
            Panel::Commits => body.flex_grow(3.).flex_basis(relative(0.)),
            Panel::Stash => body.flex_grow(1.2).flex_basis(relative(0.)),
        }
    }
}

impl Render for GitView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, fg, muted, border) = (
            theme.background,
            theme.foreground,
            theme.muted_foreground,
            theme.border,
        );
        let (green, red, cyan, yellow, popover) = (
            theme.success,
            theme.danger,
            theme.info,
            theme.warning,
            theme.popover,
        );
        let mono = theme.mono_font_family.clone();

        let root = div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            .font_family(mono)
            .text_size(px(12.))
            .track_focus(&self.focus_handle)
            .key_context("GitView")
            .on_key_down(cx.listener(Self::on_key_down));

        if self.repo.is_none() {
            return root.items_center().justify_center().child(
                div()
                    .text_color(muted)
                    .child("Not a git repository  ·  q to close"),
            );
        }

        let footer = if let Some(confirm) = &self.confirm {
            div()
                .text_color(yellow)
                .child(format!("{}  (y/n)", confirm.question))
        } else if let Some(label) = self.busy {
            div().text_color(yellow).child(format!("{label}…"))
        } else if let Some((message, is_error)) = &self.message {
            div()
                .text_color(if *is_error { red } else { green })
                .child(message.clone())
        } else {
            div().text_color(muted).child(format!(
                "{}  │  1-5 panel  j/k move  J/K scroll  q close",
                self.panel.hints()
            ))
        };

        let prompt = self.prompt.as_ref().map(|prompt| {
            let title = match prompt.kind {
                PromptKind::Commit => "Commit message",
                PromptKind::Amend => "Amend last commit",
                PromptKind::NewBranch => "New branch",
            };
            div()
                .absolute()
                .top(px(120.))
                .left_0()
                .w_full()
                .flex()
                .justify_center()
                .child(
                    div()
                        .occlude()
                        .w(px(560.))
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .bg(popover)
                        .border_1()
                        .border_color(green)
                        .rounded_md()
                        .shadow_lg()
                        .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                            this.close_prompt(window, cx);
                            cx.stop_propagation();
                        }))
                        .child(div().text_color(green).child(title))
                        .child(Input::new(&prompt.input))
                        .child(
                            div()
                                .text_color(muted)
                                .child("enter confirm  ·  esc cancel"),
                        ),
                )
        });

        let preview = uniform_list(
            "git-preview",
            self.preview.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _, _| {
                range
                    .map(|index| {
                        let line = this.preview[index].clone();
                        let color = if line.starts_with("+++") || line.starts_with("---") {
                            fg
                        } else if line.starts_with('+') {
                            green
                        } else if line.starts_with('-') {
                            red
                        } else if line.starts_with("@@") {
                            cyan
                        } else if line.starts_with("commit ")
                            || line.starts_with("Unstaged changes")
                            || line.starts_with("Staged changes")
                        {
                            yellow
                        } else if line.starts_with("diff --git") || line.starts_with("index ") {
                            muted
                        } else {
                            fg
                        };
                        div()
                            .h(px(ROW_HEIGHT - 4.))
                            .px_2()
                            .whitespace_nowrap()
                            .text_color(color)
                            .child(line)
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.preview_scroll)
        .flex_1()
        .min_h_0();

        root.child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .gap_1()
                .p_1()
                .child(
                    div()
                        .w(relative(0.36))
                        .flex_shrink_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(
                            Panel::ALL
                                .into_iter()
                                .map(|panel| {
                                    self.render_panel(panel, window, cx).into_any_element()
                                })
                                .collect::<Vec<_>>(),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .border_1()
                        .border_color(border)
                        .rounded_sm()
                        .overflow_hidden()
                        .child(
                            div()
                                .flex_shrink_0()
                                .px_2()
                                .text_color(muted)
                                .whitespace_nowrap()
                                .child(self.preview_title.clone()),
                        )
                        .child(preview),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .h(px(24.))
                .px_2()
                .flex()
                .items_center()
                .border_t_1()
                .border_color(border)
                .whitespace_nowrap()
                .overflow_hidden()
                .child(footer),
        )
        .children(prompt)
    }
}

pub(crate) fn register(commands: &mut Commands) {
    use Handler::Git as G;
    commands.register(
        "git.close",
        "Leave the git screen",
        G(|_, _, _, cx| cx.emit(GitEvent::Close)),
    );
    commands.register(
        "git.focus_status",
        "Go to the status panel",
        G(|git, _, _, cx| git.focus_panel(Panel::Status, cx)),
    );
    commands.register(
        "git.focus_files",
        "Go to the files panel",
        G(|git, _, _, cx| git.focus_panel(Panel::Files, cx)),
    );
    commands.register(
        "git.focus_branches",
        "Go to the branches panel",
        G(|git, _, _, cx| git.focus_panel(Panel::Branches, cx)),
    );
    commands.register(
        "git.focus_commits",
        "Go to the commits panel",
        G(|git, _, _, cx| git.focus_panel(Panel::Commits, cx)),
    );
    commands.register(
        "git.focus_stash",
        "Go to the stash panel",
        G(|git, _, _, cx| git.focus_panel(Panel::Stash, cx)),
    );
    commands.register(
        "git.next_panel",
        "Go to the next panel",
        G(|git, _, _, cx| git.cycle_panel(1, cx)),
    );
    commands.register(
        "git.prev_panel",
        "Go to the previous panel",
        G(|git, _, _, cx| git.cycle_panel(Panel::ALL.len() - 1, cx)),
    );
    commands.register(
        "git.down",
        "Select the next row",
        G(|git, _, _, cx| git.select(git.current() + 1, cx)),
    );
    commands.register(
        "git.up",
        "Select the previous row",
        G(|git, _, _, cx| git.select(git.current().saturating_sub(1), cx)),
    );
    commands.register(
        "git.first",
        "Select the first row",
        G(|git, _, _, cx| git.select(0, cx)),
    );
    commands.register(
        "git.last",
        "Select the last row",
        G(|git, _, _, cx| git.select(usize::MAX, cx)),
    );
    commands.register(
        "git.preview_down",
        "Scroll the preview down",
        G(|git, _, _, cx| git.scroll_preview(10, cx)),
    );
    commands.register(
        "git.preview_up",
        "Scroll the preview up",
        G(|git, _, _, cx| git.scroll_preview(-10, cx)),
    );
    commands.register(
        "git.preview_page_down",
        "Scroll the preview down by more",
        G(|git, _, _, cx| git.scroll_preview(15, cx)),
    );
    commands.register(
        "git.preview_page_up",
        "Scroll the preview up by more",
        G(|git, _, _, cx| git.scroll_preview(-15, cx)),
    );
    commands.register(
        "git.refresh",
        "Re-read the repository",
        G(|git, _, _, cx| git.refresh(cx)),
    );
    commands.register(
        "git.pull",
        "Pull",
        G(|git, _, _, cx| git.run("pull", |repo| repo.pull(), cx)),
    );
    commands.register(
        "git.push",
        "Push",
        G(|git, _, _, cx| git.run("push", |repo| repo.push(), cx)),
    );
    commands.register(
        "git.fetch",
        "Fetch",
        G(|git, _, _, cx| git.run("fetch", |repo| repo.fetch(), cx)),
    );
    commands.register(
        "git.toggle_stage",
        "Stage the selected file, or unstage it if fully staged",
        G(|git, _, _, cx| git.toggle_stage(cx)),
    );
    commands.register(
        "git.toggle_stage_all",
        "Stage everything, or unstage everything if nothing is left",
        G(|git, _, _, cx| git.toggle_stage_all(cx)),
    );
    commands.register(
        "git.commit",
        "Commit what is staged",
        G(|git, _, window, cx| git.commit(window, cx)),
    );
    commands.register(
        "git.amend",
        "Amend the last commit",
        G(|git, _, window, cx| git.open_prompt(PromptKind::Amend, window, cx)),
    );
    commands.register(
        "git.discard",
        "Discard the selected file's changes, after asking",
        G(|git, _, _, cx| git.discard(cx)),
    );
    commands.register(
        "git.stash",
        "Stash all changes",
        G(|git, _, _, cx| git.run("stash", |repo| repo.stash_push(None), cx)),
    );
    commands.register(
        "git.open_file",
        "Open the selected file in an editor",
        G(|git, _, _, cx| git.open_selected_file(cx)),
    );
    commands.register(
        "git.checkout",
        "Check out the selected branch",
        G(|git, _, _, cx| git.checkout(cx)),
    );
    commands.register(
        "git.new_branch",
        "Create a branch",
        G(|git, _, window, cx| git.open_prompt(PromptKind::NewBranch, window, cx)),
    );
    commands.register(
        "git.delete_branch",
        "Delete the selected branch, after asking",
        G(|git, _, _, cx| git.delete_branch(cx)),
    );
    commands.register(
        "git.stash_apply",
        "Apply the selected stash",
        G(|git, _, _, cx| {
            if let Some(index) = git.selected_stash().map(|stash| stash.index) {
                git.run("stash apply", move |repo| repo.stash_apply(index), cx);
            }
        }),
    );
    commands.register(
        "git.stash_pop",
        "Apply the selected stash and drop it",
        G(|git, _, _, cx| {
            if let Some(index) = git.selected_stash().map(|stash| stash.index) {
                git.run("stash pop", move |repo| repo.stash_pop(index), cx);
            }
        }),
    );
    commands.register(
        "git.stash_drop",
        "Drop the selected stash, after asking",
        G(|git, _, _, cx| git.stash_drop(cx)),
    );
}
