//! A thin, synchronous wrapper around the `git` CLI. Every call blocks, so
//! the UI runs these on a background executor.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod hunks;

pub use hunks::{Hunk, HunkKind, committed_text, line_hunks};

const SEP: char = '\x1f';

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    pub root: PathBuf,
}

/// One entry of `git status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStatus {
    pub path: String,
    /// Set for renames and copies.
    pub orig_path: Option<String>,
    /// Status in the index (`X` of porcelain's `XY`).
    pub index: char,
    /// Status in the working tree (`Y`).
    pub worktree: char,
}

impl FileStatus {
    pub fn is_untracked(&self) -> bool {
        self.index == '?'
    }

    pub fn is_conflicted(&self) -> bool {
        self.index == 'U'
            || self.worktree == 'U'
            || (self.index == 'A' && self.worktree == 'A')
            || (self.index == 'D' && self.worktree == 'D')
    }

    pub fn has_staged(&self) -> bool {
        !matches!(self.index, ' ' | '?') && !self.is_conflicted()
    }

    pub fn has_unstaged(&self) -> bool {
        self.worktree != ' '
    }

    /// The two-letter code lazygit shows, e.g. `M ` or `??`.
    pub fn code(&self) -> String {
        format!("{}{}", self.index, self.worktree)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Head {
    /// Branch name, or a short hash when detached.
    pub name: String,
    pub detached: bool,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub current: bool,
    pub upstream: Option<String>,
    /// e.g. `[ahead 1, behind 2]`, empty when in sync.
    pub track: String,
    pub date: String,
    pub subject: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub author: String,
    pub date: String,
    pub refs: String,
    pub subject: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stash {
    pub index: usize,
    pub date: String,
    pub message: String,
}

impl Repo {
    /// The repository containing `path`, if any.
    pub fn discover(path: &Path) -> Option<Repo> {
        let dir = if path.is_dir() { path } else { path.parent()? };
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--show-toplevel"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let root = String::from_utf8(out.stdout).ok()?;
        Some(Repo {
            root: PathBuf::from(root.trim_end_matches(['\n', '\r'])),
        })
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.root)
            .args([
                "--no-pager",
                "-c",
                "color.ui=never",
                "-c",
                "core.quotepath=false",
            ])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null());
        cmd
    }

    /// Run git and return stdout. Exit codes listed in `ok_codes` count as
    /// success (`git diff --no-index` exits 1 when files differ).
    fn run(&self, args: &[&str], ok_codes: &[i32]) -> Result<String> {
        let out = self
            .command()
            .args(args)
            .output()
            .context("failed to run git")?;
        let code = out.status.code().unwrap_or(-1);
        if !ok_codes.contains(&code) {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let detail = if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            };
            bail!("git {}: {}", args.first().unwrap_or(&""), detail);
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        self.run(args, &[0])
    }

    /// Like [`git`], but also returns what git printed to stderr, which is
    /// where push/pull/fetch report progress.
    fn git_verbose(&self, args: &[&str]) -> Result<String> {
        let out = self
            .command()
            .args(args)
            .output()
            .context("failed to run git")?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        let text = text.trim().to_string();
        if !out.status.success() {
            bail!("git {}: {}", args.first().unwrap_or(&""), text);
        }
        Ok(text)
    }

    fn has_head(&self) -> bool {
        self.git(&["rev-parse", "--verify", "-q", "HEAD"]).is_ok()
    }

    // ---- reading -------------------------------------------------------

    pub fn status(&self) -> Result<Vec<FileStatus>> {
        let out = self.git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
        let mut fields = out.split('\0').filter(|f| !f.is_empty());
        let mut files = Vec::new();
        while let Some(entry) = fields.next() {
            let mut chars = entry.chars();
            let (Some(index), Some(worktree)) = (chars.next(), chars.next()) else {
                continue;
            };
            let path = entry.get(3..).unwrap_or_default().to_string();
            let orig_path = if matches!(index, 'R' | 'C') || matches!(worktree, 'R' | 'C') {
                fields.next().map(str::to_string)
            } else {
                None
            };
            files.push(FileStatus {
                path,
                orig_path,
                index,
                worktree,
            });
        }
        Ok(files)
    }

    pub fn head(&self) -> Result<Head> {
        let mut head = Head::default();
        match self.git(&["symbolic-ref", "--short", "-q", "HEAD"]) {
            Ok(name) => head.name = name.trim().to_string(),
            Err(_) => {
                head.detached = true;
                head.name = self
                    .git(&["rev-parse", "--short", "HEAD"])
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| "(no commits)".into());
            }
        }
        if let Ok(upstream) = self.git(&[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ]) {
            head.upstream = Some(upstream.trim().to_string());
            if let Ok(counts) =
                self.git(&["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
            {
                let mut parts = counts.split_whitespace();
                head.ahead = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                head.behind = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            }
        }
        Ok(head)
    }

    pub fn branches(&self) -> Result<Vec<Branch>> {
        let format = "%(HEAD)%1f%(refname:short)%1f%(upstream:short)%1f%(upstream:track)%1f%(committerdate:relative)%1f%(subject)";
        let out = self.git(&[
            "for-each-ref",
            "--sort=-committerdate",
            &format!("--format={format}"),
            "refs/heads",
        ])?;
        let mut branches: Vec<Branch> = out
            .lines()
            .filter_map(|line| {
                let mut f = line.split(SEP);
                Some(Branch {
                    current: f.next()? == "*",
                    name: f.next()?.to_string(),
                    upstream: f.next().filter(|s| !s.is_empty()).map(str::to_string),
                    track: f.next().unwrap_or_default().to_string(),
                    date: f.next().unwrap_or_default().to_string(),
                    subject: f.next().unwrap_or_default().to_string(),
                })
            })
            .collect();
        // Current branch first, like lazygit.
        branches.sort_by_key(|b| !b.current);
        Ok(branches)
    }

    /// Up to `limit` commits reachable from `rev` (HEAD if `None`).
    pub fn log(&self, rev: Option<&str>, limit: usize) -> Result<Vec<Commit>> {
        if !self.has_head() {
            return Ok(Vec::new());
        }
        let n = limit.to_string();
        let mut args = vec![
            "log",
            "-n",
            &n,
            "--format=%H%x1f%h%x1f%an%x1f%ar%x1f%D%x1f%s",
        ];
        if let Some(rev) = rev {
            args.push(rev);
        }
        args.push("--");
        let out = self.git(&args)?;
        Ok(out
            .lines()
            .filter_map(|line| {
                let mut f = line.split(SEP);
                Some(Commit {
                    hash: f.next()?.to_string(),
                    short: f.next()?.to_string(),
                    author: f.next()?.to_string(),
                    date: f.next()?.to_string(),
                    refs: f.next()?.to_string(),
                    subject: f.next().unwrap_or_default().to_string(),
                })
            })
            .collect())
    }

    pub fn stashes(&self) -> Result<Vec<Stash>> {
        let out = self.git(&["stash", "list", "--format=%gd%x1f%ar%x1f%gs"])?;
        Ok(out
            .lines()
            .enumerate()
            .filter_map(|(index, line)| {
                let mut f = line.split(SEP);
                f.next()?;
                Some(Stash {
                    index,
                    date: f.next()?.to_string(),
                    message: f.next().unwrap_or_default().to_string(),
                })
            })
            .collect())
    }

    /// Diff of one status entry: its staged or its unstaged half.
    pub fn diff_file(&self, file: &FileStatus, staged: bool) -> Result<String> {
        if file.is_untracked() {
            return self.run(
                &["diff", "--no-index", "--", "/dev/null", &file.path],
                &[0, 1],
            );
        }
        let mut args = vec!["diff"];
        if staged {
            args.push("--cached");
        }
        args.push("--");
        args.push(&file.path);
        if let Some(orig) = &file.orig_path {
            args.push(orig);
        }
        self.git(&args)
    }

    /// Everything not yet committed, staged and unstaged.
    pub fn diff_all(&self) -> Result<String> {
        if self.has_head() {
            self.git(&["diff", "HEAD"])
        } else {
            self.git(&["diff", "--cached"])
        }
    }

    /// A commit's message and patch.
    pub fn show(&self, rev: &str) -> Result<String> {
        self.git(&["show", "--stat", "--patch", "--format=fuller", rev, "--"])
    }

    pub fn stash_show(&self, index: usize) -> Result<String> {
        self.git(&[
            "stash",
            "show",
            "--stat",
            "--patch",
            &format!("stash@{{{index}}}"),
        ])
    }

    /// The text of `path` as of HEAD, for gutter diffs.
    pub fn head_text(&self, path: &str) -> Result<String> {
        self.git(&["show", &format!("HEAD:{path}")])
    }

    // ---- writing -------------------------------------------------------

    pub fn stage(&self, path: &str) -> Result<()> {
        self.git(&["add", "-A", "--", path]).map(drop)
    }

    pub fn unstage(&self, path: &str) -> Result<()> {
        if self.has_head() {
            self.git(&["restore", "--staged", "--", path]).map(drop)
        } else {
            self.git(&["rm", "-q", "--cached", "--", path]).map(drop)
        }
    }

    pub fn stage_all(&self) -> Result<()> {
        self.git(&["add", "-A"]).map(drop)
    }

    pub fn unstage_all(&self) -> Result<()> {
        if self.has_head() {
            self.git(&["reset", "-q", "HEAD", "--"]).map(drop)
        } else {
            self.git(&["rm", "-r", "-q", "--cached", "."]).map(drop)
        }
    }

    /// Throw away every change to a file. Not undoable.
    pub fn discard(&self, file: &FileStatus) -> Result<()> {
        if file.is_untracked() {
            std::fs::remove_file(self.root.join(&file.path))
                .with_context(|| format!("failed to delete {}", file.path))?;
        } else if file.index == 'A' {
            self.git(&["rm", "-f", "-q", "--", &file.path])?;
        } else {
            self.git(&["checkout", "HEAD", "--", &file.path])?;
        }
        Ok(())
    }

    pub fn commit(&self, message: &str, amend: bool) -> Result<String> {
        let mut args = vec!["commit", "-m", message];
        if amend {
            args.push("--amend");
        }
        self.git_verbose(&args)
    }

    pub fn checkout(&self, branch: &str) -> Result<String> {
        self.git_verbose(&["checkout", branch, "--"])
    }

    pub fn create_branch(&self, name: &str) -> Result<String> {
        self.git_verbose(&["checkout", "-b", name])
    }

    pub fn delete_branch(&self, name: &str, force: bool) -> Result<String> {
        self.git_verbose(&["branch", if force { "-D" } else { "-d" }, name, "--"])
    }

    pub fn fetch(&self) -> Result<String> {
        self.git_verbose(&["fetch", "--all", "--prune"])
    }

    pub fn pull(&self) -> Result<String> {
        self.git_verbose(&["pull", "--no-edit"])
    }

    pub fn push(&self) -> Result<String> {
        let head = self.head()?;
        if head.upstream.is_none() && !head.detached {
            return self.git_verbose(&["push", "-u", "origin", &head.name]);
        }
        self.git_verbose(&["push"])
    }

    pub fn stash_push(&self, message: Option<&str>) -> Result<String> {
        let mut args = vec!["stash", "push", "--include-untracked"];
        if let Some(message) = message {
            args.extend(["-m", message]);
        }
        self.git_verbose(&args)
    }

    pub fn stash_apply(&self, index: usize) -> Result<String> {
        self.git_verbose(&["stash", "apply", &format!("stash@{{{index}}}")])
    }

    pub fn stash_pop(&self, index: usize) -> Result<String> {
        self.git_verbose(&["stash", "pop", &format!("stash@{{{index}}}")])
    }

    pub fn stash_drop(&self, index: usize) -> Result<String> {
        self.git_verbose(&["stash", "drop", &format!("stash@{{{index}}}")])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TempRepo {
        repo: Repo,
        /// The directory as it was made, which `repo.root` may name by
        /// another path (`/var` and `/private/var` on macOS).
        dir: PathBuf,
    }

    impl TempRepo {
        fn new() -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "med-git-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            let run = |args: &[&str]| {
                let ok = Command::new("git")
                    .arg("-C")
                    .arg(&dir)
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success();
                assert!(ok, "git {args:?}");
            };
            run(&["init", "-q", "-b", "main"]);
            run(&["config", "user.name", "Test"]);
            run(&["config", "user.email", "test@example.invalid"]);
            run(&["config", "commit.gpgsign", "false"]);
            let repo = Repo::discover(&dir).unwrap();
            Self { repo, dir }
        }

        fn write(&self, name: &str, text: &str) {
            fs::write(self.repo.root.join(name), text).unwrap();
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.repo.root);
        }
    }

