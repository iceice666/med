//! What is bound out of the box: the tables a configuration will edit.
//!
//! The vim layout itself is [`med_vim::Keymap::vim`]; this adds the keys
//! that reach beyond the buffer, the panels' keys, the app shortcuts and
//! the ex command names. Every right-hand side is a command name from
//! [`Commands`] or the engine's [`med_vim::command_names`].

use crate::VimGlobal;
use crate::commands::{Commands, RunCommand};
use crate::keymap::PanelKeymaps;
use gpui::{App, KeyBinding};
use med_vim::MapMode::{self, Insert, Normal, Visual};

const NV: &[MapMode] = &[Normal, Visual];
const N: &[MapMode] = &[Normal];
const I: &[MapMode] = &[Insert];

/// Editor keys, on top of the vim layout: modes, key sequence, command.
pub const EDITOR_KEYS: &[(&[MapMode], &str, &str)] = &[
    (NV, "gd", "editor.goto_definition"),
    (NV, "gD", "editor.goto_definition"),
    (NV, "gr", "editor.references"),
    (NV, "gh", "editor.hover"),
    (NV, "K", "editor.hover"),
    (NV, "]d", "editor.next_diagnostic"),
    (NV, "[d", "editor.prev_diagnostic"),
    (NV, "gt", "workspace.next_tab"),
    (NV, "gT", "workspace.prev_tab"),
    (NV, "<C-o>", "workspace.jump_back"),
    (NV, "<C-i>", "workspace.jump_forward"),
    (NV, "<Tab>", "workspace.jump_forward"),
    (I, "<C-n>", "editor.complete"),
    (I, "<C-p>", "editor.complete"),
    (I, "<C-Space>", "editor.complete"),
    // Leader keys. `space` alone still moves right, after a pause.
    (N, "<Space>f", "workspace.find_file"),
    (N, "<Space><Space>", "workspace.find_file"),
    (N, "<Space>/", "workspace.search"),
    (N, "<Space>e", "workspace.focus_file_tree"),
    (N, "<Space>g", "workspace.toggle_git"),
    (N, "<Space>w", "workspace.save"),
    (N, "<Space>q", "workspace.close_tab"),
];

/// Keys of the file tree and the git screen: context, key sequence,
/// command. A git panel's own context is looked at before `git`.
pub const PANEL_KEYS: &[(&str, &str, &str)] = &[
    ("tree", "j", "tree.down"),
    ("tree", "<Down>", "tree.down"),
    ("tree", "k", "tree.up"),
    ("tree", "<Up>", "tree.up"),
    ("tree", "<C-d>", "tree.page_down"),
    ("tree", "<C-u>", "tree.page_up"),
    ("tree", "h", "tree.collapse"),
    ("tree", "<Left>", "tree.collapse"),
    ("tree", "l", "tree.expand"),
    ("tree", "<Right>", "tree.expand"),
    ("tree", "<CR>", "tree.open"),
    ("tree", "o", "tree.open"),
    ("tree", "gg", "tree.first"),
    ("tree", "G", "tree.last"),
    ("tree", "R", "tree.refresh"),
    ("tree", "<Esc>", "tree.close"),
    ("tree", "q", "tree.close"),
    // The same leader keys as in an editor. Not in the git screen: there
    // space stages.
    ("tree", "<Space>f", "workspace.find_file"),
    ("tree", "<Space>/", "workspace.search"),
    ("tree", "<Space>e", "workspace.focus_file_tree"),
    ("tree", "<Space>g", "workspace.toggle_git"),
    ("git", "q", "git.close"),
    ("git", "<Esc>", "git.close"),
    ("git", "1", "git.focus_status"),
    ("git", "2", "git.focus_files"),
    ("git", "3", "git.focus_branches"),
    ("git", "4", "git.focus_commits"),
    ("git", "5", "git.focus_stash"),
    ("git", "<Tab>", "git.next_panel"),
    ("git", "l", "git.next_panel"),
    ("git", "<Right>", "git.next_panel"),
    ("git", "<S-Tab>", "git.prev_panel"),
    ("git", "h", "git.prev_panel"),
    ("git", "<Left>", "git.prev_panel"),
    ("git", "j", "git.down"),
    ("git", "<Down>", "git.down"),
    ("git", "k", "git.up"),
    ("git", "<Up>", "git.up"),
    ("git", "<lt>", "git.first"),
    ("git", ">", "git.last"),
    ("git", "J", "git.preview_down"),
    ("git", "<PageDown>", "git.preview_down"),
    ("git", "K", "git.preview_up"),
    ("git", "<PageUp>", "git.preview_up"),
    ("git", "<C-d>", "git.preview_page_down"),
    ("git", "<C-u>", "git.preview_page_up"),
    ("git", "R", "git.refresh"),
    ("git", "p", "git.pull"),
    ("git", "P", "git.push"),
    ("git", "f", "git.fetch"),
    ("git.files", "<Space>", "git.toggle_stage"),
    ("git.files", "a", "git.toggle_stage_all"),
    ("git.files", "c", "git.commit"),
    ("git.files", "A", "git.amend"),
    ("git.files", "d", "git.discard"),
    ("git.files", "s", "git.stash"),
    ("git.files", "<CR>", "git.open_file"),
    ("git.files", "e", "git.open_file"),
    ("git.files", "o", "git.open_file"),
    ("git.branches", "<Space>", "git.checkout"),
    ("git.branches", "<CR>", "git.checkout"),
    ("git.branches", "n", "git.new_branch"),
    ("git.branches", "d", "git.delete_branch"),
    ("git.stash", "<Space>", "git.stash_apply"),
    ("git.stash", "g", "git.stash_pop"),
    ("git.stash", "d", "git.stash_drop"),
];

