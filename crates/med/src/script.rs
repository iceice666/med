//! The editor's end of its scripting language, `med-lisp`.
//!
//! There is one interpreter for the application. It runs the user's
//! `init.lisp` at startup and whatever is given to `:eval`, and programs
//! change the editor through the functions defined here:
//!
//! ```lisp
//! (bind 'normal "<Space>n" "workspace.next_tab")
//! (bind '(normal visual) "gh" "editor.hover")
//! (bind 'tree "x" "tree.close")        ; a panel's keys
//! (bind-keys 'normal "Y" "y$")         ; keys typed in place of others
//! (defkeys 'listing)                   ; a table for buffers to be in
//! (bind 'listing "<CR>" "user.open")   ; in normal mode there; also
//!                                      ; listing.visual, .operator, .insert
//! (set-buffer-keys '(listing))         ; the tab up is in it
//! (unbind 'normal "gt")
//! (set-option 'font-size 16)           ; also font-family, lsp, key-timeout,
//!                                      ; auto-pairs, format-on-save,
//!                                      ; project-env
//! (message "ready")                    ; shown in the status bar
//!
//! (defcommand "user.save-and-close" "Write the file, then close its tab"
//!   (fn ()
//!     (run "workspace.save")
//!     (run "workspace.close_tab")))
//! (run "workspace.edit" "notes.md")    ; a command, with its argument
//! (current-file)  (project-root)
//! (open-file "src/main.rs" 12 1)       ; in a tab, at a line and column
//! (open-scratch "notes" "text" save)   ; a tab of text that is no file
//! (scratch-name)                       ; which, if it is the one up
//! (panes) (with-pane 1 thunk)         ; the panes, and working in another
//! (modified?)                          ; has the tab up unsaved changes
//! (focus-editor)
//! (load "tree.lisp")                   ; another file, from beside this one
//! (add-hook 'files-changed refresh)    ; be told of something: HOOKS
//! (bindings 'normal "<Space>")         ; the keys that can follow others
//! (def timer (after 300 show))         ; call a function in 300 ms
//! (cancel timer)                       ; or not after all
//! (cursor) (insert "x") (decorate ...) ; the file being edited: buffer_api.rs
//! (defpanel 'notes :at 'right ...)     ; a panel: user_panel.rs, panel.rs
//! (list-dir path) (git 'status ...)    ; the project's files: host.rs
//! ```
//!
//! A mode, a panel or an option is named by a symbol, a keyword or a
//! string.
//!
//! A program that a workspace starts (`:eval`, a command of the user's) is
//! not run there and then but once the workspace is done with the key that
//! asked for it. By then no view is in the middle of an update, so the
//! program can run commands and read from views as it goes: `run` has
//! taken effect when it returns.
//!
//! What takes time (walking the project, a search, git) is done off the
//! main thread: the function that asks for it gives a function to call
//! with the result, and returns at once.

use crate::commands::{Args, Commands, Handler, RunCommand};
use crate::host::record;
use crate::keymap::PanelKeymaps;
use crate::user_panel::UserPanels;
use crate::workspace::Workspace;
use crate::{Settings, VimGlobal};
use gpui::{
    Action, AnyWindowHandle, App, AppContext as _, Context, Global, KeyBinding,
    KeyBindingContextPredicate, NoAction, SharedString, Task, WeakEntity, Window,
};
use med_lisp::{Arity, Call, Error, ErrorKind, Interp, Value};
use med_vim::{Awaiting, Bound, MapMode, NextKey, PendingKeys, format_keys, parse_keys};
use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// A program that never ends must not take the window with it: it is
/// stopped after this many steps, a fraction of a second.
const STEP_LIMIT: u64 = 10_000_000;

/// A layout is run every time its panel is drawn, and gets far less.
const DRAW_STEP_LIMIT: u64 = 200_000;

/// What a program can ask to be told of, with `add-hook`:
/// - `opened`: a window is up, and there is a project to look at.
/// - `files-changed`: files may have changed on disk: one was saved, the
///   window came to the front, or `workspace.sync` was run.
/// - `locations`: there is a list of places to choose from (the
///   references to a name); the function is called with the list, each
///   place a `(:path :file :line :col :text)`.
/// - `keys-pending`: the keys typed so far started a command, went on
///   with one, or are done with it. The function is called with what the
///   command waits for, `(:tables :keys :typed :awaiting)` as
///   [`pending_value`] makes it, or with nil once nothing is half typed.
/// - `buffer-entered`: another file is the one being edited; the
///   function is called with its path, or nil if it has no name yet.
/// - `buffer-changed`: the text of the file being edited changed.
/// - `cursor-moved`: the cursor is somewhere else.
/// - `mode-changed`: the editor went into another mode; the function is
///   called with it, as `(mode)` gives it.
/// - `diagnostics-changed`: a language server has something new to say
///   about a file; the function is called with the file's path.
const HOOKS: &[&str] = &[
    "opened",
    "files-changed",
    "locations",
    KEYS_PENDING,
    "buffer-entered",
    "buffer-changed",
    "cursor-moved",
    "mode-changed",
    "diagnostics-changed",
    "terminal-exited",
];

const KEYS_PENDING: &str = "keys-pending";

/// The key table of the shortcuts the window system handles (`cmd-s`).
pub(crate) const APP_TABLE: &str = "app";

pub(crate) const MODES: &[(&str, MapMode)] = &[
    ("normal", MapMode::Normal),
    ("visual", MapMode::Visual),
    ("operator", MapMode::Operator),
    ("insert", MapMode::Insert),
];

pub struct Script {
    /// `None` while a program runs: the program has the interpreter, and
    /// the application as its host.
    interp: Option<Interp>,
    /// What programs said with `message`, and what went wrong with the
    /// init file, until someone shows it.
    messages: Vec<String>,
    /// The workspace the running program was started from, if any.
    origin: Option<Origin>,
    /// The running program is describing a panel that is being drawn: it
    /// may compute, but not change the editor.
    drawing: bool,
    /// The functions to call when something happens, by what happens.
    hooks: HashMap<&'static str, Vec<Value>>,
    /// What `after` is waiting to call, by the number it gave `cancel`.
    timers: HashMap<i64, Task<()>>,
    /// The number the last timer got.
    last_timer: i64,
    /// The files being loaded, the innermost last: `load` looks beside it.
    loading: Vec<PathBuf>,
    /// The init file that was run, to run again.
    init_file: Option<PathBuf>,
    /// The shortcuts programs have bound in the `app` table, and the
    /// command each runs.
    app_keys: HashMap<String, String>,
    /// Set to stop the search that is under way: a new one takes over.
    pub(crate) search: Arc<AtomicBool>,
}

impl Global for Script {}

/// What `run` runs commands in, and the views a program reads from.
#[derive(Clone)]
pub(crate) struct Origin {
    window: AnyWindowHandle,
    workspace: WeakEntity<Workspace>,
}

