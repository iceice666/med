//! Finding things in a project: its files, the ones a few typed letters
//! mean, and the lines that match a pattern. Programs reach these through
//! `walk-files`, `fuzzy` and `grep` (see `host.rs`).

use grep_matcher::Matcher as _;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// A line that matched.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub path: PathBuf,
    /// The path from the root that was searched.
    pub file: String,
    /// 0-based line and char column of the match.
    pub line: usize,
    pub column: usize,
    /// The line, without the space around it.
    pub text: String,
}

/// What is in `dir`, directories first, skipping what git ignores: each
/// path, and whether it is a directory.
pub fn list_dir(dir: &Path) -> Vec<(PathBuf, bool)> {
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

/// The `limit` of `files` that `query` fits best, best first: all of them
/// in order when there is no query.
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
pub fn grep(root: &Path, query: &str, limit: usize, cancel: &AtomicBool) -> Vec<Hit> {
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
                items.push(Hit {
                    path: path.to_path_buf(),
                    file: relative.clone(),
                    line: line_number.saturating_sub(1) as usize,
                    column,
                    text: line.trim().chars().take(200).collect(),
                });
                Ok(items.len() < limit)
            }),
        );
    }
    items
}

#[cfg(test)]
mod tests {
    use super::{fuzzy_files, grep, list_dir, walk_files};
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

        let names: Vec<(String, bool)> = list_dir(&dir)
            .into_iter()
            .map(|(path, is_dir)| {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                (name, is_dir)
            })
            .collect();
        // Directories first, and neither .git nor what it ignores.
        assert_eq!(
            names,
            [
                ("src".to_string(), true),
                (".gitignore".to_string(), false),
                ("README.md".to_string(), false)
            ]
        );

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
        hits.sort_by(|a, b| a.file.cmp(&b.file));
        // Smart case: lowercase matches both; ignored and .git files are skipped.
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].file, "src/main.rs");
        assert_eq!((hits[0].line, hits[0].column), (1, 8));
        assert_eq!(hits[0].text, "let Needle = 1;");
        assert_eq!(hits[1].file, "src/nested/lib.rs");
        // Column is in chars, not bytes.
        assert_eq!((hits[1].line, hits[1].column), (0, 6));
        assert_eq!(grep(&dir, "Needle", 100, &cancel).len(), 1);
        // An invalid regex falls back to a literal search.
        assert_eq!(grep(&dir, "main(", 100, &cancel).len(), 1);

        fs::remove_dir_all(&dir).unwrap();
    }
}