/// Shortcuts handled by the window system rather than a view's key table:
/// GPUI keystroke, GPUI key context, command.
pub const APP_KEYS: &[(&str, Option<&str>, &str)] = &[
    ("cmd-q", None, "workspace.quit_all"),
    ("cmd-s", Some("Workspace"), "workspace.save"),
    ("cmd-w", Some("Workspace"), "workspace.close_tab"),
    ("cmd-shift-]", Some("Workspace"), "workspace.next_tab"),
    ("cmd-shift-[", Some("Workspace"), "workspace.prev_tab"),
    ("ctrl-tab", Some("Workspace"), "workspace.next_tab"),
    ("ctrl-shift-tab", Some("Workspace"), "workspace.prev_tab"),
    ("cmd-b", Some("Workspace"), "workspace.toggle_file_tree"),
    (
        "cmd-shift-e",
        Some("Workspace"),
        "workspace.focus_file_tree",
    ),
    ("cmd-p", Some("Workspace"), "workspace.find_file"),
    ("cmd-shift-f", Some("Workspace"), "workspace.search"),
    ("cmd-shift-g", Some("Workspace"), "workspace.toggle_git"),
    ("cmd-v", Some("Editor"), "editor.paste"),
    ("cmd-c", Some("Editor"), "editor.copy"),
];

/// Ex command names: what can be typed after `:`, and the command it runs.
/// (Line ranges, `:s`, `:d`, `:y`, `:>`, `:<` and `:noh` are the engine's.)
pub const EX: &[(&[&str], &str)] = &[
    (&["w", "write"], "workspace.save"),
    (&["q", "quit"], "workspace.quit"),
    (&["bd", "bdelete"], "workspace.close_tab"),
    (&["wq", "x", "xit"], "workspace.save_and_quit"),
    (&["wa", "wall"], "workspace.save_all"),
    (&["qa", "qall"], "workspace.quit_all"),
    (&["e", "edit"], "workspace.edit"),
    (&["bn", "bnext", "tabn", "tabnext"], "workspace.next_tab"),
    (
        &["bp", "bprev", "bprevious", "tabp", "tabprevious"],
        "workspace.prev_tab",
    ),
];

/// Load the tables above into the globals they configure.
pub fn init(cx: &mut App) {
    let keymap = &mut cx.global_mut::<VimGlobal>().0.keymap;
    for (modes, keys, command) in EDITOR_KEYS {
        for mode in *modes {
            keymap.bind(*mode, keys, command);
        }
    }

    let mut panels = PanelKeymaps::default();
    for (context, keys, command) in PANEL_KEYS {
        panels.bind(context, keys, command);
    }
    cx.set_global(panels);

    let commands = cx.global_mut::<Commands>();
    for (names, command) in EX {
        commands.ex(names, command);
    }
}

pub fn bind_app_keys(cx: &mut App) {
    cx.bind_keys(APP_KEYS.iter().map(|(keystroke, context, command)| {
        KeyBinding::new(keystroke, RunCommand((*command).into()), *context)
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn every_default_names_a_command(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            crate::init_globals(cx);
            let registry = cx.global::<Commands>();
            let known = |command: &str| {
                registry.get(command).is_some() || med_vim::command_names().any(|c| c == command)
            };
            let editor = EDITOR_KEYS
                .iter()
                .map(|(_, keys, command)| (*keys, *command));
            let panels = PANEL_KEYS
                .iter()
                .map(|(_, keys, command)| (*keys, *command));
            let app = APP_KEYS.iter().map(|(keys, _, command)| (*keys, *command));
            let ex = EX.iter().map(|(names, command)| (names[0], *command));
            for (keys, command) in editor.chain(panels).chain(app).chain(ex) {
                assert!(
                    known(command),
                    "{keys} is bound to unknown command {command}"
                );
            }

            // And the other way round: nothing registered is unreachable
            // from the command line.
            for command in registry.all() {
                assert_eq!(
                    registry.resolve_ex(&command.name),
                    Some(command.name.clone())
                );
                assert!(!command.description.is_empty(), "{}", command.name);
            }
        });
    }
}
