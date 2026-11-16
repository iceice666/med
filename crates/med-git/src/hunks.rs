//! The lines of a file that are not as they were at HEAD, for the marks
//! beside them: what was added, changed or taken out.

use similar::{Algorithm, DiffTag};
use std::ops::Range;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunkKind {
    Added,
    Modified,
    Deleted,
}

/// Lines that differ from the text a file had, numbered as in the text it
/// has now. Deleted lines are none of those: `lines` is empty and starts
/// at the line that the deleted ones were before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub kind: HunkKind,
    pub lines: Range<usize>,
}

/// The text `file` has at HEAD in the repository it is in. `None` where
/// there is none to compare with: not in a repository, not committed, no
/// commit yet, or not text.
pub fn committed_text(file: &Path) -> Option<String> {
    let dir = file.parent()?;
    let name = file.file_name()?.to_str()?;
    // `./` is from the directory git runs in: no need to know where the
    // repository's top is, which may be the same place by another path.
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["--no-pager", "show", &format!("HEAD:./{name}")])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.contains(&0) {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// How `text` differs from `base`, line by line, the first lines first.
/// Line endings are not compared, nor whether the last line has one. Past
/// `deadline` the diff is cruder, but is still a diff.
pub fn line_hunks(base: &str, text: &str, deadline: Option<Instant>) -> Vec<Hunk> {
    let old: Vec<&str> = base.lines().collect();
    let new: Vec<&str> = text.lines().collect();
    let ops = similar::capture_diff_slices_deadline(Algorithm::Myers, &old, &new, deadline);
    let mut hunks: Vec<Hunk> = Vec::new();
    for op in ops {
        let (tag, _, lines) = op.as_tag_tuple();
        let kind = match tag {
            DiffTag::Equal => continue,
            DiffTag::Insert => HunkKind::Added,
            DiffTag::Delete => HunkKind::Deleted,
            DiffTag::Replace => HunkKind::Modified,
        };
        // Lines taken out where others go in: those were changed.
        if let Some(last) = hunks.last_mut()
            && last.kind == HunkKind::Deleted
            && kind == HunkKind::Added
            && last.lines.start == lines.start
        {
            *last = Hunk {
                kind: HunkKind::Modified,
                lines,
            };
            continue;
        }
        hunks.push(Hunk { kind, lines });
    }
    hunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use HunkKind::*;

    fn hunks(base: &str, text: &str) -> Vec<(HunkKind, Range<usize>)> {
        line_hunks(base, text, None)
            .into_iter()
            .map(|h| (h.kind, h.lines))
            .collect()
    }

    #[test]
    fn the_same_text_has_none() {
        assert_eq!(hunks("a\nb\n", "a\nb\n"), []);
        assert_eq!(hunks("", ""), []);
    }

    #[test]
    fn added_lines() {
        assert_eq!(hunks("a\nb\n", "x\na\nb\n"), [(Added, 0..1)]);
        assert_eq!(hunks("a\nb\n", "a\nx\ny\nb\n"), [(Added, 1..3)]);
        assert_eq!(hunks("a\nb\n", "a\nb\nx\n"), [(Added, 2..3)]);
        assert_eq!(hunks("", "a\nb\n"), [(Added, 0..2)]);
    }

    #[test]
    fn changed_lines() {
        assert_eq!(hunks("a\nb\nc\n", "a\nB\nc\n"), [(Modified, 1..2)]);
        assert_eq!(
            hunks("a\nb\nc\n", "A\nb\nC\n"),
            [(Modified, 0..1), (Modified, 2..3)]
        );
        // More lines in place of fewer, or fewer of more: changed, all.
        assert_eq!(hunks("a\nb\nc\n", "a\nx\ny\nc\n"), [(Modified, 1..3)]);
        assert_eq!(hunks("a\nb\nc\nd\n", "a\nx\nd\n"), [(Modified, 1..2)]);
    }

    #[test]
    fn deleted_lines() {
        assert_eq!(hunks("a\nb\nc\n", "b\nc\n"), [(Deleted, 0..0)]);
        assert_eq!(hunks("a\nb\nc\n", "a\nc\n"), [(Deleted, 1..1)]);
        assert_eq!(hunks("a\nb\nc\n", "a\nb\n"), [(Deleted, 2..2)]);
        assert_eq!(hunks("a\nb\n", ""), [(Deleted, 0..0)]);
    }

    #[test]
    fn line_endings_are_not_changes() {
        assert_eq!(hunks("a\r\nb\r\n", "a\nb\n"), []);
        assert_eq!(hunks("a\nb", "a\nb\n"), []);
        assert_eq!(hunks("a\nb\n", "a\nb"), []);
    }
}