    #[test]
    fn committed_text_is_the_file_at_head() {
        let t = TempRepo::new();
        let file = t.dir.join("sub").join("a b.txt");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "one\n").unwrap();
        // No commit yet, then not committed.
        assert_eq!(committed_text(&file), None);
        t.repo.stage_all().unwrap();
        t.repo.commit("first", false).unwrap();
        fs::write(&file, "two\n").unwrap();
        assert_eq!(committed_text(&file).as_deref(), Some("one\n"));
        t.write("new.txt", "x\n");
        assert_eq!(committed_text(&t.dir.join("new.txt")), None);
        t.write("bin", "a\0b");
        t.repo.stage_all().unwrap();
        t.repo.commit("binary", false).unwrap();
        assert_eq!(committed_text(&t.dir.join("bin")), None);
        // Not in a repository at all.
        assert_eq!(
            committed_text(&std::env::temp_dir().join("med-no-repo.txt")),
            None
        );
    }

    #[test]
    fn status_stage_commit_cycle() {
        let t = TempRepo::new();
        let repo = &t.repo;
        assert!(repo.log(None, 10).unwrap().is_empty());
        assert_eq!(repo.head().unwrap().name, "main");

        t.write("a.txt", "one\n");
        t.write("sp ace.txt", "x\n");
        let status = repo.status().unwrap();
        assert_eq!(status.len(), 2);
        assert!(status.iter().all(FileStatus::is_untracked));
        assert!(repo.diff_file(&status[0], false).unwrap().contains("+one"));

        repo.stage("a.txt").unwrap();
        let status = repo.status().unwrap();
        let a = status.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!(a.code(), "A ");
        assert!(a.has_staged() && !a.has_unstaged());
        repo.unstage("a.txt").unwrap();
        assert!(repo.status().unwrap().iter().all(FileStatus::is_untracked));

        repo.stage_all().unwrap();
        repo.commit("first", false).unwrap();
        assert!(repo.status().unwrap().is_empty());
        let log = repo.log(None, 10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].subject, "first");
        assert!(repo.show(&log[0].hash).unwrap().contains("+one"));
        assert_eq!(repo.head_text("a.txt").unwrap(), "one\n");

        t.write("a.txt", "two\n");
        let status = repo.status().unwrap();
        assert_eq!(status[0].code(), " M");
        let diff = repo.diff_file(&status[0], false).unwrap();
        assert!(diff.contains("-one") && diff.contains("+two"));
        repo.discard(&status[0]).unwrap();
        assert!(repo.status().unwrap().is_empty());
    }

    #[test]
    fn branches_and_stash() {
        let t = TempRepo::new();
        let repo = &t.repo;
        t.write("a.txt", "one\n");
        repo.stage_all().unwrap();
        repo.commit("first", false).unwrap();

        repo.create_branch("feature").unwrap();
        let branches = repo.branches().unwrap();
        assert_eq!(branches.len(), 2);
        assert!(branches[0].current && branches[0].name == "feature");
        repo.checkout("main").unwrap();
        assert_eq!(repo.head().unwrap().name, "main");
        repo.delete_branch("feature", false).unwrap();
        assert_eq!(repo.branches().unwrap().len(), 1);

        t.write("a.txt", "changed\n");
        repo.stash_push(Some("wip")).unwrap();
        assert!(repo.status().unwrap().is_empty());
        let stashes = repo.stashes().unwrap();
        assert_eq!(stashes.len(), 1);
        assert!(stashes[0].message.contains("wip"));
        assert!(repo.stash_show(0).unwrap().contains("+changed"));
        repo.stash_pop(0).unwrap();
        assert_eq!(repo.status().unwrap().len(), 1);
        assert!(repo.stashes().unwrap().is_empty());
    }

    #[test]
    fn renames_report_original_path() {
        let t = TempRepo::new();
        let repo = &t.repo;
        t.write(
            "old.txt",
            "some content that is long enough to be detected\n",
        );
        repo.stage_all().unwrap();
        repo.commit("first", false).unwrap();
        fs::rename(repo.root.join("old.txt"), repo.root.join("new.txt")).unwrap();
        repo.stage_all().unwrap();
        let status = repo.status().unwrap();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].index, 'R');
        assert_eq!(status[0].path, "new.txt");
        assert_eq!(status[0].orig_path.as_deref(), Some("old.txt"));
    }
}