/// How the command being run was invoked, as a program sees it: the rest
/// of the command line, the count typed before the key (or nil), and
/// whether there was a `!`.
const INVOCATION: [&str; 3] = ["command-arg", "command-count", "command-bang"];

fn invocation(args: &Args) -> [Value; 3] {
    [
        Value::from(args.arg.as_str()),
        args.count
            .map_or(Value::Nil, |count| Value::Int(count as i64)),
        Value::Bool(args.bang),
    ]
}

pub fn init(cx: &mut App) {
    let mut interp = Interp::new();
    interp.set_step_limit(Some(STEP_LIMIT));
    define_functions(&mut interp);
    crate::panel::define_functions(&mut interp);
    crate::user_panel::define_functions(&mut interp);
    crate::host::define_functions(&mut interp);
    crate::buffer_api::define_functions(&mut interp);
    for (name, value) in INVOCATION.iter().zip(invocation(&Args::default())) {
        interp.define(name, value);
    }
    cx.set_global(Script {
        interp: Some(interp),
        messages: Vec::new(),
        origin: None,
        drawing: false,
        hooks: HashMap::new(),
        timers: HashMap::new(),
        last_timer: 0,
        loading: Vec::new(),
        init_file: None,
        app_keys: HashMap::new(),
        search: Arc::default(),
    });
}

/// Forget the panels and hooks that programs defined, and what they were
/// waiting to do, as before the init file is run again.
fn reset_panels(cx: &mut App) {
    crate::user_panel::reset(cx);
    let script = cx.global_mut::<Script>();
    script.hooks.clear();
    script.timers.clear();
}

/// The shortcuts of the `app` table that run `command`, sorted.
pub(crate) fn app_keys_for(command: &str, cx: &App) -> Vec<String> {
    let bound = &cx.global::<Script>().app_keys;
    let mut keys: Vec<String> = bound
        .iter()
        .filter(|(_, to)| *to == command)
        .map(|(keys, _)| keys.clone())
        .collect();
    keys.sort();
    keys
}

/// Call a layout's function while its panel is being drawn.
pub(crate) fn call_to_draw(function: &Value, args: &[Value], cx: &mut App) -> Result<Value, Error> {
    with_interp(None, cx, |interp, cx| {
        cx.global_mut::<Script>().drawing = true;
        interp.set_step_limit(Some(DRAW_STEP_LIMIT));
        let result = interp.call(function, args, cx);
        interp.set_step_limit(Some(STEP_LIMIT));
        cx.global_mut::<Script>().drawing = false;
        result
    })
}

/// Lend the interpreter to `program`, with the application as its host.
fn with_interp<T>(
    origin: Option<Origin>,
    cx: &mut App,
    program: impl FnOnce(&mut Interp, &mut App) -> Result<T, Error>,
) -> Result<T, Error> {
    let script = cx.global_mut::<Script>();
    let Some(mut interp) = script.interp.take() else {
        return Err(failed("a program is already running"));
    };
    script.origin = origin;
    let result = program(&mut interp, cx);
    let script = cx.global_mut::<Script>();
    script.interp = Some(interp);
    script.origin = None;
    result
}

/// Evaluate `source` now, giving the value of its last form. There is no
/// workspace behind it, so nothing for `run` to run in: this is how the
/// init file is run.
pub fn eval(source: &str, cx: &mut App) -> Result<Value, Error> {
    with_interp(None, cx, |interp, cx| interp.eval_str(source, cx))
}

/// Run `program` now, for the workspace `origin` is, if it is one. What
/// the program says, or else what it comes to, goes to the workspace's
/// status bar; with no workspace it is kept until one asks.
fn finish(
    origin: Option<Origin>,
    cx: &mut App,
    program: impl FnOnce(&mut Interp, &mut App) -> Result<Option<String>, Error>,
) {
    let result = with_interp(origin.clone(), cx, program);
    let message = match result {
        Ok(value) => take_message(cx).or(value),
        Err(error) => Some(error.to_string()),
    };
    if let Some(message) = message {
        let shown = origin.is_some_and(|origin| {
            let show = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
                workspace.set_message(message.clone(), cx)
            };
            origin.workspace.update(cx, show).is_ok()
        });
        if !shown {
            cx.global_mut::<Script>().messages.push(message);
        }
    }
    // Whatever the program changed, panels may show it.
    cx.refresh_windows();
}

/// Run `program` for a workspace, once the workspace is done with what it
/// is doing now.
fn start(
    window: &Window,
    cx: &mut Context<Workspace>,
    program: impl FnOnce(&mut Interp, &mut App) -> Result<Option<String>, Error> + 'static,
) {
    let origin = Origin {
        window: window.window_handle(),
        workspace: cx.weak_entity(),
    };
    cx.defer(move |cx| finish(Some(origin), cx, program));
}

/// Do `work` off the main thread for the running program, then call its
/// `callback` with what `values` makes of the result. The callback runs
/// for the workspace the program was started from.
pub(crate) fn in_background<T: Send + 'static>(
    cx: &mut App,
    callback: Value,
    work: impl FnOnce() -> T + Send + 'static,
    values: impl FnOnce(T) -> Vec<Value> + 'static,
) {
    let origin = cx.global::<Script>().origin.clone();
    let work = cx.background_spawn(async move { work() });
    cx.spawn(async move |cx| {
        let result = work.await;
        cx.update(|cx| call_back(origin, callback, values(result), cx));
    })
    .detach();
}

/// [`in_background`] on a thread of its own, for work that may wait a
/// long time (for the project's environment, say) without holding up one
/// of the executor's threads.
pub(crate) fn on_thread<T: Send + 'static>(
    cx: &mut App,
    callback: Value,
    work: impl FnOnce() -> T + Send + 'static,
    values: impl FnOnce(T) -> Vec<Value> + 'static,
) {
    let origin = cx.global::<Script>().origin.clone();
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    cx.spawn(async move |cx| {
        if let Ok(result) = rx.await {
            cx.update(|cx| call_back(origin, callback, values(result), cx));
        }
    })
    .detach();
}

/// Call `callback`, a function a program left to be called later.
pub(crate) fn call_back(origin: Option<Origin>, callback: Value, args: Vec<Value>, cx: &mut App) {
    finish(origin, cx, move |interp, cx| {
        interp.call(&callback, &args, cx)?;
        Ok(None)
    });
}

/// The workspace the running program was started from.
pub(crate) fn origin(cx: &App) -> Option<Origin> {
    cx.global::<Script>().origin.clone()
}

