//! The pop-up picker: fuzzy file finder, project-wide search, and plain
//! lists of locations (LSP references).

use gpui::component::ActiveTheme as _;
use gpui::component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui::prelude::*;
use gpui::*;
use grep_matcher::Matcher as _;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const ROW_HEIGHT: f32 = 26.;
const MAX_ROWS: usize = 16;
const MAX_RESULTS: usize = 500;

actions!(picker, [SelectNext, SelectPrev, Dismiss]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-n", SelectNext, Some("Picker")),
        KeyBinding::new("ctrl-p", SelectPrev, Some("Picker")),
        KeyBinding::new("ctrl-j", SelectNext, Some("Picker")),
        KeyBinding::new("ctrl-k", SelectPrev, Some("Picker")),
        KeyBinding::new("ctrl-c", Dismiss, Some("Picker")),
    ]);
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub path: PathBuf,
    /// 0-based line and char column to jump to.
    pub position: Option<(usize, usize)>,
    pub label: SharedString,
    pub detail: SharedString,
}

pub enum PickerMode {
    /// Fuzzy-find a file by path.
    Files,
    /// Search file contents (smart-case regex).
    Grep,
    /// Filter a fixed list.
    List(Vec<Item>),
}

pub enum PickerEvent {
    Confirm(Item),
    Dismiss,
}

pub struct Picker {
    mode: PickerMode,
    root: PathBuf,
    input: Entity<InputState>,
    /// Every file under the root, relative; filled in the background.
    files: Arc<Vec<String>>,
    items: Vec<Item>,
    selected: usize,
    scroll: UniformListScrollHandle,
    searching: bool,
    cancel: Arc<AtomicBool>,
    _task: Option<Task<()>>,
    _load: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PickerEvent> for Picker {}

/// All files under `root` that git would not ignore, as relative paths.
pub fn walk_files(root: &Path) -> Vec<String> {
    let mut files: Vec<String> = ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(root).ok()?;
            Some(relative.to_string_lossy().into_owned())
        })
        .collect();
    files.sort();
    files
}

pub fn fuzzy_files(files: &[String], query: &str, limit: usize) -> Vec<String> {
    if query.trim().is_empty() {
        return files.iter().take(limit).cloned().collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut matches = pattern.match_list(files.iter(), &mut matcher);
    // Best score first; among equals prefer the shorter path.
    matches.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.len().cmp(&b.0.len())));
    matches
        .into_iter()
        .take(limit)
        .map(|(path, _)| path.clone())
        .collect()
}

