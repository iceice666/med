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
//! (file-info path)                ; (:name :path :kind :dir :size :modified
//!                                 ;  :mode :links :uid :gid :target), or nil
//! (rename-file from to)           ; nil, or what went wrong
//! (copy-file from to)             ; and all in it: nil, or what went wrong
//! (copy-file from to (fn (problem) ...))  ; the same, in the background
//! (delete-file path)              ; to the trash: nil, or what went wrong
//! (delete-file path true)         ; gone for good
//! (make-dir path)                 ; and the ones above it
//! (make-file path)                ; an empty file, never in another's place
//! (make-link target path)         ; a symbolic link at path, to target
//! (make-link target path true)    ; a hard one
//! (set-file-mode path "755")      ; or 493: the permissions, as chmod
//!
//! (spawn "ls" '("-l") dir (fn (done problem) ...))  ; (:status :out :err)
//!
//! (regex-match "(\\w+)\\.rs$" "a/main.rs")    ; ("main.rs" "main"), or nil
//! (regex-replace "(\\w+)\\.txt" text "$1.md") ; every match replaced
//!
//! (walk-files (fn (files) ...))   ; every file, as a path from the root
//! (fuzzy "mnrs" files 100)        ; the ones a few letters mean, best first
//! (grep "needle" 500 (fn (places) ...))   ; ((:path :file :line :col :text)...)
//! (workspace-symbols "name" 500 (fn (places) ...))  ; the same, :text the symbol
//! (structural-search "$x$.unwrap()" 'rust 500 (fn (places) ...))
//!                                 ; code of that shape: grep's places and
//!                                 ; :end-line :end-col :match :captures
//! (structural-search "$x$.unwrap()" 'rust 500 "$x$?" (fn (places) ...))
//!                                 ; and each one's :replacement
//! (structural-replace "$x$.unwrap()" "$x$?" nil (fn (done) ...))
//!                                 ; (:changes :files :skipped)
//! (structural-replace "$x$.unwrap()" "$x$?" nil places (fn (done) ...))
//!                                 ; only those structural-search gave
//!
//! (git 'status (fn (files problem) ...))
//! (git 'commit "message" (fn (output problem) ...))
//! ```
//!
//! A path that does not start at the top is the project's. `rename-file`,
//! `copy-file` and `make-link` never put one file in the place of another,
//! and the tabs that had a renamed file, or one in the directory, go with
//! it.
//!
//! `file-info` is of what is at the path, a link itself and not what it
//! points to: `:kind` is `'file`, `'dir`, `'link` or `'other`, and `:dir`
//! whether it is a directory or a link to one. `:size` is in bytes,
//! `:modified` in seconds since 1970, `:mode` the permission bits as a
//! number (493 for 755), `:links` how many names the file has, `:uid`
//! and `:gid` its owner and group as numbers, and `:target` where a link
//! points, as it was written. What the system does not tell is nil.
//!
//! `copy-file` copies a directory with all in it, links as links, and
//! keeps when each file was last changed.
//!
//! What takes time is given a function to call when it is done: walking
//! the project, searching it, everything git does, and a program
//! `spawn`ed. A git function is called with the answer and nil, or with
//! nil and what went wrong. `spawn` runs a program with no shell between
//! (`(spawn "sh" (list "-c" line) ...)` for one), in `dir`, nil for the
//! project's, with nothing to read, in the project's environment (see
//! `:workspace.reload_env`; a program spawned while it is being worked out
//! waits for it); its function is called with `(:status :out :err)`, the
//! exit code (nil when a signal stopped it) and what it printed, and nil,
//! or with nil and why it could not be run.
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

