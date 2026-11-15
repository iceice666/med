//! What a program can find out about the project: its files, what is in
//! them, and what git has to say.
//!
//! ```lisp
//! (list-dir path)                 ; ((:name :path :dir)...), directories first
//! (list-dir path true)            ; and what git ignores, too
//! (path-name "/a/b.rs")           ; "b.rs"
//! (path-parent "/a/b.rs")         ; "/a", and nil when there is none
//! (path-join "/a" "b.rs")         ; "/a/b.rs"
//!
//! (file-exists? path)             ; is there anything there
//! (dir? path)                     ; a directory, or a link to one
//! (rename-file from to)           ; nil, or what went wrong
//! (delete-file path)              ; to the trash: nil, or what went wrong
//! (make-dir path)                 ; and the ones above it
//!
//! (walk-files (fn (files) ...))   ; every file, as a path from the root
//! (fuzzy "mnrs" files 100)        ; the ones a few letters mean, best first
//! (grep "needle" 500 (fn (places) ...))   ; ((:path :file :line :col :text)...)
//!
//! (git 'status (fn (files problem) ...))
//! (git 'commit "message" (fn (output problem) ...))
//! ```
//!
//! A path that does not start at the top is the project's. `rename-file`
//! never puts one file in the place of another, and the tabs that had the
//! file, or one in the directory, go with it.
//!
//! What takes time is given a function to call when it is done: walking
//! the project, searching it, and everything git does. A git function is
//! called with the answer and nil, or with nil and what went wrong.
//!
//! What `git` can be asked, each followed by the function to call:
//! - `'status`: the changed files, each `(:path :file :index :worktree
//!   :staged :unstaged :untracked :orig)`. `:path` is the whole path and
//!   `:file` the path in the repository; `:index` and `:worktree` are
//!   git's letters for the file.
//! - `'head`: `(:name :detached :upstream :ahead :behind :root)`.
//! - `'branches`: each `(:name :current :upstream :track :date :subject)`.
//! - `'log revision limit`: the commits from `revision` (nil for where the
//!   repository is), each `(:hash :short :author :date :refs :subject)`.
//! - `'stashes`: each `(:index :date :message)`.
//! - `'diff file staged`, `'diff-all`, `'show revision`, `'stash-show
//!   index`: text. `file` is one of `'status`'s.
//! - `'stage path`, `'unstage path`, `'stage-all`, `'unstage-all`,
//!   `'discard file`, `'commit message`, `'amend message`, `'checkout
//!   branch`, `'create-branch name`, `'delete-branch name`, `'fetch`,
//!   `'pull`, `'push`, `'stash`, `'stash-apply index`, `'stash-pop index`,
//!   `'stash-drop index`: what git printed.

use crate::panel::{is_function, word, wrong};
use crate::script::{self, Script};
use crate::search::{self, Hit};
use anyhow::Context as _;
use gpui::App;
use med_git::{Branch, Commit, FileStatus, Head, Repo, Stash};
use med_lisp::{Arity, Error, ErrorKind, Interp, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How long a search waits for more to be typed before it starts.
const SEARCH_DELAY: Duration = Duration::from_millis(120);

fn failed(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Raised, message)
}

fn path_value(path: &Path) -> Value {
    Value::from(path.display().to_string())
}

/// A list that goes name, value, name, value.
pub(crate) fn record<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::list(
        fields
            .into_iter()
            .flat_map(|(name, value)| [Value::keyword(name), value]),
    )
}

fn text_or_nil(text: Option<&str>) -> Value {
    text.map_or(Value::Nil, Value::from)
}

/// The function a program gave to be called later: the last argument.
fn callback(name: &str, value: &Value) -> Result<Value, Error> {
    match is_function(value) {
        true => Ok(value.clone()),
        false => Err(wrong(name, "a function to call, last", value)),
    }
}

/// How many of something a program asked for.
fn count(name: &str, value: &Value) -> Result<usize, Error> {
    match value {
        Value::Int(count) if *count >= 0 => Ok(*count as usize),
        other => Err(wrong(name, "a count", other)),
    }
}

/// The project of the window the program was started from.
fn project(name: &str, cx: &App) -> Result<PathBuf, Error> {
    script::project_root(cx).ok_or_else(|| failed(format!("{name}: there is no project open")))
}

