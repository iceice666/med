//! Panels of the user's own: sidebars and popups a program defines.
//!
//! ```lisp
//! (def notes '("call Ada" "ship it"))
//!
//! (defpanel 'notes :at 'right :width 240
//!   (fn (panel)
//!     (col :width 'full :height 'full :bg 'sidebar
//!       (text "Notes" :bold true :padding-x 12 :height 32)
//!       (rows notes :grow true
//!         (fn (note) (row :height 24 :padding-x 12 (text note)))))))
//!
//! (show-panel 'notes)   (hide-panel 'notes)
//! (toggle-panel 'notes) (panel-shown? 'notes)
//! ```
//!
//! A panel is at the `'left` or `'right` of the editor, or a `'popup` over
//! it, which a click beside it or escape puts away. Its layout is written
//! as any other (see `panel.rs`) and gets a `panel` with `:name`, `:file`
//! (the file being edited, or nil) and `:root`.
//!
//! What a panel shows beyond that is the program's own: values it keeps
//! with `def` and changes with `set!`, from the function it gives
//! `on-click` or from a command. Panels are drawn again whenever a program
//! has run, so they show the change.

use crate::commands::Args;
use crate::panel::{self, Click, Host, Layouts, Vars};
use crate::script;
use crate::workspace::Workspace;
use gpui::prelude::*;
use gpui::*;
use med_lisp::{Arity, Error, ErrorKind, Interp, Value};
use std::collections::HashSet;

/// Where a panel is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    Left,
    Right,
    /// Over the editor, until it is put away.
    Popup,
}

#[derive(Clone)]
pub struct UserPanel {
    pub name: SharedString,
    pub at: At,
    pub width: f32,
}

#[derive(Default)]
pub struct UserPanels {
    /// In the order they were defined, which is the order they are drawn.
    panels: Vec<UserPanel>,
    shown: HashSet<SharedString>,
    /// Every name that has been defined, so that `:shown true` puts a
    /// panel up when it is first defined and not each time the init file
    /// is run again.
    known: HashSet<SharedString>,
}

impl Global for UserPanels {}

impl UserPanels {
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.panels.iter().map(|panel| panel.name.as_ref())
    }

    /// The panels to draw now.
    pub fn shown(&self) -> impl Iterator<Item = &UserPanel> {
        self.panels
            .iter()
            .filter(|panel| self.shown.contains(&panel.name))
    }

    pub fn hide(&mut self, name: &str) {
        self.shown.remove(name);
    }

    /// Put every popup away. False if none was up.
    pub fn hide_popups(&mut self) -> bool {
        let popups: Vec<&SharedString> = self
            .panels
            .iter()
            .filter(|panel| panel.at == At::Popup)
            .map(|panel| &panel.name)
            .collect();
        let before = self.shown.len();
        self.shown.retain(|name| !popups.contains(&name));
        self.shown.len() != before
    }
}

pub fn init(cx: &mut App) {
    cx.set_global(UserPanels::default());
}

/// Forget the panels that were defined, as before the init file is run
/// again. Which ones were up is remembered for those it defines anew.
pub(crate) fn reset(cx: &mut App) {
    cx.global_mut::<UserPanels>().panels.clear();
    cx.global_mut::<Layouts>().forget_users();
}

fn failed(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Raised, message)
}

/// The panel of the user's that `value` names.
fn defined(value: &Value, cx: &App) -> Result<SharedString, Error> {
    let panels = cx.global::<UserPanels>();
    let name = panel::word(value).unwrap_or_default();
    match panels.panels.iter().find(|panel| *panel.name == name) {
        Some(panel) => Ok(panel.name.clone()),
        None => {
            let names: Vec<&str> = panels.names().collect();
            Err(failed(format!(
                "{value} is not a panel of yours ({})",
                names.join(" ")
            )))
        }
    }
}

/// Show or hide a panel: `shown` says which from how it is now.
fn set_shown(
    call: &mut med_lisp::Call<'_>,
    name: &Value,
    shown: fn(bool) -> bool,
) -> Result<Value, Error> {
    let cx = script::app(call)?;
    let name = defined(name, cx)?;
    let panels = cx.global_mut::<UserPanels>();
    let show = shown(panels.shown.contains(&name));
    if show {
        panels.shown.insert(name);
    } else {
        panels.shown.remove(&name);
    }
    cx.refresh_windows();
    Ok(Value::Bool(show))
}

