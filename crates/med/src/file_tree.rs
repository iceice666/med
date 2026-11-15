//! The project file tree in the sidebar, driven with vim-style keys.

use gpui::component::ActiveTheme as _;
use gpui::prelude::*;
use gpui::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const ROW_HEIGHT: f32 = 24.;

pub enum FileTreeEvent {
    Open(PathBuf),
    /// The user is done here; focus should go back to the editor.
    Dismiss,
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
    pending_g: bool,
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
            pending_g: false,
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

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.platform || keystroke.modifiers.alt {
            return;
        }
        let ctrl = keystroke.modifiers.control;
        // Named keys ("enter", "escape") by name; printable ones by the
        // character they produce, so shift and layout are accounted for.
        let key = match &keystroke.key_char {
            Some(text) if !ctrl && keystroke.key.chars().count() == 1 => text.as_str(),
            _ => keystroke.key.as_str(),
        };
        let was_pending_g = std::mem::take(&mut self.pending_g);
        let last = self.entries.len().saturating_sub(1);
        match key {
            "d" if ctrl => self.select((self.selected + 10).min(last), cx),
            "u" if ctrl => self.select(self.selected.saturating_sub(10), cx),
            "j" | "down" => self.select((self.selected + 1).min(last), cx),
            "k" | "up" => self.select(self.selected.saturating_sub(1), cx),
            "h" | "left" => self.collapse_or_parent(cx),
            "l" | "right" => {
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
            "enter" | "o" => self.activate(self.selected, cx),
            "g" if was_pending_g => self.select(0, cx),
            "g" => self.pending_g = true,
            "G" => self.select(last, cx),
            "R" => self.refresh(cx),
            "escape" | "q" => cx.emit(FileTreeEvent::Dismiss),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for FileTree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let focused = self.focus_handle.is_focused(window);
        let (fg, muted, active, hover) = (
            theme.sidebar_foreground,
            theme.muted_foreground,
            theme.list_active,
            theme.list_hover,
        );
        let (modified, added) = (theme.warning, theme.success);
        let root_name = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string());

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.sidebar)
            .text_color(fg)
            .track_focus(&self.focus_handle)
            .key_context("FileTree")
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(32.))
                    .px_3()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.sidebar_border)
                    .text_color(if focused { fg } else { muted })
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(root_name),
            )
            .child(
                uniform_list(
                    "file-tree",
                    self.entries.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                let entry = &this.entries[index];
                                let selected = index == this.selected;
                                let status = this.git_status.get(&entry.path).copied();
                                let color = match status {
                                    Some('?') | Some('A') => added,
                                    Some(_) => modified,
                                    None => fg,
                                };
                                let marker = if !entry.is_dir {
                                    "  "
                                } else if this.expanded.contains(&entry.path) {
                                    "▾ "
                                } else {
                                    "▸ "
                                };
                                div()
                                    .id(("tree-row", index))
                                    .h(px(ROW_HEIGHT))
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .pl(px(10. + entry.depth as f32 * 14.))
                                    .pr_2()
                                    .whitespace_nowrap()
                                    .text_color(color)
                                    .when(selected && focused, |el| el.bg(active))
                                    .when(selected && !focused, |el| el.bg(hover))
                                    .hover(|el| el.bg(hover))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.focus_handle.focus(window, cx);
                                        this.select(index, cx);
                                        this.activate(index, cx);
                                    }))
                                    .child(div().text_color(muted).child(marker))
                                    .child(entry.name.clone())
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .flex_1()
                .min_h_0(),
            )
    }
}