fn hit_value(hit: Hit) -> Value {
    record([
        ("path", path_value(&hit.path)),
        ("file", Value::from(hit.file)),
        ("line", Value::Int(hit.line as i64 + 1)),
        ("col", Value::Int(hit.column as i64 + 1)),
        ("text", Value::from(hit.text)),
    ])
}

/// A place for the `locations` hook: a line of a file, as `grep` gives
/// them. `line` and `column` count from 0.
pub(crate) fn location(root: &Path, path: PathBuf, line: usize, column: usize) -> Value {
    let text = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| text.lines().nth(line).map(|line| line.trim().to_string()))
        .unwrap_or_default();
    let file = path
        .strip_prefix(root)
        .unwrap_or(&path)
        .display()
        .to_string();
    hit_value(Hit {
        path,
        file,
        line,
        column,
        text,
    })
}

pub(crate) fn define_functions(interp: &mut Interp) {
    // ---- paths
    interp.define_native("path-name", 1, |_, args| {
        let name = Path::new(args[0].as_str()?).file_name();
        Ok(name.map_or(Value::Nil, |name| {
            Value::from(name.to_string_lossy().as_ref())
        }))
    });
    interp.define_native("path-parent", 1, |_, args| {
        let parent = Path::new(args[0].as_str()?).parent();
        // The parent of a bare name is nowhere, not the empty path.
        let parent = parent.filter(|parent| !parent.as_os_str().is_empty());
        Ok(parent.map_or(Value::Nil, path_value))
    });
    interp.define_native("path-join", Arity::at_least(1), |_, args| {
        let mut path = PathBuf::new();
        for part in args {
            path.push(part.as_str()?);
        }
        Ok(path_value(&path))
    });

    // (list-dir path [all]): what is in a directory, directories first,
    // without what git ignores unless `all`. Nothing, if it cannot be read.
    interp.define_native("list-dir", Arity::between(1, 2), |_, args| {
        let dir = Path::new(args[0].as_str()?);
        let entries = match args.get(1).is_some_and(Value::is_truthy) {
            true => search::list_dir_all(dir),
            false => search::list_dir(dir),
        };
        Ok(Value::list(entries.into_iter().map(|(path, is_dir)| {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            record([
                ("name", Value::from(name.as_ref())),
                ("path", path_value(&path)),
                ("dir", Value::Bool(is_dir)),
            ])
        })))
    });

    // ---- changing what is on disk

    // (file-exists? path): whether there is a file, a directory or a link
    // there, even one to nowhere.
    interp.define_native("file-exists?", 1, |call, args| {
        let path = on_disk("file-exists?", &args[0], script::app(call)?)?;
        Ok(Value::Bool(path.symlink_metadata().is_ok()))
    });

    // (dir? path): whether there is a directory there, or a link to one.
    interp.define_native("dir?", 1, |call, args| {
        let path = on_disk("dir?", &args[0], script::app(call)?)?;
        Ok(Value::Bool(path.is_dir()))
    });

    // (rename-file from to): move a file or a directory. Nil once it is
    // done, or else what went wrong, the place being taken among it.
    interp.define_native("rename-file", 2, |call, args| {
        let cx = script::app(call)?;
        let from = on_disk("rename-file", &args[0], cx)?;
        let to = on_disk("rename-file", &args[1], cx)?;
        let (from, to) = (canonical(&from), canonical(&to));
        match rename(&from, &to) {
            Ok(()) => {
                script::in_workspace(cx, |workspace, _, cx| workspace.renamed(&from, &to, cx));
                Ok(Value::Nil)
            }
            Err(problem) => Ok(Value::from(problem)),
        }
    });

    // (delete-file path): put a file or a directory in the trash. Nil, or
    // what went wrong.
    interp.define_native("delete-file", 1, |call, args| {
        let path = on_disk("delete-file", &args[0], script::app(call)?)?;
        if path.symlink_metadata().is_err() {
            return Ok(Value::from(format!(
                "{}: there is nothing there",
                path.display()
            )));
        }
        match to_trash(&path) {
            Ok(()) => Ok(Value::Nil),
            Err(error) => Ok(Value::from(format!("{}: {error}", path.display()))),
        }
    });

    // (make-dir path): a directory, and the ones it is in. Nil, or what
    // went wrong.
    interp.define_native("make-dir", 1, |call, args| {
        let path = on_disk("make-dir", &args[0], script::app(call)?)?;
        match std::fs::create_dir_all(&path) {
            Ok(()) => Ok(Value::Nil),
            Err(error) => Ok(Value::from(format!("{}: {error}", path.display()))),
        }
    });

    // (walk-files function): every file of the project that git would not
    // ignore, as paths from its root.
    interp.define_native("walk-files", 1, |call, args| {
        let callback = callback("walk-files", &args[0])?;
        let cx = script::app(call)?;
        let root = project("walk-files", cx)?;
        script::in_background(
            cx,
            callback,
            move || search::walk_files(&root),
            |files| vec![Value::list(files.into_iter().map(Value::from))],
        );
        Ok(Value::Nil)
    });

    // (fuzzy query list limit): the texts of the list the query's letters
    // are found in, in order, best first.
    interp.define_native("fuzzy", 3, |_, args| {
        let query = args[0].as_str()?;
        let texts = args[1]
            .to_vec()?
            .iter()
            .map(|text| text.as_str().map(str::to_string))
            .collect::<Result<Vec<_>, _>>()?;
        let found = search::fuzzy_files(&texts, query, count("fuzzy", &args[2])?);
        Ok(Value::list(found.into_iter().map(Value::from)))
    });

    // (grep query limit function): the lines of the project's files that
    // match, as a regular expression if it is one. It waits a moment
    // first, and gives up without calling the function if another search
    // is asked for meanwhile: the last one asked for is the one answered.
    interp.define_native("grep", 3, |call, args| {
        let query = args[0].as_str()?.to_string();
        let limit = count("grep", &args[1])?;
        let callback = callback("grep", &args[2])?;
        let cx = script::app(call)?;
        let root = project("grep", cx)?;
        let origin = script::origin(cx);

        let cancel = Arc::new(AtomicBool::new(false));
        let script = cx.global_mut::<Script>();
        std::mem::replace(&mut script.search, cancel.clone()).store(true, Ordering::Relaxed);
        let executor = cx.background_executor().clone();
        cx.spawn(async move |cx| {
            executor.timer(SEARCH_DELAY).await;
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let flag = cancel.clone();
            let hits = executor
                .spawn(async move { search::grep(&root, &query, limit, &flag) })
                .await;
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let places = Value::list(hits.into_iter().map(hit_value));
            cx.update(|cx| script::call_back(origin, callback, vec![places], cx));
        })
        .detach();
        Ok(Value::Nil)
    });

    // (git what argument... function): see the top of this file.
    interp.define_native("git", Arity::at_least(2), |call, args| {
        let [what, params @ .., last] = args else {
            unreachable!("git takes at least two arguments");
        };
        let callback = callback("git", last)?;
        let job = git_job(what, params)?;
        let cx = script::app(call)?;
        let root = project("git", cx)?;
        script::in_background(
            cx,
            callback,
            move || {
                let repo = Repo::discover(&root).context("not a git repository")?;
                job(&repo)
            },
            |result| match result {
                Ok(reply) => vec![reply.into_value(), Value::Nil],
                Err(error) => vec![Value::Nil, Value::from(format!("{error:#}"))],
            },
        );
        Ok(Value::Nil)
    });
}

