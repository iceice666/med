//! Everything the application can do, by name.
//!
//! Keys, ex commands and menu shortcuts never call into a view directly:
//! they name a command, and the registry says which view runs it. The vim
//! engine's own commands (`motion.*`, `operator.*`…) are not here — a key
//! bound to one of those never leaves the engine.

use crate::editor::Editor;
use crate::workspace::Workspace;
use gpui::{Action, App, Context, Global, SharedString, Window};
use std::collections::HashMap;

/// What a command was invoked with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// The count typed before the key (`3gt`).
    pub count: Option<usize>,
    /// The rest of an ex command line (`foo.txt` in `:w foo.txt`).
    pub arg: String,
    /// The ex command was given with `!`.
    pub bang: bool,
}

/// The view a command acts on, and how.
#[derive(Clone)]
pub enum Handler {
    Workspace(fn(&mut Workspace, &Args, &mut Window, &mut Context<Workspace>)),
    /// Runs on the active editor.
    Editor(fn(&mut Editor, &Args, &mut Context<Editor>)),
    /// A function of a program's, defined with `defcommand`. The workspace
    /// hands it to the interpreter.
    Script(med_lisp::Value),
}

pub struct Command {
    pub name: SharedString,
    pub description: SharedString,
    pub handler: Handler,
}

#[derive(Default)]
pub struct Commands {
    commands: HashMap<SharedString, Command>,
    /// Ex command names (`w`, `write`) → command names.
    ex: HashMap<SharedString, SharedString>,
}

impl Global for Commands {}

impl Commands {
    pub fn register(
        &mut self,
        name: impl Into<SharedString>,
        description: impl Into<SharedString>,
        handler: Handler,
    ) {
        let name = name.into();
        self.commands.insert(
            name.clone(),
            Command {
                name,
                description: description.into(),
                handler,
            },
        );
    }

    /// Make `command` available on the command line under each of `names`.
    pub fn ex(&mut self, names: &[&str], command: &str) {
        for name in names {
            self.ex
                .insert(SharedString::new(*name), SharedString::new(command));
        }
    }

    pub fn get(&self, name: &str) -> Option<&Command> {
        self.commands.get(name)
    }

    pub fn handler(&self, name: &str) -> Option<Handler> {
        self.get(name).map(|command| command.handler.clone())
    }

    /// The command an ex command line's first word stands for: one of the
    /// names given to [`Commands::ex`], or a command's full name.
    pub fn resolve_ex(&self, name: &str) -> Option<SharedString> {
        self.ex
            .get(name)
            .or_else(|| self.commands.get(name).map(|command| &command.name))
            .cloned()
    }

    /// The ex command names `command` goes by, shortest first.
    pub fn ex_names(&self, command: &str) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .ex
            .iter()
            .filter(|(_, to)| to.as_ref() == command)
            .map(|(name, _)| name.as_ref())
            .collect();
        names.sort_by_key(|name| (name.len(), *name));
        names
    }

    /// Every name given to [`Commands::ex`], in no order.
    pub fn all_ex_names(&self) -> impl Iterator<Item = &str> {
        self.ex.keys().map(|name| name.as_ref())
    }

    /// Every command, sorted by name.
    pub fn all(&self) -> Vec<&Command> {
        let mut commands: Vec<&Command> = self.commands.values().collect();
        commands.sort_by(|a, b| a.name.cmp(&b.name));
        commands
    }
}

/// The GPUI action behind every app-level shortcut (`cmd-s`…): it carries a
/// command name up to the workspace, which runs it.
#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = med, no_json)]
pub struct RunCommand(pub SharedString);

/// Register the commands of every view.
pub fn init(cx: &mut App) {
    let mut commands = Commands::default();
    crate::workspace::register(&mut commands);
    crate::editor::register(&mut commands);
    cx.set_global(commands);
}
