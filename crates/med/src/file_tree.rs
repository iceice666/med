//! The project file tree in the sidebar, driven with vim-style keys.
//!
//! This is what the tree holds and does. How it looks is the `file_tree`
//! layout (`panels/file_tree.json`), which draws from the values handed
//! out in the [`Host`] implementation below.

use crate::commands::{Args, Commands, Handler};
use crate::keymap::{KeySequence, Press, panel_key};
use crate::panel::{self, Host, Layouts, Vars};
use gpui::prelude::*;
use gpui::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub enum FileTreeEvent {
    Open(PathBuf),
    /// The user is done here; focus should go back to the editor.
    Dismiss,
    /// A key bound to a command that is not the tree's to run.
    Command(SharedString),
}

struct Entry {
    path: PathBuf,
    name: String,
    depth: usize,
    is_dir: bool,
}

pub struct FileTree {
    root: PathBuf,
    focus_handle: FocusHandle,
    expanded: HashSet<PathBuf>,
    /// The visible rows, flattened in display order.
    entries: Vec<Entry>,
    selected: usize,
    scroll: UniformListScrollHandle,
    /// Porcelain status letter per changed path, plus every parent directory.
    git_status: HashMap<PathBuf, char>,
    /// A key sequence in progress (`g` of `gg`).
    keys: KeySequence,
    _status_task: Option<Task<()>>,
}

impl EventEmitter<FileTreeEvent> for FileTree {}

impl Focusable for FileTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Children of `dir`, directories first, skipping what git ignores.
fn read_dir(dir: &Path) -> Vec<(PathBuf, bool)> {
    let mut children: Vec<(PathBuf, bool)> = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git" && entry.file_name() != ".DS_Store")
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.depth() == 1)
        .map(|entry| {
            let is_dir = entry.path().is_dir();
            (entry.into_path(), is_dir)
        })
        .collect();
    children.sort_by_cached_key(|(path, is_dir)| {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        (!is_dir, name.to_lowercase())
    });
    children
}

impl FileTree {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            root,
            focus_handle: cx.focus_handle(),
            expanded: HashSet::new(),
            entries: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            git_status: HashMap::new(),
            keys: KeySequence::default(),
            _status_task: None,
        };
        this.refresh(cx);
        this
    }

    /// Re-read the directories on screen and the git status.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let selected = self.entries.get(self.selected).map(|e| e.path.clone());
        self.entries.clear();
        let root = self.root.clone();
        self.push_children(&root, 0);
        if let Some(index) = selected.and_then(|p| self.entries.iter().position(|e| e.path == p)) {
            self.selected = index;
        }
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));

        let root = self.root.clone();
        let status = cx.background_spawn(async move {
            let repo = med_git::Repo::discover(&root)?;
            let files = repo.status().ok()?;
            let mut map = HashMap::new();
            for file in files {
                let letter = if file.is_untracked() {
                    '?'
                } else if file.worktree != ' ' {
                    file.worktree
                } else {
                    file.index
                };
                let path = repo.root.join(&file.path);
                // Directories show that something inside them changed.
                for ancestor in path.ancestors().skip(1) {
                    if !ancestor.starts_with(&repo.root) {
                        break;
                    }
                    map.entry(ancestor.to_path_buf()).or_insert('M');
                }
                map.insert(path, letter);
            }
            Some(map)
        });
        self._status_task = Some(cx.spawn(async move |this, cx| {
            let status = status.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.git_status = status;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn push_children(&mut self, dir: &Path, depth: usize) {
        for (path, is_dir) in read_dir(dir) {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let expanded = is_dir && self.expanded.contains(&path);
            self.entries.push(Entry {
                path: path.clone(),
                name,
                depth,
                is_dir,
            });
            if expanded {
                self.push_children(&path, depth + 1);
            }
        }
    }

    /// Expand down to `path` and select it.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return;
        };
        let mut dir = self.root.clone();
        let mut changed = false;
        if let Some(parent) = relative.parent() {
            for part in parent.components() {
                dir.push(part);
                changed |= self.expanded.insert(dir.clone());
            }
        }
        if changed {
            self.refresh(cx);
        }
        if let Some(index) = self.entries.iter().position(|e| e.path == path) {
            self.select(index, cx);
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.entries.is_empty() {
            return;
        }
        self.selected = index.min(self.entries.len() - 1);
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Center);
        cx.notify();
    }

    fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        let path = entry.path.clone();
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        self.refresh(cx);
    }

    /// Open the file, or fold/unfold the directory, at `index`.
    fn activate(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        if entry.is_dir {
            self.toggle(index, cx);
        } else {
            cx.emit(FileTreeEvent::Open(entry.path.clone()));
        }
    }

    fn collapse_or_parent(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(self.selected) else {
            return;
        };
        if entry.is_dir && self.expanded.contains(&entry.path) {
            self.toggle(self.selected, cx);
            return;
        }
        let depth = entry.depth;
        if depth == 0 {
            return;
        }
        if let Some(parent) = self.entries[..self.selected]
            .iter()
            .rposition(|e| e.depth < depth)
        {
            self.select(parent, cx);
        }
    }

    /// `l`: unfold a directory, open a file.
    fn expand_or_open(&mut self, cx: &mut Context<Self>) {
        let expandable = self
            .entries
            .get(self.selected)
            .is_some_and(|e| e.is_dir && !self.expanded.contains(&e.path));
        if expandable {
            self.toggle(self.selected, cx);
        } else if self.entries.get(self.selected).is_some_and(|e| !e.is_dir) {
            self.activate(self.selected, cx);
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let last = self.entries.len().saturating_sub(1);
        let index = self.selected.saturating_add_signed(delta).min(last);
        self.select(index, cx);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = panel_key(&event.keystroke) else {
            return;
        };
        match self.keys.press(key, &["tree"], cx) {
            // Not ours: let it travel on.
            Press::Unbound => return,
            Press::Pending => {}
            Press::Command(name) => self.run_command(name, cx),
        }
        cx.stop_propagation();
    }

    /// Run a command a key or a click named: here if it is the tree's,
    /// else by whoever is listening.
    fn run_command(&mut self, name: SharedString, cx: &mut Context<Self>) {
        match cx.global::<Commands>().handler(&name) {
            Some(Handler::FileTree(run)) => run(self, &Args::default(), cx),
            _ => cx.emit(FileTreeEvent::Command(name)),
        }
    }
}