// ---- files -------------------------------------------------------------

/// A path a program gave: from the top, or else the project's.
fn on_disk(name: &str, value: &Value, cx: &App) -> Result<PathBuf, Error> {
    let path = PathBuf::from(value.as_str()?);
    if path.as_os_str().is_empty() {
        return Err(failed(format!("{name}: an empty path is nowhere")));
    }
    match path.is_absolute() {
        true => Ok(path),
        false => Ok(project(name, cx)?.join(path)),
    }
}

/// `path`, with the directory it is in as tabs have it: a link at the end
/// is left as it is, for it is what moves.
fn canonical(path: &Path) -> PathBuf {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return path.to_path_buf();
    };
    match parent.canonicalize() {
        Ok(parent) => parent.join(name),
        Err(_) => path.to_path_buf(),
    }
}

/// Move `from` to `to`, unless something is there already. On a disk that
/// does not tell `a` from `A`, `to` may be `from` by another name.
fn rename(from: &Path, to: &Path) -> Result<(), String> {
    let source = from
        .symlink_metadata()
        .map_err(|error| format!("{}: {error}", from.display()))?;
    if let Ok(there) = to.symlink_metadata()
        && !same_file(&source, &there)
    {
        return Err(format!(
            "{}: there is something there already",
            to.display()
        ));
    }
    std::fs::rename(from, to)
        .map_err(|error| format!("{} → {}: {error}", from.display(), to.display()))
}