pub(crate) fn define_functions(interp: &mut Interp) {
    // (defpanel name options... function): options are :at ('left 'right
    // 'popup), :width, and :shown to have it up from the start.
    interp.define_native("defpanel", Arity::at_least(2), |call, args| {
        let Some(name) = panel::word(&args[0]).filter(|name| !name.is_empty()) else {
            return Err(panel::wrong("defpanel", "a name", &args[0]));
        };
        if panel::is_built_in(&name) {
            return Err(failed(format!(
                "{name} is one of the editor's own panels: set-layout changes how it looks"
            )));
        }
        let [options @ .., function] = &args[1..] else {
            unreachable!("defpanel takes at least two arguments");
        };
        if !matches!(function, Value::Fn(_) | Value::Native(_)) {
            return Err(panel::wrong("defpanel", "a function, last", function));
        }

        let (mut at, mut width, mut shown) = (At::Right, None, false);
        for option in options.chunks(2) {
            let [Value::Keyword(key), value] = option else {
                return Err(failed(format!(
                    "defpanel: expected an option and its value, got {}",
                    option[0]
                )));
            };
            match &*key.name() {
                "at" => {
                    let places = [
                        ("left", At::Left),
                        ("right", At::Right),
                        ("popup", At::Popup),
                    ];
                    at = panel::choice("at", value, &places)?;
                }
                "width" => match panel::number("width", value)? {
                    width_given if width_given > 0. => width = Some(width_given),
                    _ => return Err(panel::wrong("width", "a width", value)),
                },
                "shown" => shown = value.is_truthy(),
                other => {
                    return Err(failed(format!(
                        "{other} is not an option of defpanel (at width shown)"
                    )));
                }
            }
        }
        let width = width.unwrap_or(if at == At::Popup { 480. } else { 260. });

        let cx = script::app(call)?;
        let name = SharedString::from(name);
        let panel = UserPanel {
            name: name.clone(),
            at,
            width,
        };
        let panels = cx.global_mut::<UserPanels>();
        match panels.panels.iter_mut().find(|known| known.name == name) {
            Some(known) => *known = panel,
            None => panels.panels.push(panel),
        }
        if panels.known.insert(name.clone()) && shown {
            panels.shown.insert(name.clone());
        }
        cx.global_mut::<Layouts>().set(&name, function.clone());
        cx.refresh_windows();
        Ok(Value::from(name.as_ref()))
    });

    // Each gives back whether the panel is up afterwards.
    interp.define_native("show-panel", 1, |call, args| {
        set_shown(call, &args[0], |_| true)
    });
    interp.define_native("hide-panel", 1, |call, args| {
        set_shown(call, &args[0], |_| false)
    });
    interp.define_native("toggle-panel", 1, |call, args| {
        set_shown(call, &args[0], |shown| !shown)
    });
    interp.define_native("panel-shown?", 1, |call, args| {
        set_shown(call, &args[0], |shown| shown)
    });
}

/// The view of one panel of the user's, inside a workspace.
pub struct PanelView {
    name: SharedString,
    workspace: WeakEntity<Workspace>,
}

impl PanelView {
    pub fn new(name: SharedString, workspace: WeakEntity<Workspace>) -> Self {
        Self { name, workspace }
    }
}

/// What a panel of the user's can draw from: `:name`, `:file` and `:root`.
/// It has no lists of its own: `rows` is given one of the program's.
impl Host for PanelView {
    fn vars(&self, _: &Window, cx: &App) -> Vars {
        let read = |workspace: &Workspace, cx: &App| {
            let file = workspace
                .active_editor()
                .and_then(|editor| editor.read(cx).path.clone());
            (file, workspace.root().to_path_buf())
        };
        let (file, root) = self.workspace.read_with(cx, read).ok().unzip();
        let path = |path: std::path::PathBuf| Value::from(path.display().to_string());
        Vars::default()
            .set("name", self.name.as_ref())
            .set("file", file.flatten().map_or(Value::Nil, path))
            .set("root", root.map_or(Value::Nil, path))
    }

    fn row_count(&self, _: &str) -> usize {
        0
    }

    fn row_vars(&self, _: &str, _: usize, _: &App) -> Vars {
        Vars::default()
    }

    fn scroll_handle(&self, _: &str) -> Option<&UniformListScrollHandle> {
        None
    }

    fn click(
        &mut self,
        action: &Click,
        _: Option<(&str, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action = action.clone();
        let run = |workspace: &mut Workspace, cx: &mut Context<Workspace>| match action {
            Click::Command(name) => workspace.run_command(&name, &Args::default(), window, cx),
            Click::Function(function) => {
                script::run_command(function, Args::default(), window, cx);
            }
        };
        self.workspace.update(cx, run).ok();
    }
}

impl Render for PanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.name.clone();
        div()
            .size_full()
            .child(panel::render(&name, self, window, cx))
    }
}
