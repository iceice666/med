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
//! (unbind 'normal "gt")
//! (set-option 'font-size 16)           ; also font-family and lsp
//! (message "ready")                    ; shown in the status bar
//!
//! (defcommand "user.save-and-close" "Write the file, then close its tab"
//!   (fn ()
//!     (run "workspace.save")
//!     (run "workspace.close_tab")))
//! (run "workspace.edit" "notes.md")    ; a command, with its argument
//! (current-file)  (project-root)
//! (open-file "src/main.rs" 12 1)       ; in a tab, at a line and column
//! (focus-editor)
//! (load "tree.lisp")                   ; another file, from beside this one
//! (add-hook 'files-changed refresh)    ; be told of something: HOOKS
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
use crate::keymap::PanelKeymaps;
use crate::user_panel::UserPanels;
use crate::workspace::Workspace;
use crate::{Settings, VimGlobal};
use gpui::{
    Action, AnyWindowHandle, App, AppContext as _, Context, Global, KeyBinding,
    KeyBindingContextPredicate, NoAction, SharedString, WeakEntity, Window,
};
use med_lisp::{Arity, Call, Error, ErrorKind, Interp, Value};
use med_vim::MapMode;
use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

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
const HOOKS: &[&str] = &["opened", "files-changed", "locations"];

/// The key table of the shortcuts the window system handles (`cmd-s`).
const APP_TABLE: &str = "app";

const MODES: &[(&str, MapMode)] = &[
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
    /// The files being loaded, the innermost last: `load` looks beside it.
    loading: Vec<PathBuf>,
    /// The init file that was run, to run again.
    init_file: Option<PathBuf>,
    /// The shortcuts programs have bound in the `app` table.
    app_keys: HashSet<String>,
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
    for (name, value) in INVOCATION.iter().zip(invocation(&Args::default())) {
        interp.define(name, value);
    }
    cx.set_global(Script {
        interp: Some(interp),
        messages: Vec::new(),
        origin: None,
        drawing: false,
        hooks: HashMap::new(),
        loading: Vec::new(),
        init_file: None,
        app_keys: HashSet::new(),
        search: Arc::default(),
    });
}

/// Forget the panels and hooks that programs defined, as before the init
/// file is run again.
fn reset_panels(cx: &mut App) {
    crate::user_panel::reset(cx);
    cx.global_mut::<Script>().hooks.clear();
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
        invoke(interp, cx, &function, &args)?;
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

/// Call a command's function. While it runs, the names in [`INVOCATION`]
/// say how it was invoked; a command that runs another gets them back
/// afterwards.
fn invoke(
    interp: &mut Interp,
    host: &mut dyn Any,
    function: &Value,
    args: &Args,
) -> Result<Value, Error> {
    let before = INVOCATION.map(|name| interp.global(name).cloned().unwrap_or(Value::Nil));
    for (name, value) in INVOCATION.iter().zip(invocation(args)) {
        interp.define(name, value);
    }
    let result = interp.call(function, &[], host);
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
/// defined last time are forgotten first, in case it no longer defines
/// them; the keys and options it set are not undone.
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

/// Keys are bound in an editor mode, in a panel, or for the whole window.
enum Table {
    Mode(MapMode),
    Panel(String),
    App,
}

/// The tables `value` names: one name, or a list of them.
fn tables(value: &Value, cx: &App) -> Result<Vec<Table>, Error> {
    let names = match value {
        Value::Pair(_) => value.to_vec()?,
        one => vec![one.clone()],
    };
    let mut tables = Vec::new();
    for value in &names {
        let name = name(value)?;
        let panels = cx.global::<UserPanels>();
        if let Some((_, mode)) = MODES.iter().find(|(mode, _)| *mode == name) {
            tables.push(Table::Mode(*mode));
        } else if name == APP_TABLE {
            tables.push(Table::App);
        } else if panels.has_context(&name) {
            tables.push(Table::Panel(name));
        } else {
            let modes: Vec<&str> = MODES.iter().map(|(mode, _)| *mode).collect();
            let panels: Vec<&str> = panels.names().collect();
            return Err(failed(format!(
                "{name} is neither a mode ({}), {APP_TABLE}, nor a panel ({})",
                modes.join(" "),
                panels.join(" ")
            )));
        }
    }
    Ok(tables)
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
                Table::Panel(panel) => {
                    cx.global_mut::<PanelKeymaps>().bind(&panel, keys, command);
                }
                Table::App => {
                    let action = RunCommand(SharedString::from(command.to_string()));
                    let binding = app_binding(keys, Box::new(action), cx)?;
                    cx.bind_keys([binding]);
                    let bound = &mut cx.global_mut::<Script>().app_keys;
                    bound.insert(keys.to_string());
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

    // (unbind table keys): whether there was anything to remove.
    interp.define_native("unbind", 2, |call, args| {
        let keys = keys(&args[1])?;
        let cx = app(call)?;
        let mut removed = false;
        for table in tables(&args[0], cx)? {
            removed |= match table {
                Table::Mode(mode) => cx.global_mut::<VimGlobal>().0.keymap.unbind(mode, keys),
                Table::Panel(panel) => cx.global_mut::<PanelKeymaps>().unbind(&panel, keys),
                Table::App => {
                    let bound = cx.global_mut::<Script>().app_keys.remove(keys);
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
            "lsp" => match value {
                Value::Bool(enabled) => settings.lsp = *enabled,
                other => {
                    return Err(Error::new(
                        ErrorKind::Type,
                        format!("expected true or false, got {other}"),
                    ));
                }
            },
            other => {
                return Err(failed(format!(
                    "{other} is not an option (font-size font-family lsp)"
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
            invoke(call.interp, call.host, &function, &invocation)?;
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