/// Do something in the workspace the running program was started from.
/// `None` if there is none: the program is the init file, or the window
/// is gone.
pub(crate) fn in_workspace<T>(
    cx: &mut App,
    update: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> T,
) -> Option<T> {
    let origin = origin(cx)?;
    origin
        .window
        .update(cx, |_, window, cx| {
            origin
                .workspace
                .update(cx, |workspace, cx| update(workspace, window, cx))
                .ok()
        })
        .ok()
        .flatten()
}

/// The directory of the workspace the running program was started from.
pub(crate) fn project_root(cx: &App) -> Option<PathBuf> {
    let read = |workspace: &Workspace, _: &App| workspace.root().to_path_buf();
    origin(cx)?.workspace.read_with(cx, read).ok()
}

/// The environment of the project a program is running for.
pub(crate) fn project_env(cx: &App) -> Option<std::sync::Arc<crate::env::EnvCell>> {
    let read = |workspace: &Workspace, _: &App| workspace.env();
    origin(cx)?.workspace.read_with(cx, read).ok()
}

/// Evaluate `source` for a workspace: `:eval`.
pub fn eval_from(source: String, window: &Window, cx: &mut Context<Workspace>) {
    start(window, cx, move |interp, cx| {
        let value = interp.eval_str(&source, cx)?;
        Ok(Some(value.to_string()))
    });
}

/// Run a command of the user's for a workspace.
pub fn run_command(function: Value, args: Args, window: &Window, cx: &mut Context<Workspace>) {
    start(window, cx, move |interp, cx| {
        invoke(interp, cx, &function, &[], &args)?;
        Ok(None)
    });
}

/// Give a program the text of its tab to write, `:w` having been typed
/// there: the function is called with the text, as a command is.
pub fn write_scratch(
    function: Value,
    text: String,
    args: Args,
    window: &Window,
    cx: &mut Context<Workspace>,
) {
    start(window, cx, move |interp, cx| {
        invoke(interp, cx, &function, &[Value::from(text)], &args)?;
        Ok(None)
    });
}

/// Call a function a panel's layout gave a click or an input, for a
/// workspace.
pub fn call_from(function: Value, args: Vec<Value>, window: &Window, cx: &mut Context<Workspace>) {
    start(window, cx, move |interp, cx| {
        interp.call(&function, &args, cx)?;
        Ok(None)
    });
}

/// Tell the programs that asked that `hook` happened. False if none did.
pub fn run_hook(
    hook: &str,
    args: Vec<Value>,
    window: &Window,
    cx: &mut Context<Workspace>,
) -> bool {
    let functions = match cx.global::<Script>().hooks.get(hook) {
        Some(functions) if !functions.is_empty() => functions.clone(),
        _ => return false,
    };
    start(window, cx, move |interp, cx| {
        for function in &functions {
            interp.call(function, &args, cx)?;
        }
        Ok(None)
    });
    true
}

/// What the `keys-pending` hook is called with while a command is half
/// typed:
/// - `:tables`: the key tables the next key is looked up in, the first to
///   know it deciding: for an editor the buffer's tables and its mode
///   last, for a panel its contexts.
/// - `:keys`: the start of a binding typed so far, as `bindings` takes
///   it. Empty after a count or an operator.
/// - `:typed`: every key of the command so far (`"a2d`).
/// - `:awaiting`: `'register`, `'char`, `'text-object`, `'mark` or
///   `'flash` when the next key is not looked up but taken as it is (after
///   `"`, `f`, `di`, `m`, `s`).
pub(crate) fn pending_value(
    tables: &[&str],
    keys: &str,
    typed: &str,
    awaiting: Option<&str>,
) -> Value {
    record([
        (
            "tables",
            Value::list(tables.iter().copied().map(Value::from)),
        ),
        ("keys", Value::from(keys)),
        ("typed", Value::from(typed)),
        ("awaiting", awaiting.map_or(Value::Nil, Value::symbol)),
    ])
}

/// [`pending_value`] of an editor's half-typed command.
pub(crate) fn editor_pending(pending: &PendingKeys) -> Value {
    let mode = MODES.iter().find(|(_, mode)| *mode == pending.mode);
    let awaiting = pending.awaiting.map(|awaiting| match awaiting {
        Awaiting::Register => "register",
        Awaiting::Char => "char",
        Awaiting::TextObject => "text-object",
        Awaiting::Mark => "mark",
        Awaiting::Flash => "flash",
    });
    let mode = mode.map_or(MapMode::Normal, |(_, mode)| *mode);
    let mut tables: Vec<String> = pending
        .tables
        .iter()
        .map(|table| buffer_table_name(table, mode))
        .collect();
    tables.push(mode_name(mode).to_string());
    let tables: Vec<&str> = tables.iter().map(String::as_str).collect();
    pending_value(&tables, &pending.keys, &pending.typed, awaiting)
}

/// Tell the programs that asked what the keys typed so far wait for:
/// `pending` is a [`pending_value`], or nil when they wait for nothing.
pub fn keys_pending(pending: Value, window: &Window, cx: &mut Context<Workspace>) {
    run_hook(KEYS_PENDING, vec![pending], window, cx);
}

/// Call a command's function with `values`. While it runs, the names in
/// [`INVOCATION`] say how it was invoked; a command that runs another gets
/// them back afterwards.
fn invoke(
    interp: &mut Interp,
    host: &mut dyn Any,
    function: &Value,
    values: &[Value],
    args: &Args,
) -> Result<Value, Error> {
    let before = INVOCATION.map(|name| interp.global(name).cloned().unwrap_or(Value::Nil));
    for (name, value) in INVOCATION.iter().zip(invocation(args)) {
        interp.define(name, value);
    }
    let result = interp.call(function, values, host);
    for (name, value) in INVOCATION.iter().zip(before) {
        interp.define(name, value);
    }
    result
}

/// Run the file at `path`. What goes wrong is left as a message; what the
/// file did before that stays done.
pub fn load_file(path: &Path, cx: &mut App) {
    let result = match std::fs::read_to_string(path) {
        Ok(source) => {
            cx.global_mut::<Script>().loading.push(path.to_path_buf());
            let result = eval(&source, cx);
            cx.global_mut::<Script>().loading.pop();
            result.map_err(|error| in_file(path, &error))
        }
        Err(error) => Err(format!("{}: {error}", path.display())),
    };
    if let Err(problem) = result {
        cx.global_mut::<Script>().messages.push(problem);
    }
}

/// What went wrong in the file at `path`, with where.
fn in_file(path: &Path, error: &Error) -> String {
    match error.span {
        // path:line:column: the form editors and terminals jump to.
        Some(_) => format!("{}:{error}", path.display()),
        None => format!("{}: {error}", path.display()),
    }
}

/// Run the init file at `path`, if there is one. The panels and hooks it
/// defined last time are forgotten first, and its timers called off, in
/// case it no longer defines them; the keys and options it set are not
/// undone.
pub fn run_init(path: &Path, cx: &mut App) -> bool {
    reset_panels(cx);
    if !path.is_file() {
        return false;
    }
    cx.global_mut::<Script>().init_file = Some(path.to_path_buf());
    load_file(path, cx);
    true
}

