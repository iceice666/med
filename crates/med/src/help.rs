//! `:help`: what a command does, and where it can be run from.
//!
//! Nothing here is written by hand. A page is made, when it is asked for,
//! from what the editor knows at that moment: the description a command
//! was registered with (or `defcommand` gave it), the ex names it goes
//! by, and the keys the tables bind to it, the init file's included.

use crate::commands::Commands;
use crate::keymap::PanelKeymaps;
use crate::script::{APP_TABLE, MODES};
use crate::{VimGlobal, script};
use gpui::App;
use med_vim::{KeysFor, MapMode};
use std::fmt::Write;

/// The name of the tab the pages are shown in.
pub const TAB: &str = "help";

/// The page for `topic`: a command by its name or by one of its ex names
/// (`w`, `:w`), or with no topic the list of every command. `None` when
/// `topic` is none of those.
pub fn page(topic: &str, cx: &App) -> Option<String> {
    let topic = topic.trim();
    let topic = topic.strip_prefix(':').unwrap_or(topic);
    if topic.is_empty() {
        return Some(index(cx));
    }
    let commands = cx.global::<Commands>();
    let known = commands.get(topic).is_some() || is_engine(topic);
    let name = match known {
        true => topic.to_string(),
        false => commands.resolve_ex(topic)?.to_string(),
    };
    Some(command_page(&name, cx))
}

fn is_engine(name: &str) -> bool {
    med_vim::command_names().any(|engine| engine == name)
}

/// The name of a table, as `bind` takes it.
fn mode_table(mode: MapMode) -> &'static str {
    MODES
        .iter()
        .find(|(_, of)| *of == mode)
        .map_or("?", |(name, _)| name)
}

fn table_name(found: &KeysFor) -> String {
    match (&found.table, found.mode) {
        (None, mode) => mode_table(mode).to_string(),
        (Some(table), MapMode::Normal) => table.clone(),
        (Some(table), mode) => format!("{table}.{}", mode_table(mode)),
    }
}

/// Every key bound to `command`: the table and the keys, in `bind`'s
/// notation, the editor's tables first, then the panels', then the app's.
fn keys_for(command: &str, cx: &App) -> Vec<(String, String)> {
    let editor = cx.global::<VimGlobal>().0.keymap.keys_for(command);
    let editor = editor
        .into_iter()
        .map(|found| (table_name(&found), found.keys));
    let panels = cx.global::<PanelKeymaps>().keys_for(command);
    let app = script::app_keys_for(command, cx)
        .into_iter()
        .map(|keys| (APP_TABLE.to_string(), keys));
    editor.chain(panels).chain(app).collect()
}

fn command_page(name: &str, cx: &App) -> String {
    let commands = cx.global::<Commands>();
    let mut page = String::new();
    let _ = writeln!(page, "{name}");
    match commands.get(name) {
        Some(command) => {
            let _ = writeln!(page, "{}", command.description);
        }
        None => {
            let _ = writeln!(
                page,
                "One of the vim engine's own: keys can run it, the command line cannot"
            );
        }
    }

    if commands.get(name).is_some() {
        let mut ex: Vec<String> = commands
            .ex_names(name)
            .into_iter()
            .map(|ex| format!(":{ex}"))
            .collect();
        ex.push(format!(":{name}"));
        let _ = writeln!(page, "\nCommand line\n  {}", ex.join("  "));
    }

    let _ = writeln!(page, "\nKeys");
    let keys = keys_for(name, cx);
    if keys.is_empty() {
        let _ = writeln!(
            page,
            "  none: (bind 'normal \"<keys>\" \"{name}\") binds some"
        );
    }
    let width = keys.iter().map(|(table, _)| table.len()).max().unwrap_or(0);
    for (table, keys) in &keys {
        let _ = writeln!(page, "  {table:width$}  {keys}");
    }

    let _ = writeln!(page, "\n:help with no command lists them all");
    page
}

/// Every command with what it says of itself, then the vim engine's with
/// the keys normal mode binds to them.
fn index(cx: &App) -> String {
    let commands = cx.global::<Commands>().all();
    let mut engine: Vec<&str> = med_vim::command_names().collect();
    engine.sort();
    let width = commands
        .iter()
        .map(|command| command.name.len())
        .chain(engine.iter().map(|name| name.len()))
        .max()
        .unwrap_or(0);

    let mut page = String::new();
    let _ = writeln!(page, "Commands: :help <command> for one\n");
    for command in &commands {
        let _ = writeln!(page, "{:width$}  {}", command.name, command.description);
    }

    let keymap = &cx.global::<VimGlobal>().0.keymap;
    let _ = writeln!(page, "\nThe vim engine's, with their keys in normal mode\n");
    for name in engine {
        let keys: Vec<String> = keymap
            .keys_for(name)
            .into_iter()
            .filter(|found| found.table.is_none() && found.mode == MapMode::Normal)
            .map(|found| found.keys)
            .collect();
        let line = format!("{name:width$}  {}", keys.join(" "));
        let _ = writeln!(page, "{}", line.trim_end());
    }
    page
}