pub(crate) fn register(commands: &mut Commands) {
    use Handler::FileTree as T;
    commands.register(
        "tree.down",
        "Select the next entry",
        T(|tree, _, cx| tree.move_selection(1, cx)),
    );
    commands.register(
        "tree.up",
        "Select the previous entry",
        T(|tree, _, cx| tree.move_selection(-1, cx)),
    );
    commands.register(
        "tree.page_down",
        "Move the selection down ten entries",
        T(|tree, _, cx| tree.move_selection(10, cx)),
    );
    commands.register(
        "tree.page_up",
        "Move the selection up ten entries",
        T(|tree, _, cx| tree.move_selection(-10, cx)),
    );
    commands.register(
        "tree.first",
        "Select the first entry",
        T(|tree, _, cx| tree.select(0, cx)),
    );
    commands.register(
        "tree.last",
        "Select the last entry",
        T(|tree, _, cx| tree.select(usize::MAX, cx)),
    );
    commands.register(
        "tree.collapse",
        "Fold the directory, or go to its parent",
        T(|tree, _, cx| tree.collapse_or_parent(cx)),
    );
    commands.register(
        "tree.expand",
        "Unfold the directory, or open the file",
        T(|tree, _, cx| tree.expand_or_open(cx)),
    );
    commands.register(
        "tree.open",
        "Open the file, or fold or unfold the directory",
        T(|tree, _, cx| tree.activate(tree.selected, cx)),
    );
    commands.register(
        "tree.refresh",
        "Re-read the directory",
        T(|tree, _, cx| tree.refresh(cx)),
    );
    commands.register(
        "tree.close",
        "Go back to the editor",
        T(|_, _, cx| cx.emit(FileTreeEvent::Dismiss)),
    );
}

/// What the `file_tree` layout can draw from. Everywhere: the text
/// `root_name` and the flag `focused`. In a row of the list `entries`: the
/// texts `name` and `status` (git's letter for the entry, if any), the
/// number `depth`, and the flags `selected`, `dir`, `expanded`, `added`
/// and `modified`.
impl Host for FileTree {
    fn vars(&self, window: &Window, _: &App) -> Vars {
        let root_name = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string());
        Vars::default()
            .text("root_name", root_name)
            .flag("focused", self.focus_handle.is_focused(window))
    }

    fn row_count(&self, _: &str) -> usize {
        self.entries.len()
    }

    fn row_vars(&self, _: &str, index: usize, _: &App) -> Vars {
        let Some(entry) = self.entries.get(index) else {
            return Vars::default();
        };
        let status = self.git_status.get(&entry.path).copied();
        let added = matches!(status, Some('?' | 'A'));
        Vars::default()
            .text("name", entry.name.clone())
            .text("status", status.map(String::from).unwrap_or_default())
            .number("depth", entry.depth as f32)
            .flag("selected", index == self.selected)
            .flag("dir", entry.is_dir)
            .flag("expanded", self.expanded.contains(&entry.path))
            .flag("added", added)
            .flag("modified", status.is_some() && !added)
    }

    fn scroll_handle(&self, _: &str) -> Option<&UniformListScrollHandle> {
        Some(&self.scroll)
    }

    /// A click on a row selects it before the row's command runs.
    fn click(
        &mut self,
        command: &str,
        row: Option<(&str, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        if let Some((_, index)) = row {
            self.select(index, cx);
        }
        self.run_command(SharedString::new(command), cx);
    }
}

impl Render for FileTree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = cx.global::<Layouts>().get("file_tree");
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("FileTree")
            .on_key_down(cx.listener(Self::on_key_down))
            .child(panel::render(&layout, self, window, cx))
    }
}