/// Search file contents under `root`. A query that is not a valid regex is
/// searched for literally.
pub fn grep(root: &Path, query: &str, limit: usize, cancel: &AtomicBool) -> Vec<Item> {
    let build = |literal: bool| {
        RegexMatcherBuilder::new()
            .case_smart(true)
            .fixed_strings(literal)
            .build(query)
    };
    let Ok(matcher) = build(false).or_else(|_| build(true)) else {
        return Vec::new();
    };
    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .line_number(true)
        .build();
    let mut items = Vec::new();
    let walk = ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    for entry in walk.filter_map(Result::ok) {
        if cancel.load(Ordering::Relaxed) || items.len() >= limit {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .display()
            .to_string();
        let _ = searcher.search_path(
            &matcher,
            path,
            UTF8(|line_number, line| {
                let column = matcher
                    .find(line.as_bytes())
                    .ok()
                    .flatten()
                    .map_or(0, |m| line[..m.start()].chars().count());
                let line_index = line_number.saturating_sub(1) as usize;
                let text: String = line.trim().chars().take(200).collect();
                items.push(Item {
                    path: path.to_path_buf(),
                    position: Some((line_index, column)),
                    label: text.into(),
                    detail: format!("{relative}:{line_number}").into(),
                });
                Ok(items.len() < limit)
            }),
        );
    }
    items
}

impl Picker {
    pub fn new(
        mode: PickerMode,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = match &mode {
            PickerMode::Files => "Find file…",
            PickerMode::Grep => "Search in project…",
            PickerMode::List(_) => "Filter…",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscriptions = vec![cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => this.update_results(cx),
                InputEvent::PressEnter { .. } => this.confirm(cx),
                _ => {}
            },
        )];
        input.update(cx, |input, cx| input.focus(window, cx));

        let mut this = Self {
            mode,
            root,
            input,
            files: Arc::default(),
            items: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            searching: false,
            cancel: Arc::default(),
            _task: None,
            _load: None,
            _subscriptions: subscriptions,
        };
        if matches!(this.mode, PickerMode::Files) {
            let root = this.root.clone();
            let walk = cx.background_spawn(async move { walk_files(&root) });
            this.searching = true;
            this._load = Some(cx.spawn(async move |this, cx| {
                let files = walk.await;
                this.update(cx, |this, cx| {
                    this.files = Arc::new(files);
                    this.searching = false;
                    this.update_results(cx);
                })
                .ok();
            }));
        }
        this.update_results(cx);
        this
    }

    fn file_item(&self, relative: &str) -> Item {
        let (dir, name) = match relative.rsplit_once('/') {
            Some((dir, name)) => (dir, name),
            None => ("", relative),
        };
        Item {
            path: self.root.join(relative),
            position: None,
            label: name.to_string().into(),
            detail: dir.to_string().into(),
        }
    }

    fn set_items(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        self.items = items;
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn update_results(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        self.cancel.store(true, Ordering::Relaxed);
        match &self.mode {
            PickerMode::Files => {
                let items = fuzzy_files(&self.files, &query, MAX_RESULTS)
                    .iter()
                    .map(|relative| self.file_item(relative))
                    .collect();
                self.set_items(items, cx);
            }
            PickerMode::List(all) => {
                let needle = query.to_lowercase();
                let items = all
                    .iter()
                    .filter(|item| {
                        needle.is_empty()
                            || item.label.to_lowercase().contains(&needle)
                            || item.detail.to_lowercase().contains(&needle)
                    })
                    .cloned()
                    .collect();
                self.set_items(items, cx);
            }
            PickerMode::Grep => {
                if query.chars().count() < 2 {
                    self.searching = false;
                    self._task = None;
                    self.set_items(Vec::new(), cx);
                    return;
                }
                let cancel = Arc::new(AtomicBool::new(false));
                self.cancel = cancel.clone();
                self.searching = true;
                let root = self.root.clone();
                let executor = cx.background_executor().clone();
                self._task = Some(cx.spawn(async move |this, cx| {
                    // Let a burst of typing settle before hitting the disk.
                    executor.timer(Duration::from_millis(120)).await;
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let flag = cancel.clone();
                    let items = executor
                        .spawn(async move { grep(&root, &query, MAX_RESULTS, &flag) })
                        .await;
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    this.update(cx, |this, cx| {
                        this.searching = false;
                        this.set_items(items, cx);
                    })
                    .ok();
                }));
                cx.notify();
            }
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.items.len() as isize;
        if count == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(count) as usize;
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Center);
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(item) = self.items.get(self.selected).cloned() {
            self.cancel.store(true, Ordering::Relaxed);
            cx.emit(PickerEvent::Confirm(item));
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        cx.emit(PickerEvent::Dismiss);
    }
}

impl Render for Picker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, active, hover, border) = (
            theme.popover_foreground,
            theme.muted_foreground,
            theme.list_active,
            theme.list_hover,
            theme.border,
        );
        let grep = matches!(self.mode, PickerMode::Grep | PickerMode::List(_));
        let rows = self.items.len().min(MAX_ROWS);
        let status = if self.searching {
            "searching…".to_string()
        } else {
            format!("{} results", self.items.len())
        };

        div()
            .key_context("Picker")
            .occlude()
            .w(px(if grep { 860. } else { 640. }))
            .flex()
            .flex_col()
            .bg(theme.popover)
            .text_color(fg)
            .border_1()
            .border_color(border)
            .rounded_lg()
            .shadow_lg()
            .overflow_hidden()
            // The input owns these keys; take them on the way down.
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                this.move_selection(1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                this.move_selection(-1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                this.dismiss(cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.dismiss(cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.input).appearance(false)),
                    )
                    .child(div().text_size(px(11.)).text_color(muted).child(status)),
            )
            .child(
                uniform_list(
                    "picker-items",
                    self.items.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                let item = &this.items[index];
                                let selected = index == this.selected;
                                let row = div()
                                    .id(("picker-row", index))
                                    .h(px(ROW_HEIGHT))
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .px_3()
                                    .whitespace_nowrap()
                                    .when(selected, |el| el.bg(active))
                                    .hover(|el| el.bg(hover))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected = index;
                                        this.confirm(cx);
                                    }));
                                let label = div().flex_shrink_0().child(item.label.clone());
                                let detail = div()
                                    .text_color(muted)
                                    .text_size(px(11.))
                                    .child(item.detail.clone());
                                if grep {
                                    // Location first, then the matching line.
                                    row.child(detail.flex_shrink_0().w(px(260.)).overflow_hidden())
                                        .child(label.flex_1().min_w_0().overflow_hidden())
                                } else {
                                    row.child(label)
                                        .child(detail.flex_1().min_w_0().overflow_hidden())
                                }
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .h(px(rows as f32 * ROW_HEIGHT)),
            )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would pull in GPUI's `test` attribute macro.
    use super::{fuzzy_files, grep, walk_files};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    fn temp_tree() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("med-picker-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/nested")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join(".gitignore"), "target\n").unwrap();
        fs::write(dir.join(".git/config"), "needle\n").unwrap();
        fs::write(dir.join("target/out.txt"), "needle\n").unwrap();
        fs::write(
            dir.join("src/main.rs"),
            "fn main() {\n    let Needle = 1;\n}\n",
        )
        .unwrap();
        fs::write(dir.join("src/nested/lib.rs"), "// 註解 needle here\n").unwrap();
        fs::write(dir.join("README.md"), "hello\n").unwrap();
        dir
    }

    #[test]
    fn walks_fuzzy_matches_and_greps() {
        let dir = temp_tree();

        let files = walk_files(&dir);
        assert_eq!(
            files,
            [
                ".gitignore",
                "README.md",
                "src/main.rs",
                "src/nested/lib.rs"
            ]
        );
        assert_eq!(fuzzy_files(&files, "main", 10), ["src/main.rs"]);
        assert_eq!(fuzzy_files(&files, "snl", 10), ["src/nested/lib.rs"]);
        assert_eq!(fuzzy_files(&files, "", 2).len(), 2);

        let cancel = AtomicBool::new(false);
        let mut hits = grep(&dir, "needle", 100, &cancel);
        hits.sort_by(|a, b| a.detail.cmp(&b.detail));
        // Smart case: lowercase matches both; ignored and .git files are skipped.
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].detail.as_ref(), "src/main.rs:2");
        assert_eq!(hits[0].position, Some((1, 8)));
        assert_eq!(hits[1].detail.as_ref(), "src/nested/lib.rs:1");
        // Column is in chars, not bytes.
        assert_eq!(hits[1].position, Some((0, 6)));
        assert_eq!(grep(&dir, "Needle", 100, &cancel).len(), 1);
        // An invalid regex falls back to a literal search.
        assert_eq!(grep(&dir, "main(", 100, &cancel).len(), 1);

        fs::remove_dir_all(&dir).unwrap();
    }
}