use crate::buffer_api::field;
use crate::env::{EnvCell, ProjectEnv};
use crate::panel::{is_function, word, wrong};
use crate::script::{self, Script};
use crate::search::{self, Hit};
use crate::{structural, syntax};
use anyhow::Context as _;
use gpui::App;
use med_git::{Branch, Commit, FileStatus, Head, Repo, Stash};
use med_lisp::{Arity, Error, ErrorKind, Interp, Value};
use std::collections::HashMap;
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
/// them, its text `label` when there is one. `line` and `column` count
/// from 0.
pub(crate) fn location(
    root: &Path,
    path: PathBuf,
    line: usize,
    column: usize,
    label: Option<String>,
) -> Value {
    let text = label.unwrap_or_else(|| {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.lines().nth(line).map(|line| line.trim().to_string()))
            .unwrap_or_default()
    });
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

    // (file-info path): what is there, a link and not what it points to;
    // nil for nothing. See the top of this file.
    interp.define_native("file-info", 1, |call, args| {
        let path = on_disk("file-info", &args[0], script::app(call)?)?;
        Ok(match path.symlink_metadata() {
            Ok(meta) => info_value(&path, &meta),
            Err(_) => Value::Nil,
        })
    });

    // (copy-file from to [function]): a copy of a file, or of a directory
    // and all in it. Nil, or what went wrong; given a function, it is done
    // in the background and the function is called with that.
    interp.define_native("copy-file", Arity::between(2, 3), |call, args| {
        let cx = script::app(call)?;
        let from = on_disk("copy-file", &args[0], cx)?;
        let to = on_disk("copy-file", &args[1], cx)?;
        let Some(function) = args.get(2) else {
            return Ok(said(copy(&from, &to)));
        };
        let callback = callback("copy-file", function)?;
        script::in_background(
            cx,
            callback,
            move || copy(&from, &to),
            |done| vec![said(done)],
        );
        Ok(Value::Nil)
    });

    // (delete-file path [forever]): put a file or a directory in the
    // trash, or do away with it. Nil, or what went wrong.
    interp.define_native("delete-file", Arity::between(1, 2), |call, args| {
        let path = on_disk("delete-file", &args[0], script::app(call)?)?;
        if path.symlink_metadata().is_err() {
            return Ok(Value::from(format!(
                "{}: there is nothing there",
                path.display()
            )));
        }
        let done = match args.get(1).is_some_and(Value::is_truthy) {
            true => remove(&path).map_err(|error| error.to_string()),
            false => to_trash(&path).map_err(|error| error.to_string()),
        };
        Ok(said(
            done.map_err(|error| format!("{}: {error}", path.display())),
        ))
    });

    // (make-dir path): a directory, and the ones it is in. Nil, or what
    // went wrong.
    interp.define_native("make-dir", 1, |call, args| {
        let path = on_disk("make-dir", &args[0], script::app(call)?)?;
        Ok(said(
            std::fs::create_dir_all(&path).map_err(wrong_at(&path)),
        ))
    });

    // (make-file path): an empty file, never in the place of anything
    // that is there. Nil, or what went wrong.
    interp.define_native("make-file", 1, |call, args| {
        let path = on_disk("make-file", &args[0], script::app(call)?)?;
        let made = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map(drop);
        Ok(said(made.map_err(wrong_at(&path))))
    });

    // (make-link target path [hard]): a symbolic link at `path` that
    // points to `target`, as written, or a hard link to it. Nil, or what
    // went wrong.
    interp.define_native("make-link", Arity::between(2, 3), |call, args| {
        let cx = script::app(call)?;
        let path = on_disk("make-link", &args[1], cx)?;
        let made = match args.get(2).is_some_and(Value::is_truthy) {
            true => std::fs::hard_link(on_disk("make-link", &args[0], cx)?, &path),
            false => symlink(Path::new(args[0].as_str()?), &path),
        };
        Ok(said(made.map_err(wrong_at(&path))))
    });

    // (set-file-mode path mode): the permissions of a file, or of what a
    // link points to, as chmod: a number, or its octal digits as text.
    // Nil, or what went wrong.
    interp.define_native("set-file-mode", 2, |call, args| {
        let path = on_disk("set-file-mode", &args[0], script::app(call)?)?;
        let mode = file_mode(&args[1])?;
        Ok(said(set_mode(&path, mode).map_err(wrong_at(&path))))
    });

    // (spawn program arguments dir function): run a program. See the top
    // of this file.
    interp.define_native("spawn", 4, |call, args| {
        let program = args[0].as_str()?.to_string();
        let arguments = args[1]
            .to_vec()?
            .iter()
            .map(|argument| argument.as_str().map(str::to_string))
            .collect::<Result<Vec<_>, _>>()?;
        let callback = callback("spawn", &args[3])?;
        let cx = script::app(call)?;
        let dir = match &args[2] {
            Value::Nil => project("spawn", cx)?,
            dir => on_disk("spawn", dir, cx)?,
        };
        // The project's environment; med's own with no project.
        let cell =
            script::project_env(cx).unwrap_or_else(|| EnvCell::ready(ProjectEnv::inherited()));
        let values = |result: Result<Ran, String>| match result {
            Ok(done) => vec![done.into_value(), Value::Nil],
            Err(problem) => vec![Value::Nil, Value::from(problem)],
        };
        match cell.get() {
            Some(env) => script::in_background(
                cx,
                callback,
                move || run_program(&env, &program, &arguments, &dir),
                values,
            ),
            // Still being worked out, which can take minutes: wait for it
            // on a thread of its own, not one of the executor's.
            None => script::on_thread(
                cx,
                callback,
                move || run_program(&cell.wait(), &program, &arguments, &dir),
                values,
            ),
        }
        Ok(Value::Nil)
    });

    // ---- text

    // (regex-match pattern text): the first match of a regular expression,
    // as the text it matched and then each group's, nil for a group that
    // took no part; nil when nothing matches.
    interp.define_native("regex-match", 2, |_, args| {
        let regex = compiled("regex-match", &args[0])?;
        let found = regex.captures(args[1].as_str()?);
        Ok(found.map_or(Value::Nil, |groups| {
            Value::list(
                groups
                    .iter()
                    .map(|group| group.map_or(Value::Nil, |group| Value::from(group.as_str()))),
            )
        }))
    });

    // (regex-replace pattern text replacement): every match made the
    // replacement, in which `$1` and `${name}` are what a group matched.
    interp.define_native("regex-replace", 3, |_, args| {
        let regex = compiled("regex-replace", &args[0])?;
        let text = args[1].as_str()?;
        let replaced = regex.replace_all(text, args[2].as_str()?);
        Ok(Value::from(replaced.as_ref()))
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

    // (workspace-symbols query limit function): the symbols of the project
    // whose names match, as the language servers running find them, each
    // a place as `grep` gives them with the symbol's kind and name as its
    // text; none when no server running can look. As with `grep`, the last
    // one asked for is the one answered.
    interp.define_native("workspace-symbols", 3, |call, args| {
        let query = args[0].as_str()?.to_string();
        let limit = count("workspace-symbols", &args[1])?;
        let callback = callback("workspace-symbols", &args[2])?;
        let cx = script::app(call)?;
        let root = project("workspace-symbols", cx)?;
        let origin = script::origin(cx);
        let Some(lsp) = script::in_workspace(cx, |workspace, _, _| workspace.lsp()) else {
            return Err(failed("workspace-symbols: there is no window to look from"));
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let script = cx.global_mut::<Script>();
        std::mem::replace(&mut script.search, cancel.clone()).store(true, Ordering::Relaxed);
        let executor = cx.background_executor().clone();
        cx.spawn(async move |cx| {
            executor.timer(SEARCH_DELAY).await;
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let request = cx.update(|cx| lsp.read(cx).workspace_symbols(&query));
            let found = match request {
                Some(request) => request.await,
                None => Vec::new(),
            };
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let places = Value::list(found.into_iter().take(limit).filter_map(|place| {
                let path = crate::lsp::uri_to_path(&place.location.uri)?;
                let start = place.location.range.start;
                let (line, column) = (start.line as usize, start.character as usize);
                Some(location(&root, path, line, column, place.label))
            }));
            cx.update(|cx| script::call_back(origin, callback, vec![places], cx));
        })
        .detach();
        Ok(Value::Nil)
    });

    // (structural-search pattern language limit [replacement] function):
    // the code of the project that has the pattern's shape, as `grep`'s
    // places with their ends, the text matched and what each variable
    // stands for; given a replacement, what each would be made, nil for
    // one a replace leaves. See `structural.rs`. As with `grep`, the last
    // one asked for is the one answered.
    interp.define_native("structural-search", Arity::between(4, 5), |call, args| {
        let cx = script::app(call)?;
        let pattern = pattern("structural-search", &args[0], &args[1], cx)?;
        let limit = count("structural-search", &args[2])?;
        let replacement = match args.len() {
            5 => Some(replacement("structural-search", &args[3], &pattern)?),
            _ => None,
        };
        let callback = callback("structural-search", args.last().unwrap())?;
        let root = project("structural-search", cx)?;
        let open = open_texts(cx);
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
            let files = executor
                .spawn(async move { structural::search(&root, &pattern, &open, limit, &flag) })
                .await;
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let places = Value::list(files.iter().flat_map(|file| {
                file.found
                    .iter()
                    .map(|found| structural_place(file, found, replacement.as_ref()))
            }));
            cx.update(|cx| script::call_back(origin, callback, vec![places], cx));
        })
        .detach();
        Ok(Value::Nil)
    });

    // (structural-replace pattern replacement language [places] function):
    // every match of the pattern in the project made the replacement, or
    // only those of `places`, as `structural-search` gave them; in the
    // tabs that have the files and on disk for the rest. A match in
    // another is left for the next replace. The function is called with
    // `(:changes :files :skipped)`, `:skipped` the matches left as they
    // are: in a file that changed while they were being found, or with a
    // comment in no variable, which the replacement would lose.
    interp.define_native("structural-replace", Arity::between(4, 5), |call, args| {
        let cx = script::app(call)?;
        let pattern = pattern("structural-replace", &args[0], &args[2], cx)?;
        let replacement = replacement("structural-replace", &args[1], &pattern)?;
        let chosen = match args.len() {
            5 => Some(chosen_places(&args[3])?),
            _ => None,
        };
        let callback = callback("structural-replace", args.last().unwrap())?;
        let root = project("structural-replace", cx)?;
        let open = open_texts(cx);
        let origin = script::origin(cx);
        let work = cx.background_executor().spawn(async move {
            let never = AtomicBool::new(false);
            let files = structural::search(&root, &pattern, &open, usize::MAX, &never);
            structural::edits(files, &replacement, chosen.as_deref())
        });
        cx.spawn(async move |cx| {
            let edits = work.await;
            cx.update(|cx| {
                let replaced = origin.as_ref().and_then(|origin| {
                    script::in_origin(origin, cx, |workspace, window, cx| {
                        structural::apply(workspace, edits, window, cx)
                    })
                });
                let done = replaced.map_or(Value::Nil, |replaced| {
                    record([
                        ("changes", Value::Int(replaced.changes as i64)),
                        ("files", Value::Int(replaced.files as i64)),
                        ("skipped", Value::Int(replaced.skipped as i64)),
                    ])
                });
                script::call_back(origin, callback, vec![done], cx);
            });
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

/// A pattern for `structural-search` or `structural-replace`, of the
/// language a program named, or of the tab up's for nil.
fn pattern(
    name: &str,
    source: &Value,
    language: &Value,
    cx: &mut App,
) -> Result<structural::Pattern, Error> {
    let lang = match language {
        Value::Nil => script::in_workspace(cx, |workspace, _, cx| {
            workspace.active_editor()?.read(cx).lang
        })
        .flatten(),
        other => {
            let language = word(other).ok_or_else(|| wrong(name, "a language", other))?;
            syntax::lang_named(&language)
        }
    };
    let Some(lang) = lang else {
        return Err(failed(match language {
            Value::Nil => format!("{name}: the tab up is of no language"),
            other => format!("{name}: there is no such language: {other}"),
        }));
    };
    structural::Pattern::new(source.as_str()?, lang)
        .map_err(|problem| failed(format!("{name}: {problem}")))
}

/// The text of each file open in a tab of the program's window.
fn open_texts(cx: &mut App) -> HashMap<PathBuf, String> {
    script::in_workspace(cx, |workspace, _, cx| workspace.open_texts(cx)).unwrap_or_default()
}

/// A match of `structural-search`: where it starts, as `grep` gives it,
/// where it ends, what it is, and what each variable stands for there.
/// A replacement for a pattern a program gave.
fn replacement(
    name: &str,
    source: &Value,
    pattern: &structural::Pattern,
) -> Result<structural::Replacement, Error> {
    structural::Replacement::new(source.as_str()?, pattern)
        .map_err(|problem| failed(format!("{name}: {problem}")))
}

/// The matches a program chose, as `structural-search` gave them.
fn chosen_places(places: &Value) -> Result<Vec<structural::Chosen>, Error> {
    let at = |fields: &[Value], key: &str| -> Result<usize, Error> {
        match field(fields, key) {
            Some(Value::Int(n)) if *n >= 1 => Ok(*n as usize - 1),
            other => Err(wrong(
                "structural-replace",
                &format!("a place with {key}"),
                other.unwrap_or(&Value::Nil),
            )),
        }
    };
    places
        .to_vec()?
        .iter()
        .map(|place| {
            let fields = place.to_vec()?;
            let path = match field(&fields, "path") {
                Some(path) => PathBuf::from(path.as_str()?),
                None => return Err(wrong("structural-replace", "a place with :path", place)),
            };
            Ok(structural::Chosen {
                path,
                start: (at(&fields, "line")?, at(&fields, "col")?),
                end: (at(&fields, "end-line")?, at(&fields, "end-col")?),
            })
        })
        .collect()
}

fn structural_place(
    file: &structural::FileFound,
    found: &structural::Found,
    replacement: Option<&structural::Replacement>,
) -> Value {
    let text = &file.text;
    let (line, column, _) = structural::place(text, found.range.start);
    let (end_line, end_column, _) = structural::place(text, found.range.end);
    let line_text = text.lines().nth(line).unwrap_or_default();
    let captures = Value::list(
        found
            .captures
            .iter()
            .flat_map(|(name, range)| [Value::keyword(name), Value::from(&text[range.clone()])]),
    );
    let place = record([
        ("path", path_value(&file.path)),
        ("file", Value::from(file.file.as_str())),
        ("line", Value::Int(line as i64 + 1)),
        ("col", Value::Int(column as i64 + 1)),
        ("end-line", Value::Int(end_line as i64 + 1)),
        ("end-col", Value::Int(end_column as i64 + 1)),
        (
            "text",
            Value::from(line_text.trim().chars().take(200).collect::<String>()),
        ),
        ("match", Value::from(&text[found.range.clone()])),
        ("captures", captures),
    ]);
    match replacement {
        None => place,
        Some(replacement) => {
            let made = match found.loses_comments {
                true => Value::Nil,
                false => Value::from(replacement.fill(found, text)),
            };
            let mut fields = place.to_vec().unwrap_or_default();
            fields.extend([Value::keyword("replacement"), made]);
            Value::list(fields)
        }
    }
}

/// A regular expression a program gave; one that is none is its mistake.
fn compiled(name: &str, value: &Value) -> Result<regex::Regex, Error> {
    regex::Regex::new(value.as_str()?).map_err(|error| failed(format!("{name}: {error}")))
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
    remove(path)
}

/// Do away with `path`, and all in it if it is a directory; a link goes,
/// and not what it points to.
fn remove(path: &Path) -> std::io::Result<()> {
    match path.symlink_metadata()?.is_dir() {
        true => std::fs::remove_dir_all(path),
        false => std::fs::remove_file(path),
    }
}

/// What the functions that change the disk give back: nil, or what went
/// wrong.
fn said(done: Result<(), String>) -> Value {
    match done {
        Ok(()) => Value::Nil,
        Err(problem) => Value::from(problem),
    }
}

fn info_value(path: &Path, meta: &std::fs::Metadata) -> Value {
    let kind = meta.file_type();
    let kind = match () {
        _ if kind.is_symlink() => "link",
        _ if kind.is_dir() => "dir",
        _ if kind.is_file() => "file",
        _ => "other",
    };
    let modified =
        meta.modified()
            .ok()
            .map(|time| match time.duration_since(std::time::UNIX_EPOCH) {
                Ok(since) => since.as_secs() as i64,
                Err(before) => -(before.duration().as_secs() as i64),
            });
    let target = std::fs::read_link(path).ok();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let unix = unix_info(meta);
    let int = |value: Option<u64>| value.map_or(Value::Nil, |value| Value::Int(value as i64));
    record([
        ("name", Value::from(name.as_ref())),
        ("path", path_value(path)),
        ("kind", Value::symbol(kind)),
        ("dir", Value::Bool(path.is_dir())),
        ("size", Value::Int(meta.len() as i64)),
        ("modified", modified.map_or(Value::Nil, Value::Int)),
        ("mode", int(unix.map(|unix| unix.mode))),
        ("links", int(unix.map(|unix| unix.links))),
        ("uid", int(unix.map(|unix| unix.uid))),
        ("gid", int(unix.map(|unix| unix.gid))),
        ("target", target.as_deref().map_or(Value::Nil, path_value)),
    ])
}

/// What only a unix system tells of a file.
#[derive(Clone, Copy)]
struct UnixInfo {
    mode: u64,
    links: u64,
    uid: u64,
    gid: u64,
}

#[cfg(unix)]
fn unix_info(meta: &std::fs::Metadata) -> Option<UnixInfo> {
    use std::os::unix::fs::MetadataExt;
    Some(UnixInfo {
        mode: u64::from(meta.mode() & 0o7777),
        links: meta.nlink(),
        uid: u64::from(meta.uid()),
        gid: u64::from(meta.gid()),
    })
}

#[cfg(not(unix))]
fn unix_info(_: &std::fs::Metadata) -> Option<UnixInfo> {
    None
}

/// Copy `from` to `to`, unless something is there already: a directory
/// with all in it, links as links, each file with the time it was last
/// changed.
fn copy(from: &Path, to: &Path) -> Result<(), String> {
    let meta = from
        .symlink_metadata()
        .map_err(|error| format!("{}: {error}", from.display()))?;
    if to.symlink_metadata().is_ok() {
        return Err(format!(
            "{}: there is something there already",
            to.display()
        ));
    }
    if meta.is_dir()
        && let Ok(source) = from.canonicalize()
        && canonical(to).starts_with(&source)
    {
        return Err(format!(
            "{} → {}: a directory cannot be copied into itself",
            from.display(),
            to.display()
        ));
    }
    copy_entry(from, to, &meta)
}

/// What went wrong with a path, as the functions that change the disk say it.
fn wrong_at(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |error| format!("{}: {error}", path.display())
}

fn copy_entry(from: &Path, to: &Path, meta: &std::fs::Metadata) -> Result<(), String> {
    let wrong = wrong_at;
    let kind = meta.file_type();
    if kind.is_symlink() {
        let target = std::fs::read_link(from).map_err(wrong(from))?;
        return symlink(&target, to).map_err(wrong(to));
    }
    if kind.is_dir() {
        std::fs::create_dir(to).map_err(wrong(to))?;
        let entries = std::fs::read_dir(from).map_err(wrong(from))?;
        for entry in entries {
            let entry = entry.map_err(wrong(from))?;
            let meta = entry
                .path()
                .symlink_metadata()
                .map_err(wrong(&entry.path()))?;
            copy_entry(&entry.path(), &to.join(entry.file_name()), &meta)?;
        }
        // After what is in it, which a directory without the permission
        // to write in it would keep out.
        std::fs::set_permissions(to, meta.permissions()).map_err(wrong(to))?;
        keep_time(to, meta);
        return Ok(());
    }
    if kind.is_file() {
        std::fs::copy(from, to).map_err(wrong(from))?;
        keep_time(to, meta);
        return Ok(());
    }
    Err(format!(
        "{}: only files, directories and links are copied",
        from.display()
    ))
}

/// Give the copy the time its original was last changed, if the system
/// lets it: the copy is no less a copy without.
fn keep_time(path: &Path, meta: &std::fs::Metadata) {
    if let Ok(modified) = meta.modified()
        && let Ok(file) = std::fs::File::open(path)
    {
        let _ = file.set_modified(modified);
    }
}

#[cfg(unix)]
fn symlink(target: &Path, path: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

#[cfg(not(unix))]
fn symlink(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("links are made only on unix"))
}

/// The permissions `set-file-mode` was given: a number, or octal digits.
fn file_mode(value: &Value) -> Result<u32, Error> {
    let mode = match value {
        Value::Int(mode) => u32::try_from(*mode).ok(),
        Value::Str(digits) => u32::from_str_radix(digits, 8).ok(),
        _ => None,
    };
    mode.filter(|mode| *mode <= 0o7777)
        .ok_or_else(|| wrong("set-file-mode", "permissions, as 493 or \"755\"", value))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> std::io::Result<()> {
    Err(std::io::Error::other("permissions are set only on unix"))
}

// ---- programs ----------------------------------------------------------

/// How a program `spawn`ed ended, still as it left the thread that ran it.
struct Ran {
    status: Option<i32>,
    out: String,
    err: String,
}

impl Ran {
    fn into_value(self) -> Value {
        record([
            (
                "status",
                self.status
                    .map_or(Value::Nil, |code| Value::Int(code.into())),
            ),
            ("out", Value::from(self.out)),
            ("err", Value::from(self.err)),
        ])
    }
}

fn run_program(
    env: &ProjectEnv,
    program: &str,
    arguments: &[String],
    dir: &Path,
) -> Result<Ran, String> {
    let output = env
        .command(program)
        .args(arguments)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("{program}: {error}"))?;
    Ok(Ran {
        status: output.status.code(),
        out: String::from_utf8_lossy(&output.stdout).into_owned(),
        err: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
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
