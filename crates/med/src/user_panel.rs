//! Panels: sidebars, popups and whole screens that a program defines.
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
//! (show-panel 'notes)    (hide-panel 'notes)
//! (toggle-panel 'notes)  (panel-shown? 'notes)
//! (focus-panel 'notes)   (panel-focused? 'notes)
//! (bind 'notes "q" "user.close-notes")
//! ```
//!
//! A panel is at the `'left` or `'right` of the editor, a `'popup` over
//! it, which a click beside it or escape puts away, or `'full`, in place
//! of the editor. Its layout is written as in `panel.rs` and gets a
//! `panel` with `:name`, `:file` (the file being edited, or nil), `:root`
//! and `:focused`.
//!
//! What a panel shows beyond that is the program's own: values it keeps
//! with `def` and changes with `set!`, from the function it gives
//! `on-click` or from a command. Panels are drawn again whenever a program
//! has run, so they show the change.
//!
//! A panel that has the keyboard (`focus-panel`) looks its keys up in the
//! key table of its own name, or in those `:contexts` names: a list, or a
//! function that gives one, the first table to know a key deciding. When
//! the panel is put away the keyboard goes back to where it was.

use crate::commands::Args;
use crate::keymap::{KeySequence, Press, panel_key};
use crate::panel::{self, Click, InputSpec, Layouts, Node, Vars};
use crate::script;
use crate::workspace::Workspace;
use gpui::component::input::{InputEvent, InputState};
use gpui::prelude::*;
use gpui::*;
use med_lisp::{Arity, Error, ErrorKind, Interp, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// Where a panel is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    Left,
    Right,
    /// Over the editor, until it is put away.
    Popup,
    /// In place of the editor and the sidebars.
    Full,
}

/// The key tables a panel with the keyboard looks its keys up in.
#[derive(Clone)]
enum Contexts {
    /// The one of the panel's own name.
    Own,
    Named(Vec<String>),
    /// A function of no arguments that gives the names.
    Function(Value),
}

#[derive(Clone)]
pub struct UserPanel {
    pub name: SharedString,
    pub at: At,
    pub width: f32,
    contexts: Contexts,
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

    pub fn is_shown(&self, name: &str) -> bool {
        self.shown.contains(name)
    }

    pub fn show(&mut self, name: &SharedString) {
        self.shown.insert(name.clone());
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

    /// Whether `context` is a key table of one of the panels: its name, or
    /// its name and a part after a dot (`git.files`).
    pub fn has_context(&self, context: &str) -> bool {
        self.names().any(|name| {
            context
                .strip_prefix(name)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
        })
    }
}

pub fn init(cx: &mut App) {
    cx.set_global(UserPanels::default());
}

/// Forget the panels that were defined, as before the init file is run
/// again. Which ones were up is remembered for those it defines anew.
pub(crate) fn reset(cx: &mut App) {
    cx.global_mut::<UserPanels>().panels.clear();
    cx.global_mut::<Layouts>().clear();
}

fn failed(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Raised, message)
}