/// Run `init.lisp` from the configuration directory, and say which file
/// that was, if there was one.
pub fn load_init(cx: &mut App) -> Option<PathBuf> {
    let path = crate::config_dir()?.join("init.lisp");
    run_init(&path, cx).then_some(path)
}

/// Run the init file again: the one that was run before, or the one that
/// has since been written in the configuration directory.
pub fn reload_init(cx: &mut App) -> Option<PathBuf> {
    match cx.global::<Script>().init_file.clone() {
        Some(path) => run_init(&path, cx).then_some(path),
        None => load_init(cx),
    }
}

/// The last thing programs had to say since this was last asked.
pub fn take_message(cx: &mut App) -> Option<String> {
    cx.global_mut::<Script>().messages.drain(..).next_back()
}

/// The terminal a program names by its number, as `function` was given it.
fn terminal_arg(
    call: &mut Call<'_>,
    id: &Value,
    function: &str,
) -> Result<gpui::Entity<crate::terminal::TermView>, Error> {
    let id = id.as_int()?;
    let cx = app(call)?;
    let found = origin(cx).and_then(|origin| {
        let find = |workspace: &Workspace, _: &App| {
            u64::try_from(id)
                .ok()
                .and_then(|id| workspace.terminal_by_id(id))
        };
        origin.workspace.read_with(cx, find).ok().flatten()
    });
    found.ok_or_else(|| failed(format!("{function}: there is no terminal {id}")))
}

fn failed(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Raised, message)
}

/// A mode, panel or option as a program writes it: `'name`, `:name` or
/// `"name"`.
fn name(value: &Value) -> Result<String, Error> {
    match value {
        Value::Symbol(name) | Value::Keyword(name) => Ok(name.name().to_string()),
        Value::Str(name) => Ok(name.to_string()),
        other => Err(Error::new(
            ErrorKind::Type,
            format!("expected a name, got {other}"),
        )),
    }
}

/// Keys are bound in an editor mode, in a table of buffers' own for a
/// mode, in a panel, or for the whole window.
enum Table {
    Mode(MapMode),
    Buffer(String, MapMode),
    Panel(String),
    App,
}

fn mode_name(mode: MapMode) -> &'static str {
    MODES
        .iter()
        .find(|(_, other)| *other == mode)
        .map_or("normal", |(name, _)| name)
}

/// What a program calls a buffers' table in a mode: `listing` in normal
/// mode, `listing.insert` in insert mode.
fn buffer_table_name(table: &str, mode: MapMode) -> String {
    match mode {
        MapMode::Normal => table.to_string(),
        other => format!("{table}.{}", mode_name(other)),
    }
}

/// The buffers' table `name` is, and its mode.
fn buffer_table(name: &str, cx: &App) -> Option<(String, MapMode)> {
    let keymap = &cx.global::<VimGlobal>().0.keymap;
    if keymap.has_table(name) {
        return Some((name.to_string(), MapMode::Normal));
    }
    let (table, mode) = name.rsplit_once('.')?;
    let (_, mode) = MODES.iter().find(|(other, _)| *other == mode)?;
    keymap.has_table(table).then(|| (table.to_string(), *mode))
}

/// The table `value` names.
fn table(value: &Value, cx: &App) -> Result<Table, Error> {
    let name = name(value)?;
    let panels = cx.global::<UserPanels>();
    if let Some((_, mode)) = MODES.iter().find(|(mode, _)| *mode == name) {
        Ok(Table::Mode(*mode))
    } else if name == APP_TABLE {
        Ok(Table::App)
    } else if let Some((table, mode)) = buffer_table(&name, cx) {
        Ok(Table::Buffer(table, mode))
    } else if panels.has_context(&name) {
        Ok(Table::Panel(name))
    } else {
        let modes: Vec<&str> = MODES.iter().map(|(mode, _)| *mode).collect();
        let panels: Vec<&str> = panels.names().collect();
        let mut buffers: Vec<&str> = cx.global::<VimGlobal>().0.keymap.table_names().collect();
        buffers.sort();
        let buffers = match buffers.is_empty() {
            true => String::new(),
            false => format!(" a table of defkeys ({}),", buffers.join(" ")),
        };
        Err(failed(format!(
            "{name} is neither a mode ({}), {APP_TABLE},{buffers} nor a panel ({})",
            modes.join(" "),
            panels.join(" ")
        )))
    }
}

/// The tables `value` names: one name, or a list of them.
fn tables(value: &Value, cx: &App) -> Result<Vec<Table>, Error> {
    let names = match value {
        Value::Pair(_) => value.to_vec()?,
        one => vec![one.clone()],
    };
    names.iter().map(|name| table(name, cx)).collect()
}

/// A time a program gives in milliseconds.
fn milliseconds(value: &Value) -> Result<Duration, Error> {
    Duration::try_from_secs_f64(value.as_float()? / 1000.)
        .map_err(|_| failed(format!("{value} is no time to wait, in milliseconds")))
}

/// An option that is on or off.
fn flag(value: &Value) -> Result<bool, Error> {
    match value {
        Value::Bool(on) => Ok(*on),
        Value::Nil => Ok(false),
        other => Err(Error::new(
            ErrorKind::Type,
            format!("expected true or false, got {other}"),
        )),
    }
}

/// Keys as a program writes them, in vim notation.
fn keys(value: &Value) -> Result<&str, Error> {
    match value.as_str()? {
        "" => Err(failed("no keys to bind")),
        keys => Ok(keys),
    }
}

/// A binding in the `app` table. Its keys are written as the window
/// system has them: `cmd-p`, `cmd-shift-f`.
fn app_binding(keys: &str, action: Box<dyn Action>, cx: &App) -> Result<KeyBinding, Error> {
    // Under the workspace, so in an editor and in every panel.
    let context = KeyBindingContextPredicate::parse("Workspace").ok();
    KeyBinding::load(keys, action, context.map(Rc::new), false, None, cx).map_err(|_| {
        failed(format!(
            "{keys:?} is no shortcut: in {APP_TABLE} they are written like cmd-shift-p"
        ))
    })
}

/// The application, for a function that changes it or reads from it.
pub(crate) fn app<'a>(call: &'a mut Call<'_>) -> Result<&'a mut App, Error> {
    let cx = call.host::<App>()?;
    if cx.global::<Script>().drawing {
        return Err(failed(
            "a layout can only describe its panel, not do anything else",
        ));
    }
    Ok(cx)
}