/// Put `path` in the trash, by the way that asks for no more permission.
#[cfg(not(test))]
fn to_trash(path: &Path) -> Result<(), trash::Error> {
    #[allow(unused_mut)]
    let mut context = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos as _};
        context.set_delete_method(DeleteMethod::NsFileManager);
    }
    context.delete(path)
}

/// Tests would fill the trash of whoever runs them: theirs are gone.
#[cfg(test)]
fn to_trash(path: &Path) -> Result<(), std::io::Error> {
    match path.symlink_metadata()?.is_dir() {
        true => std::fs::remove_dir_all(path),
        false => std::fs::remove_file(path),
    }
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(_: &std::fs::Metadata, _: &std::fs::Metadata) -> bool {
    false
}

// ---- git ---------------------------------------------------------------

/// What git came back with, still as it left the thread that asked.
enum Reply {
    Text(String),
    Status(PathBuf, Vec<FileStatus>),
    Head(PathBuf, Head),
    Branches(Vec<Branch>),
    Commits(Vec<Commit>),
    Stashes(Vec<Stash>),
}

type Job = Box<dyn FnOnce(&Repo) -> anyhow::Result<Reply> + Send>;

fn letter(letter: char) -> Value {
    Value::from(letter.to_string())
}

fn file_value(root: &Path, file: &FileStatus) -> Value {
    record([
        ("path", path_value(&root.join(&file.path))),
        ("file", Value::from(file.path.as_str())),
        ("index", letter(file.index)),
        ("worktree", letter(file.worktree)),
        ("staged", Value::Bool(file.has_staged())),
        ("unstaged", Value::Bool(file.has_unstaged())),
        ("untracked", Value::Bool(file.is_untracked())),
        ("orig", text_or_nil(file.orig_path.as_deref())),
    ])
}

/// One of `'status`'s files, given back to say which file is meant.
fn file_status(value: &Value) -> Result<FileStatus, Error> {
    let not_a_file = || wrong("git", "one of the files of 'status", value);
    let fields = value.to_vec().map_err(|_| not_a_file())?;
    let field = |name: &str| {
        let key = Value::keyword(name);
        let at = fields.chunks(2).find(|pair| pair[0] == key);
        at.and_then(|pair| pair.get(1))
    };
    let text = |name: &str| match field(name) {
        Some(Value::Str(text)) => Ok(text.to_string()),
        _ => Err(not_a_file()),
    };
    let letter = |name: &str| text(name)?.chars().next().ok_or_else(not_a_file);
    Ok(FileStatus {
        path: text("file")?,
        orig_path: text("orig").ok(),
        index: letter("index")?,
        worktree: letter("worktree")?,
    })
}

impl Reply {
    fn into_value(self) -> Value {
        match self {
            Reply::Text(text) => Value::from(text),
            Reply::Status(root, files) => {
                Value::list(files.iter().map(|file| file_value(&root, file)))
            }
            Reply::Head(root, head) => record([
                ("name", Value::from(head.name)),
                ("detached", Value::Bool(head.detached)),
                ("upstream", text_or_nil(head.upstream.as_deref())),
                ("ahead", Value::Int(head.ahead as i64)),
                ("behind", Value::Int(head.behind as i64)),
                ("root", path_value(&root)),
            ]),
            Reply::Branches(branches) => Value::list(branches.into_iter().map(|branch| {
                record([
                    ("name", Value::from(branch.name)),
                    ("current", Value::Bool(branch.current)),
                    ("upstream", text_or_nil(branch.upstream.as_deref())),
                    ("track", Value::from(branch.track)),
                    ("date", Value::from(branch.date)),
                    ("subject", Value::from(branch.subject)),
                ])
            })),
            Reply::Commits(commits) => Value::list(commits.into_iter().map(|commit| {
                record([
                    ("hash", Value::from(commit.hash)),
                    ("short", Value::from(commit.short)),
                    ("author", Value::from(commit.author)),
                    ("date", Value::from(commit.date)),
                    ("refs", Value::from(commit.refs)),
                    ("subject", Value::from(commit.subject)),
                ])
            })),
            Reply::Stashes(stashes) => Value::list(stashes.into_iter().map(|stash| {
                record([
                    ("index", Value::Int(stash.index as i64)),
                    ("date", Value::from(stash.date)),
                    ("message", Value::from(stash.message)),
                ])
            })),
        }
    }
}

/// What is to be done in the repository for `(git what params...)`.
fn git_job(what: &Value, params: &[Value]) -> Result<Job, Error> {
    let what = word(what).ok_or_else(|| wrong("git", "what to do, as a name", what))?;
    let expects = |described: &str| {
        failed(format!(
            "git {what}: expected {described}, then the function to call"
        ))
    };
    let text = |value: &Value, described: &str| match value {
        Value::Str(text) => Ok(text.to_string()),
        _ => Err(expects(described)),
    };
    let index = |value: &Value, described: &str| match value {
        Value::Int(index) if *index >= 0 => Ok(*index as usize),
        _ => Err(expects(described)),
    };
    // Something git does and has something to say about.
    fn said(run: impl FnOnce(&Repo) -> anyhow::Result<String> + Send + 'static) -> Job {
        Box::new(move |repo| run(repo).map(Reply::Text))
    }
    // Something git does in silence.
    fn done(run: impl FnOnce(&Repo) -> anyhow::Result<()> + Send + 'static) -> Job {
        Box::new(move |repo| run(repo).map(|()| Reply::Text(String::new())))
    }

    Ok(match (what.as_str(), params) {
        ("status", []) => Box::new(|repo| Ok(Reply::Status(repo.root.clone(), repo.status()?))),
        ("head", []) => Box::new(|repo| Ok(Reply::Head(repo.root.clone(), repo.head()?))),
        ("branches", []) => Box::new(|repo| Ok(Reply::Branches(repo.branches()?))),
        ("log", [revision, limit]) => {
            let revision = match revision {
                Value::Nil => None,
                revision => Some(text(revision, "a revision or nil, and a count")?),
            };
            let limit = index(limit, "a revision or nil, and a count")?;
            Box::new(move |repo| Ok(Reply::Commits(repo.log(revision.as_deref(), limit)?)))
        }
        ("stashes", []) => Box::new(|repo| Ok(Reply::Stashes(repo.stashes()?))),

        ("diff", [file, staged]) => {
            let (file, staged) = (file_status(file)?, staged.is_truthy());
            said(move |repo| repo.diff_file(&file, staged))
        }
        ("diff-all", []) => said(|repo| repo.diff_all()),
        ("show", [revision]) => {
            let revision = text(revision, "a revision")?;
            said(move |repo| repo.show(&revision))
        }
        ("stash-show", [stash]) => {
            let stash = index(stash, "which stash")?;
            said(move |repo| repo.stash_show(stash))
        }

        ("stage", [path]) => {
            let path = text(path, "a path")?;
            done(move |repo| repo.stage(&path))
        }
        ("unstage", [path]) => {
            let path = text(path, "a path")?;
            done(move |repo| repo.unstage(&path))
        }
        ("stage-all", []) => done(|repo| repo.stage_all()),
        ("unstage-all", []) => done(|repo| repo.unstage_all()),
        ("discard", [file]) => {
            let file = file_status(file)?;
            done(move |repo| repo.discard(&file))
        }
        ("commit", [message]) => {
            let message = text(message, "a message")?;
            said(move |repo| repo.commit(&message, false))
        }
        ("amend", [message]) => {
            let message = text(message, "a message")?;
            said(move |repo| repo.commit(&message, true))
        }
        ("checkout", [branch]) => {
            let branch = text(branch, "a branch")?;
            said(move |repo| repo.checkout(&branch))
        }
        ("create-branch", [name]) => {
            let name = text(name, "a name")?;
            said(move |repo| repo.create_branch(&name))
        }
        ("delete-branch", [name]) => {
            let name = text(name, "a name")?;
            said(move |repo| repo.delete_branch(&name, false))
        }
        ("fetch", []) => said(|repo| repo.fetch()),
        ("pull", []) => said(|repo| repo.pull()),
        ("push", []) => said(|repo| repo.push()),
        ("stash", []) => said(|repo| repo.stash_push(None)),
        ("stash-apply", [stash]) => {
            let stash = index(stash, "which stash")?;
            said(move |repo| repo.stash_apply(stash))
        }
        ("stash-pop", [stash]) => {
            let stash = index(stash, "which stash")?;
            said(move |repo| repo.stash_pop(stash))
        }
        ("stash-drop", [stash]) => {
            let stash = index(stash, "which stash")?;
            said(move |repo| repo.stash_drop(stash))
        }
        _ => {
            return Err(failed(format!(
                "git: {what} with {} argument(s) is nothing it can be asked",
                params.len()
            )));
        }
    })
}