/// The panel that `value` names.
fn defined(value: &Value, cx: &App) -> Result<SharedString, Error> {
    let panels = cx.global::<UserPanels>();
    let name = panel::word(value).unwrap_or_default();
    match panels.panels.iter().find(|panel| *panel.name == name) {
        Some(panel) => Ok(panel.name.clone()),
        None => {
            let names: Vec<&str> = panels.names().collect();
            Err(failed(format!(
                "{value} is not a panel ({})",
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
    let show = shown(panels.is_shown(&name));
    if show {
        panels.show(&name);
    } else {
        panels.hide(&name);
    }
    // A panel that went away with the keyboard gives it back.
    script::in_workspace(cx, |workspace, window, cx| {
        workspace.panels_changed(window, cx);
    });
    cx.refresh_windows();
    Ok(Value::Bool(show))
}

/// The view of the panel `name`, in the window the program was started
/// from.
fn view_of(name: &SharedString, cx: &mut App) -> Result<Entity<PanelView>, Error> {
    script::in_workspace(cx, |workspace, _, cx| workspace.panel_view(name, cx))
        .ok_or_else(|| failed(format!("there is no window with the panel {name}")))
}

pub(crate) fn define_functions(interp: &mut Interp) {
    // (defpanel name options... function): options are :at ('left 'right
    // 'popup 'full), :width, :shown to have it up from the start, and
    // :contexts, the key tables it looks its keys up in.
    interp.define_native("defpanel", Arity::at_least(2), |call, args| {
        let Some(name) = panel::word(&args[0]).filter(|name| !name.is_empty()) else {
            return Err(panel::wrong("defpanel", "a name", &args[0]));
        };
        let [options @ .., function] = &args[1..] else {
            unreachable!("defpanel takes at least two arguments");
        };
        if !panel::is_function(function) {
            return Err(panel::wrong("defpanel", "a function, last", function));
        }

        let (mut at, mut width, mut shown) = (At::Right, None, false);
        let mut contexts = Contexts::Own;
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
                        ("full", At::Full),
                    ];
                    at = panel::choice("at", value, &places)?;
                }
                "width" => match panel::number("width", value)? {
                    width_given if width_given > 0. => width = Some(width_given),
                    _ => return Err(panel::wrong("width", "a width", value)),
                },
                "shown" => shown = value.is_truthy(),
                "contexts" => {
                    contexts = match value {
                        function if panel::is_function(function) => {
                            Contexts::Function(function.clone())
                        }
                        names => Contexts::Named(context_names(names)?),
                    }
                }
                other => {
                    return Err(failed(format!(
                        "{other} is not an option of defpanel (at width shown contexts)"
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
            contexts,
        };
        let panels = cx.global_mut::<UserPanels>();
        match panels.panels.iter_mut().find(|known| known.name == name) {
            Some(known) => *known = panel,
            None => panels.panels.push(panel),
        }
        if panels.known.insert(name.clone()) && shown {
            panels.show(&name);
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

    // (focus-panel name): put the panel up and give it the keyboard, which
    // goes to its input if it has one.
    interp.define_native("focus-panel", 1, |call, args| {
        let cx = script::app(call)?;
        let name = defined(&args[0], cx)?;
        cx.global_mut::<UserPanels>().show(&name);
        let focused = script::in_workspace(cx, |workspace, window, cx| {
            let view = workspace.panel_view(&name, cx);
            view.update(cx, |view, cx| view.take_focus(window, cx));
            cx.notify();
        });
        match focused {
            Some(()) => Ok(Value::Nil),
            None => Err(failed(format!("there is no window to show {name} in"))),
        }
    });

    // (panel-focused? name): whether the panel has the keyboard.
    interp.define_native("panel-focused?", 1, |call, args| {
        let cx = script::app(call)?;
        let name = defined(&args[0], cx)?;
        let focused = script::in_workspace(cx, |workspace, window, cx| {
            let view = workspace.panel_view(&name, cx);
            view.read(cx).has_focus(window, cx)
        });
        Ok(Value::Bool(focused.unwrap_or(false)))
    });

    // (input-text panel input): what is typed in an input of the panel.
    interp.define_native("input-text", 2, |call, args| {
        let cx = script::app(call)?;
        let name = defined(&args[0], cx)?;
        let Some(input) = panel::word(&args[1]) else {
            return Err(panel::wrong("input-text", "the name of an input", &args[1]));
        };
        let view = view_of(&name, cx)?;
        Ok(Value::from(view.read(cx).input_text(&input, cx)))
    });

    // (set-input panel input text): put text in an input of the panel.
    // This is not typing: the input's `:on-change` is not called.
    interp.define_native("set-input", 3, |call, args| {
        let text = args[2].display();
        let cx = script::app(call)?;
        let name = defined(&args[0], cx)?;
        let Some(input) = panel::word(&args[1]) else {
            return Err(panel::wrong("set-input", "the name of an input", &args[1]));
        };
        let view = view_of(&name, cx)?;
        view.update(cx, |view, cx| view.set_input(input.into(), text, cx));
        Ok(Value::Nil)
    });
}

/// Key tables as a program names them: one name or a list of names.
fn context_names(value: &Value) -> Result<Vec<String>, Error> {
    let names = match value {
        Value::Pair(_) => value.to_vec()?,
        one => vec![one.clone()],
    };
    names
        .iter()
        .map(|name| {
            panel::word(name).ok_or_else(|| panel::wrong("contexts", "names of key tables", value))
        })
        .collect()
}

/// An input of a panel: the text in it, and what its layout last asked
/// to be told of.
struct InputSlot {
    state: Entity<InputState>,
    spec: InputSpec,
    _subscription: Subscription,
}

/// Where a list of a panel is scrolled to, and what its layout last asked
/// to have in view.
#[derive(Default)]
struct ListScroll {
    handle: UniformListScrollHandle,
    reveal: Option<usize>,
    top: Option<usize>,
}

/// The view of one panel, inside a workspace.
pub struct PanelView {
    name: SharedString,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    /// A key sequence in progress (`g` of `gg`).
    keys: KeySequence,
    inputs: HashMap<SharedString, InputSlot>,
    /// Text a program put in an input, until the input is drawn.
    pending_text: HashMap<SharedString, String>,
    /// The lists, in the order the layout has them.
    lists: RefCell<Vec<ListScroll>>,
    /// The keyboard was given to the panel, and goes on to its input once
    /// that is drawn.
    wants_input: bool,
    /// Where the keyboard was when the panel took it.
    return_to: Option<FocusHandle>,
}

impl PanelView {
    pub fn new(
        name: SharedString,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            name,
            workspace,
            focus_handle: cx.focus_handle(),
            keys: KeySequence::default(),
            inputs: HashMap::new(),
            pending_text: HashMap::new(),
            lists: RefCell::default(),
            wants_input: false,
            return_to: None,
        }
    }

    /// Whether `handle` is the panel's or one of its inputs'.
    pub(crate) fn owns(&self, handle: &FocusHandle, cx: &App) -> bool {
        self.focus_handle == *handle
            || self
                .inputs
                .values()
                .any(|slot| slot.state.focus_handle(cx) == *handle)
    }

    pub(crate) fn has_focus(&self, window: &Window, cx: &App) -> bool {
        window
            .focused(cx)
            .is_some_and(|focused| self.owns(&focused, cx))
    }

    /// Take the keyboard, remembering where it was.
    pub(crate) fn take_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_focus(window, cx) {
            self.return_to = window.focused(cx);
        }
        self.focus_handle.focus(window, cx);
        self.wants_input = true;
        self.keys.clear();
        cx.notify();
    }

    /// Where the keyboard was before the panel took it, to give it back.
    pub(crate) fn take_return(&mut self) -> Option<FocusHandle> {
        self.wants_input = false;
        self.return_to.take()
    }

    /// The input `name`, once it has been drawn.
    pub(crate) fn input(&self, name: &str) -> Option<&Entity<InputState>> {
        self.inputs.get(name).map(|slot| &slot.state)
    }

    fn input_text(&self, name: &str, cx: &App) -> String {
        match (self.pending_text.get(name), self.inputs.get(name)) {
            (Some(text), _) => text.clone(),
            (None, Some(slot)) => slot.state.read(cx).value().to_string(),
            (None, None) => String::new(),
        }
    }

    /// Put `text` in the input `name`, now or when it is first drawn.
    fn set_input(&mut self, name: SharedString, text: String, cx: &mut Context<Self>) {
        self.pending_text.insert(name, text);
        cx.notify();
    }

    /// Where the panel's list number `index` is scrolled to, moved to what
    /// the layout wants in view if that is not what it wanted last time.
    pub(crate) fn list_scroll(
        &self,
        index: usize,
        reveal: Option<usize>,
        top: Option<usize>,
    ) -> UniformListScrollHandle {
        let mut lists = self.lists.borrow_mut();
        if lists.len() <= index {
            lists.resize_with(index + 1, ListScroll::default);
        }
        let list = &mut lists[index];
        if list.top != top {
            list.top = top;
            if let Some(row) = top {
                list.handle.scroll_to_item_strict(row, ScrollStrategy::Top);
            }
        }
        if list.reveal != reveal {
            list.reveal = reveal;
            if let Some(row) = reveal {
                list.handle.scroll_to_item(row, ScrollStrategy::Center);
            }
        }
        list.handle.clone()
    }

    /// Have an input for each one the layout describes: made when first
    /// seen, kept while it is there, with what it is to tell the program.
    fn sync_inputs(&mut self, node: &Node, window: &mut Window, cx: &mut Context<Self>) {
        let mut specs = Vec::new();
        node.inputs(&mut specs);
        self.inputs
            .retain(|name, _| specs.iter().any(|spec| spec.name == *name));
        for spec in &specs {
            match self.inputs.get_mut(&spec.name) {
                Some(slot) => {
                    if slot.spec.placeholder != spec.placeholder {
                        let placeholder = spec.placeholder.clone();
                        slot.state.update(cx, |input, cx| {
                            input.set_placeholder(placeholder, window, cx);
                        });
                    }
                    slot.spec = (*spec).clone();
                }
                None => {
                    let placeholder = spec.placeholder.clone();
                    let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
                    let name = spec.name.clone();
                    let subscription = cx.subscribe_in(
                        &state,
                        window,
                        move |view, state, event: &InputEvent, window, cx| {
                            let Some(slot) = view.inputs.get(&name) else {
                                return;
                            };
                            let function = match event {
                                InputEvent::Change => slot.spec.on_change.clone(),
                                InputEvent::PressEnter { .. } => slot.spec.on_submit.clone(),
                                _ => None,
                            };
                            if let Some(function) = function {
                                let text = state.read(cx).value().to_string();
                                view.call(function, vec![Value::from(text)], window, cx);
                            }
                        },
                    );
                    let slot = InputSlot {
                        state,
                        spec: (*spec).clone(),
                        _subscription: subscription,
                    };
                    self.inputs.insert(spec.name.clone(), slot);
                }
            }
            if let Some(text) = self.pending_text.remove(&spec.name) {
                let state = self.inputs[&spec.name].state.clone();
                if state.read(cx).value() != text {
                    state.update(cx, |input, cx| input.set_value(text, window, cx));
                }
            }
        }
        // Text for an input that is not there has nowhere to go.
        self.pending_text.clear();

        if self.wants_input {
            self.wants_input = false;
            if let Some(spec) = specs.first()
                && self.focus_handle.is_focused(window)
            {
                let state = self.inputs[&spec.name].state.clone();
                state.update(cx, |input, cx| input.focus(window, cx));
            }
        }
    }

    /// The key tables to look a key up in, the first to know it deciding.
    fn contexts(&self, cx: &mut Context<Self>) -> Vec<String> {
        let own = || vec![self.name.to_string()];
        let panels = cx.global::<UserPanels>();
        let contexts = panels
            .panels
            .iter()
            .find(|panel| panel.name == self.name)
            .map(|panel| panel.contexts.clone());
        match contexts {
            None | Some(Contexts::Own) => own(),
            Some(Contexts::Named(names)) => names,
            // A function that fails leaves the panel with its own table.
            Some(Contexts::Function(function)) => script::call_to_draw(&function, &[], cx)
                .and_then(|names| context_names(&names))
                .unwrap_or_else(|_| own()),
        }
    }

    /// A key on its way to the panel or to one of its inputs: the panel's
    /// tables get to see it first.
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = panel_key(&event.keystroke) else {
            return;
        };
        let contexts = self.contexts(cx);
        let contexts: Vec<&str> = contexts.iter().map(String::as_str).collect();
        match self.keys.press(key, &contexts, cx) {
            // Not ours: let it travel on, to an input or out of the panel.
            Press::Unbound => return,
            Press::Pending => {}
            Press::Command(name) => {
                // Once the panel is done with the key: the command may
                // want to look at the panel.
                let workspace = self.workspace.clone();
                window.defer(cx, move |window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.message = None;
                            workspace.run_command(&name, &Args::default(), window, cx);
                        })
                        .ok();
                });
            }
        }
        cx.stop_propagation();
    }

    /// Call a function of the panel's program.
    fn call(
        &mut self,
        function: Value,
        args: Vec<Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace
            .update(cx, |_, cx| script::call_from(function, args, window, cx))
            .ok();
    }

    /// A node was clicked.
    pub(crate) fn click(&mut self, action: &Click, window: &mut Window, cx: &mut Context<Self>) {
        match action.clone() {
            Click::Command(name) => {
                let run = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
                    workspace.run_command(&name, &Args::default(), window, cx)
                };
                self.workspace.update(cx, run).ok();
            }
            Click::Function(function) => self.call(function, Vec::new(), window, cx),
        }
    }

    /// What a layout can draw from: `:name`, `:file`, `:root` and
    /// `:focused`.
    fn vars(&self, window: &Window, cx: &App) -> Vars {
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
            .set("focused", self.has_focus(window, cx))
    }
}

impl Render for PanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let vars = self.vars(window, cx);
        let name = self.name.clone();
        let body = match panel::layout(&name, vars, cx) {
            Ok(node) => {
                self.sync_inputs(&node, window, cx);
                panel::draw(&node, self, cx)
            }
            Err(error) => panel::problem(&error, cx),
        };
        div()
            .id(name)
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("Panel")
            // A click leaves the keyboard where it is: a program gives it
            // to the panel, with `focus-panel`. (An input is clicked into
            // all the same: it sees the click before this does.)
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .capture_key_down(cx.listener(Self::on_key_down))
            .child(body)
    }
}