fn define_functions(interp: &mut Interp) {
    // (bind table keys command)
    interp.define_native("bind", 3, |call, args| {
        let (keys, command) = (keys(&args[1])?, args[2].as_str()?);
        let cx = app(call)?;
        let known = cx.global::<Commands>().get(command).is_some()
            || med_vim::command_names().any(|name| name == command);
        if !known {
            return Err(failed(format!("there is no command {command}")));
        }
        for table in tables(&args[0], cx)? {
            match table {
                Table::Mode(mode) => {
                    let keymap = &mut cx.global_mut::<VimGlobal>().0.keymap;
                    keymap.bind(mode, keys, command);
                }
                Table::Buffer(table, mode) => {
                    let keymap = &mut cx.global_mut::<VimGlobal>().0.keymap;
                    keymap.bind_in(Some(&table), mode, keys, command);
                }
                Table::Panel(panel) => {
                    cx.global_mut::<PanelKeymaps>().bind(&panel, keys, command);
                }
                Table::App => {
                    let action = RunCommand(SharedString::from(command.to_string()));
                    let binding = app_binding(keys, Box::new(action), cx)?;
                    cx.bind_keys([binding]);
                    let bound = &mut cx.global_mut::<Script>().app_keys;
                    bound.insert(keys.to_string(), command.to_string());
                }
            }
        }
        Ok(Value::Nil)
    });

    // (bind-keys mode keys other-keys): vim's :map.
    interp.define_native("bind-keys", 3, |call, args| {
        let (keys, to) = (keys(&args[1])?, keys(&args[2])?);
        let cx = app(call)?;
        for table in tables(&args[0], cx)? {
            match table {
                Table::Mode(mode) => {
                    let keymap = &mut cx.global_mut::<VimGlobal>().0.keymap;
                    keymap.bind_keys(mode, keys, to);
                }
                Table::Buffer(table, mode) => {
                    let keymap = &mut cx.global_mut::<VimGlobal>().0.keymap;
                    keymap.bind_keys_in(Some(&table), mode, keys, to);
                }
                Table::Panel(panel) => {
                    return Err(failed(format!(
                        "keys stand for other keys only in an editor mode, not in {panel}"
                    )));
                }
                Table::App => {
                    return Err(failed(format!(
                        "keys stand for other keys only in an editor mode, not in {APP_TABLE}"
                    )));
                }
            }
        }
        Ok(Value::Nil)
    });

    // (defkeys name): a table of keys for buffers to be in, looked up
    // before the mode's: `name` in normal mode, `name.visual`,
    // `name.operator` and `name.insert` in the others. A buffer is put in
    // it with `set-buffer-keys`.
    interp.define_native("defkeys", 1, |call, args| {
        let given = name(&args[0])?;
        let cx = app(call)?;
        let taken = MODES.iter().any(|(mode, _)| *mode == given)
            || given == APP_TABLE
            || cx.global::<UserPanels>().has_context(&given);
        if given.is_empty() || given.contains('.') || given.contains(char::is_whitespace) {
            return Err(failed(format!("{given:?} is no name for a table of keys")));
        }
        if taken {
            return Err(failed(format!("{given} is a table of keys already")));
        }
        cx.global_mut::<VimGlobal>().0.keymap.define_table(&given);
        Ok(Value::from(given))
    });

    // (set-buffer-keys tables): the tab up looks keys up in these tables
    // of `defkeys`, the first first, before its mode's; nil for none.
    interp.define_native("set-buffer-keys", 1, |call, args| {
        let names = match &args[0] {
            Value::Nil => Vec::new(),
            Value::Pair(_) => args[0].to_vec()?,
            one => vec![one.clone()],
        };
        let names = names.iter().map(name).collect::<Result<Vec<_>, _>>()?;
        let cx = app(call)?;
        let keymap = &cx.global::<VimGlobal>().0.keymap;
        if let Some(unknown) = names.iter().find(|name| !keymap.has_table(name)) {
            return Err(failed(format!("{unknown} is no table of defkeys")));
        }
        let set = in_workspace(cx, |workspace, _, cx| {
            let editor = workspace.active_editor()?.clone();
            editor.update(cx, |editor, _| editor.vim.tables = names);
            Some(())
        });
        match set.flatten() {
            Some(()) => Ok(Value::Nil),
            None => Err(failed("set-buffer-keys: there is no tab up")),
        }
    });

    // (buffer-keys): the tables the tab up looks keys up in first.
    interp.define_native("buffer-keys", 0, |call, _| {
        let cx = app(call)?;
        let tables = origin(cx).and_then(|origin| {
            let read = |workspace: &Workspace, cx: &App| {
                Some(workspace.active_editor()?.read(cx).vim.tables.clone())
            };
            origin.workspace.read_with(cx, read).ok().flatten()
        });
        Ok(Value::list(
            tables.unwrap_or_default().into_iter().map(Value::from),
        ))
    });

    // (unbind table keys): whether there was anything to remove.
    interp.define_native("unbind", 2, |call, args| {
        let keys = keys(&args[1])?;
        let cx = app(call)?;
        let mut removed = false;
        for table in tables(&args[0], cx)? {
            removed |= match table {
                Table::Mode(mode) => cx.global_mut::<VimGlobal>().0.keymap.unbind(mode, keys),
                Table::Buffer(table, mode) => {
                    let keymap = &mut cx.global_mut::<VimGlobal>().0.keymap;
                    keymap.unbind_in(Some(&table), mode, keys)
                }
                Table::Panel(panel) => cx.global_mut::<PanelKeymaps>().unbind(&panel, keys),
                Table::App => {
                    let bound = cx.global_mut::<Script>().app_keys.remove(keys).is_some();
                    if bound {
                        let binding = app_binding(keys, Box::new(NoAction), cx)?;
                        cx.bind_keys([binding]);
                    }
                    bound
                }
            };
        }
        Ok(Value::Bool(removed))
    });

    // (bindings table [keys]): the keys that can come after `keys` in a
    // mode's table or a panel's, or with no keys the first key of every
    // binding, in the order of their notation. Each is `(:key :keys
    // :command :description :to :prefix)`: the key and the keys with it,
    // the command they are bound to with what it says of itself (nil for
    // the vim engine's own) or else the keys they stand for, and whether
    // longer bindings go on from there.
    interp.define_native("bindings", Arity::between(1, 2), |call, args| {
        let typed = match args.get(1) {
            None | Some(Value::Nil) => String::new(),
            Some(keys) => format_keys(&parse_keys(keys.as_str()?)),
        };
        let cx = app(call)?;
        let next: Vec<NextKey<Bound>> = match table(&args[0], cx)? {
            Table::Mode(mode) => cx.global::<VimGlobal>().0.keymap.next_keys(mode, &typed),
            Table::Buffer(table, mode) => {
                let keymap = &cx.global::<VimGlobal>().0.keymap;
                keymap.next_keys_in(Some(&table), mode, &typed)
            }
            Table::Panel(panel) => {
                let next = cx.global::<PanelKeymaps>().next_keys(&panel, &typed);
                let command = |command: &SharedString| Bound::Command(command.to_string());
                next.into_iter()
                    .map(|next| NextKey {
                        key: next.key,
                        value: next.value.map(command),
                        prefix: next.prefix,
                    })
                    .collect()
            }
            Table::App => {
                return Err(failed(format!(
                    "the shortcuts in {APP_TABLE} are the window system's, and cannot be listed"
                )));
            }
        };
        let commands = cx.global::<Commands>();
        let text = |text: Option<&str>| text.map_or(Value::Nil, Value::from);
        Ok(Value::list(next.iter().map(|next| {
            let key = format_keys(std::slice::from_ref(&next.key));
            let (command, to) = match &next.value {
                Some(Bound::Command(command)) => (Some(command.as_str()), None),
                Some(Bound::Keys(keys)) => (None, Some(format_keys(keys))),
                None => (None, None),
            };
            let known = command.and_then(|command| commands.get(command));
            record([
                ("key", Value::from(key.as_str())),
                ("keys", Value::from(format!("{typed}{key}"))),
                ("command", text(command)),
                (
                    "description",
                    text(known.map(|known| known.description.as_ref())),
                ),
                ("to", text(to.as_deref())),
                ("prefix", Value::Bool(next.prefix)),
            ])
        })))
    });

    // (after milliseconds function): call the function, with nothing,
    // once that long from now. What it gives is for `cancel`.
    interp.define_native("after", 2, |call, args| {
        let delay = milliseconds(&args[0])?;
        if !matches!(args[1], Value::Fn(_) | Value::Native(_)) {
            return Err(Error::new(
                ErrorKind::Type,
                format!("expected a function, got {}", args[1]),
            ));
        }
        let callback = args[1].clone();
        let cx = app(call)?;
        let origin = origin(cx);
        let script = cx.global_mut::<Script>();
        script.last_timer += 1;
        let id = script.last_timer;
        let timer = cx.background_executor().timer(delay);
        let task = cx.spawn(async move |cx| {
            timer.await;
            cx.update(|cx| {
                // No longer one to cancel.
                if let Some(task) = cx.global_mut::<Script>().timers.remove(&id) {
                    task.detach();
                }
                call_back(origin, callback, Vec::new(), cx);
            });
        });
        cx.global_mut::<Script>().timers.insert(id, task);
        Ok(Value::Int(id))
    });

    // (cancel timer): do not call what `after` was to call. Whether it
    // was still to be called; nil is a timer that never was.
    interp.define_native("cancel", 1, |call, args| {
        let id = match &args[0] {
            Value::Nil => return Ok(Value::Bool(false)),
            timer => timer.as_int()?,
        };
        let timers = &mut app(call)?.global_mut::<Script>().timers;
        Ok(Value::Bool(timers.remove(&id).is_some()))
    });

    // (set-option name value)
    interp.define_native("set-option", 2, |call, args| {
        let (option, value) = (name(&args[0])?, &args[1]);
        let cx = app(call)?;
        let settings = cx.global_mut::<Settings>();
        match option.as_str() {
            "font-size" => {
                let size = value.as_float()?;
                if size.is_nan() || size <= 0. {
                    return Err(failed(format!("{value} is no size for a font")));
                }
                settings.font_size = size as f32;
            }
            // nil goes back to the theme's font.
            "font-family" => {
                settings.font_family = match value {
                    Value::Nil => None,
                    family => Some(SharedString::from(family.as_str()?.to_string())),
                };
            }
            // Read when a window opens: of use in the init file.
            "lsp" => settings.lsp = flag(value)?,
            // How long a key that is a binding by itself waits to become
            // a longer one, in milliseconds; nil for as long as it takes.
            "key-timeout" => {
                settings.key_timeout = match value {
                    Value::Nil => None,
                    time => Some(milliseconds(time)?),
                };
            }
            "format-on-save" => settings.format_on_save = flag(value)?,
            // Read when a window opens, as `lsp` is.
            "project-env" => settings.project_env = flag(value)?,
            // Close brackets and quotes as they are typed.
            "auto-pairs" => {
                let on = flag(value)?;
                cx.global_mut::<VimGlobal>().0.auto_pairs = on;
            }
            other => {
                return Err(failed(format!(
                    "{other} is not an option \
                     (font-size font-family lsp key-timeout auto-pairs format-on-save project-env)"
                )));
            }
        }
        cx.refresh_windows();
        Ok(Value::Nil)
    });

    // (defcommand name description function): the function takes no
    // arguments. From then on the name can be bound to keys, typed on the
    // command line and given to `run`.
    interp.define_native("defcommand", 3, |call, args| {
        let (command, description) = (name(&args[0])?, args[1].as_str()?);
        if !matches!(args[2], Value::Fn(_) | Value::Native(_)) {
            return Err(Error::new(
                ErrorKind::Type,
                format!("expected a function, got {}", args[2]),
            ));
        }
        if command.is_empty() || command.ends_with('!') || command.contains(char::is_whitespace) {
            return Err(failed(format!("{command:?} is no name for a command")));
        }
        if description.is_empty() {
            return Err(failed(format!("{command} needs a description")));
        }
        let commands = app(call)?.global_mut::<Commands>();
        let built_in = med_vim::command_names().any(|name| name == command)
            || commands
                .get(&command)
                .is_some_and(|known| !matches!(known.handler, Handler::Script(_)));
        if built_in {
            return Err(failed(format!(
                "{command} is one of the editor's own commands"
            )));
        }
        // `:w` goes on meaning what it does.
        if let Some(other) = commands.resolve_ex(&command)
            && other != command
        {
            return Err(failed(format!(
                "{command} is what the command line calls {other}"
            )));
        }
        commands.register(
            command.clone(),
            description.to_string(),
            Handler::Script(args[2].clone()),
        );
        Ok(Value::from(command))
    });

    // (run command [argument]): the command by any name the command line
    // knows it by, a `!` included. It has run when this returns.
    interp.define_native("run", Arity::between(1, 2), |call, args| {
        let line = args[0].as_str()?;
        let (given, bang) = match line.strip_suffix('!') {
            Some(given) => (given, true),
            None => (line, false),
        };
        let invocation = Args {
            count: None,
            arg: args.get(1).map(Value::display).unwrap_or_default(),
            bang,
        };
        let cx = app(call)?;
        let commands = cx.global::<Commands>();
        let Some(command) = commands.resolve_ex(given) else {
            return Err(failed(format!("there is no command {given}")));
        };
        // Another command of the user's: the interpreter is right here.
        if let Some(Handler::Script(function)) = commands.handler(&command) {
            invoke(call.interp, call.host, &function, &[], &invocation)?;
            return Ok(Value::Nil);
        }
        if origin(cx).is_none() {
            return Err(failed(format!("there is no window to run {command} in")));
        }
        let ran = in_workspace(cx, |workspace, window, cx| {
            workspace.run_command(&command, &invocation, window, cx);
        });
        match ran {
            Some(()) => Ok(Value::Nil),
            None => Err(failed(format!("the window to run {command} in is gone"))),
        }
    });

    // (current-file): the path of the file being edited, or nil.
    interp.define_native("current-file", 0, |call, _| {
        let cx = app(call)?;
        let path = origin(cx).and_then(|origin| {
            let read = |workspace: &Workspace, cx: &App| {
                let editor = workspace.active_editor()?;
                editor.read(cx).path.clone()
            };
            origin.workspace.read_with(cx, read).ok().flatten()
        });
        Ok(path.map_or(Value::Nil, |path| Value::from(path.display().to_string())))
    });

    // (project-root): the directory the window was opened on, or nil.
    interp.define_native("project-root", 0, |call, _| {
        let root = project_root(app(call)?);
        Ok(root.map_or(Value::Nil, |root| Value::from(root.display().to_string())))
    });

    // (open-file path [line [column]]): open the file in a tab, or go to
    // the tab that has it, and to the line and column, counted from 1.
    // Whether there was a file to open.
    interp.define_native("open-file", Arity::between(1, 3), |call, args| {
        let path = PathBuf::from(args[0].as_str()?);
        let place = |index: usize| match args.get(index) {
            None | Some(Value::Nil) => Ok(None),
            Some(Value::Int(place)) if *place >= 1 => Ok(Some(*place as usize - 1)),
            Some(other) => Err(failed(format!(
                "open-file: expected a line or column from 1, got {other}"
            ))),
        };
        let position = place(1)?.map(|line| Ok::<_, Error>((line, place(2)?.unwrap_or(0))));
        let position = position.transpose()?;
        let cx = app(call)?;
        let opened = in_workspace(cx, |workspace, window, cx| {
            workspace.open_at(&path, position, window, cx)
        });
        match opened {
            Some(opened) => Ok(Value::Bool(opened)),
            None => Err(failed("there is no window to open a file in")),
        }
    });

    // (open-scratch name [text [on-save]]): go to the tab of text called
    // `name`, which is no file, or make one. Given text, that is what the
    // tab has, as written; given a function, `:w` calls it with the text,
    // as a command, and until then the text is unsaved. Without one there is nothing to
    // lose, and nowhere to write it.
    interp.define_native("open-scratch", Arity::between(1, 3), |call, args| {
        let name = match args[0].as_str()? {
            "" => return Err(failed("open-scratch: a tab needs a name")),
            name => name.to_string(),
        };
        let text = match args.get(1) {
            None | Some(Value::Nil) => None,
            Some(text) => Some(text.as_str()?.to_string()),
        };
        let on_save = match args.get(2) {
            None | Some(Value::Nil) => None,
            Some(function @ (Value::Fn(_) | Value::Native(_))) => Some(function.clone()),
            Some(other) => {
                return Err(Error::new(
                    ErrorKind::Type,
                    format!("open-scratch: expected a function to write with, got {other}"),
                ));
            }
        };
        let cx = app(call)?;
        let opened = in_workspace(cx, |workspace, window, cx| {
            workspace.open_scratch(&name, text.as_deref(), on_save, window, cx)
        });
        match opened {
            Some(()) => Ok(Value::from(name)),
            None => Err(failed("there is no window to open a tab in")),
        }
    });

    // (scratch-name): the name of the tab of text being edited, if it is
    // one of `open-scratch`'s, or nil.
    interp.define_native("scratch-name", 0, |call, _| {
        let cx = app(call)?;
        let name = origin(cx).and_then(|origin| {
            let read = |workspace: &Workspace, cx: &App| {
                let editor = workspace.active_editor()?.read(cx);
                Some(editor.scratch.as_ref()?.name.clone())
            };
            origin.workspace.read_with(cx, read).ok().flatten()
        });
        Ok(name.map_or(Value::Nil, |name| Value::from(name.as_ref())))
    });

    // (panes): the panes, from the top left, each as what it shows:
    // (:focused :scratch :path :terminal), the focused one's :focused
    // true, the others' false; :scratch the name of its tab if
    // `open-scratch` made it, :path its file, :terminal the number of its
    // terminal, or nil.
    interp.define_native("panes", 0, |call, _| {
        let cx = app(call)?;
        let infos = origin(cx).and_then(|origin| {
            let read = |workspace: &Workspace, cx: &App| workspace.pane_infos(cx);
            origin.workspace.read_with(cx, read).ok()
        });
        let text = |text: Option<String>| text.map_or(Value::Nil, Value::from);
        Ok(Value::list(infos.unwrap_or_default().into_iter().map(
            |info| {
                crate::host::record([
                    ("focused", Value::Bool(info.focused)),
                    ("scratch", text(info.scratch.map(|name| name.to_string()))),
                    (
                        "path",
                        text(info.path.map(|path| path.display().to_string())),
                    ),
                    (
                        "terminal",
                        info.terminal
                            .map_or(Value::Nil, |id| Value::from(id as i64)),
                    ),
                ])
            },
        )))
    });

    // (scratch-tabs): every tab of text `open-scratch` made, in every
    // pane from the top left, shown or not: (:name :modified :text).
    interp.define_native("scratch-tabs", 0, |call, _| {
        let cx = app(call)?;
        let tabs = origin(cx).and_then(|origin| {
            let read = |workspace: &Workspace, cx: &App| workspace.scratch_tabs(cx);
            origin.workspace.read_with(cx, read).ok()
        });
        Ok(Value::list(tabs.unwrap_or_default().into_iter().map(
            |(name, modified, text)| {
                crate::host::record([
                    ("name", Value::from(name.as_ref())),
                    ("modified", Value::Bool(modified)),
                    ("text", Value::from(text)),
                ])
            },
        )))
    });

    // (with-pane n function): call the function with the nth pane of
    // `panes`, from 0, as the one being edited, then go back to the one
    // that was, which keeps the keyboard. What the function does to the
    // tab being edited is done in that pane.
    interp.define_native("with-pane", 2, |call, args| {
        let nth = args[0].as_int()?;
        let function = args[1].clone();
        let entered = usize::try_from(nth).ok().and_then(|nth| {
            let cx = app(call).ok()?;
            in_workspace(cx, |workspace, window, cx| {
                workspace.enter_pane(nth, window, cx)
            })
            .flatten()
        });
        let Some(left) = entered else {
            return Err(failed(format!("with-pane: there is no pane {nth}")));
        };
        let result = call.apply(&function, &[]);
        let cx = app(call)?;
        in_workspace(cx, |workspace, window, cx| {
            workspace.leave_pane(left, window, cx)
        });
        result
    });

    // (modified?): whether the tab up has changes that are not written.
    interp.define_native("modified?", 0, |call, _| {
        let cx = app(call)?;
        let dirty = origin(cx).and_then(|origin| {
            let read = |workspace: &Workspace, cx: &App| {
                Some(workspace.active_editor()?.read(cx).is_dirty())
            };
            origin.workspace.read_with(cx, read).ok().flatten()
        });
        Ok(Value::Bool(dirty.unwrap_or(false)))
    });

    // (focus-editor): give the keyboard back to the file being edited.
    interp.define_native("focus-editor", 0, |call, _| {
        let cx = app(call)?;
        match in_workspace(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx)
        }) {
            Some(()) => Ok(Value::Nil),
            None => Err(failed("there is no window with an editor")),
        }
    });

    // (open-terminal [command [dir]]): a terminal in a pane split off
    // below, with the keys, running `command` rather than the shell, in
    // `dir` (the project's root for nil, or what a relative path is from)
    // and the project's environment. With neither, the terminal still
    // running, if there is one. Its number, for the other terminal
    // functions.
    interp.define_native("open-terminal", Arity::between(0, 2), |call, args| {
        let text = |index: usize| match args.get(index) {
            None | Some(Value::Nil) => Ok(None),
            Some(text) => text.as_str().map(|text| Some(text.to_string())),
        };
        let command = text(0)?.filter(|command| !command.trim().is_empty());
        let dir = text(1)?;
        let cx = app(call)?;
        let opened = in_workspace(cx, |workspace, window, cx| {
            let cwd = match &dir {
                Some(dir) => workspace.root().join(dir),
                None => workspace.root().to_path_buf(),
            };
            let new = dir.is_some();
            let view = workspace.open_terminal(command, cwd, new, window, cx)?;
            Some(crate::workspace::terminal_id(&view))
        });
        match opened {
            Some(Some(id)) => Ok(Value::from(id as i64)),
            Some(None) => Err(failed("open-terminal: the terminal could not be started")),
            None => Err(failed("there is no window to open a terminal in")),
        }
    });

    // (terminals): every terminal, from the top left pane, as
    // (:id :title :command :running :exit): :command what it was started
    // to run, nil for the shell; :exit its exit code once it is done, nil
    // until then or if it was killed.
    interp.define_native("terminals", 0, |call, _| {
        let cx = app(call)?;
        let infos = origin(cx).and_then(|origin| {
            let read = |workspace: &Workspace, cx: &App| workspace.terminal_infos(cx);
            origin.workspace.read_with(cx, read).ok()
        });
        Ok(Value::list(infos.unwrap_or_default().into_iter().map(
            |info| {
                crate::host::record([
                    ("id", Value::from(info.id as i64)),
                    ("title", Value::from(info.title)),
                    ("command", info.command.map_or(Value::Nil, Value::from)),
                    ("running", Value::Bool(info.running)),
                    (
                        "exit",
                        info.exit_code
                            .map_or(Value::Nil, |code| Value::from(code as i64)),
                    ),
                ])
            },
        )))
    });

    // (terminal-send id text): `text` to the program in terminal `id`, as
    // if typed: "\r" is enter. False if the program is done.
    interp.define_native("terminal-send", 2, |call, args| {
        let text = args[1].as_str()?.to_string();
        let view = terminal_arg(call, &args[0], "terminal-send")?;
        let cx = app(call)?;
        let sent = view.update(cx, |view, _| view.send(&text));
        Ok(Value::Bool(sent))
    });

    // (terminal-keys id keys): keys to the program in terminal `id`, as
    // `bind` writes them, "<C-c>" or "q<CR>", as if pressed. False if the
    // program is done.
    interp.define_native("terminal-keys", 2, |call, args| {
        let keys = med_vim::parse_keys(args[1].as_str()?);
        let view = terminal_arg(call, &args[0], "terminal-keys")?;
        let cx = app(call)?;
        let sent = view.update(cx, |view, _| view.send_keys(&keys));
        Ok(Value::Bool(sent))
    });

    // (terminal-text id): what went by in terminal `id`, and its screen,
    // as text, a line for each row.
    interp.define_native("terminal-text", 1, |call, args| {
        let view = terminal_arg(call, &args[0], "terminal-text")?;
        let cx = app(call)?;
        Ok(Value::from(view.read(cx).history().text))
    });

    // (close-terminal id): close the tab of terminal `id`, hanging up on
    // its program. False if there is no such terminal.
    interp.define_native("close-terminal", 1, |call, args| {
        let id = args[0].as_int()?;
        let cx = app(call)?;
        let closed = in_workspace(cx, |workspace, window, cx| {
            u64::try_from(id).is_ok_and(|id| workspace.close_terminal(id, window, cx))
        });
        Ok(Value::Bool(closed.unwrap_or(false)))
    });

    // (load path): run another file. A path that does not start at the
    // top is looked for beside the file that asks, or else in the
    // configuration directory.
    interp.define_native("load", 1, |call, args| {
        let given = PathBuf::from(args[0].as_str()?);
        let cx = app(call)?;
        let beside = cx.global::<Script>().loading.last();
        let beside = beside.and_then(|file| file.parent().map(PathBuf::from));
        let path = match beside.or_else(crate::config_dir) {
            _ if given.is_absolute() => given,
            Some(dir) => dir.join(given),
            None => return Err(failed(format!("{}: beside what?", given.display()))),
        };
        let source = std::fs::read_to_string(&path)
            .map_err(|error| failed(format!("{}: {error}", path.display())))?;
        cx.global_mut::<Script>().loading.push(path.clone());
        let result = call.interp.eval_str(&source, call.host);
        call.host::<App>()?.global_mut::<Script>().loading.pop();
        match result {
            Ok(_) => Ok(Value::from(path.display().to_string())),
            Err(error) => Err(failed(in_file(&path, &error))),
        }
    });

    // (add-hook name function): call the function when what the hook is
    // named after happens. See HOOKS.
    interp.define_native("add-hook", 2, |call, args| {
        let given = name(&args[0])?;
        let Some(hook) = HOOKS.iter().find(|hook| **hook == given) else {
            return Err(failed(format!(
                "{given} is not a hook ({})",
                HOOKS.join(" ")
            )));
        };
        if !matches!(args[1], Value::Fn(_) | Value::Native(_)) {
            return Err(Error::new(
                ErrorKind::Type,
                format!("expected a function, got {}", args[1]),
            ));
        }
        let hooks = &mut app(call)?.global_mut::<Script>().hooks;
        hooks.entry(hook).or_default().push(args[1].clone());
        Ok(Value::Nil)
    });

    // (message part...): say something in the status bar.
    interp.define_native("message", Arity::at_least(1), |call, args| {
        let text: String = args.iter().map(Value::display).collect();
        app(call)?
            .global_mut::<Script>()
            .messages
            .push(text.clone());
        Ok(Value::from(text))
    });
}
