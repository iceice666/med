//! What is bound out of the box: the tables a configuration will edit.
//!
//! The vim layout itself is [`med_vim::Keymap::vim`]; this adds the keys
//! that reach beyond the buffer, the ones of flash (`s`, `S`, `f`…), the
//! app shortcuts and the ex command names. Every right-hand side is a command name from [`Commands`] or the
//! engine's [`med_vim::command_names`]. A panel's keys, and the keys that
//! bring it up, are bound by the program that defines the panel.

use crate::VimGlobal;
use crate::commands::{Commands, RunCommand};
use crate::keymap::PanelKeymaps;
use gpui::{App, KeyBinding};
use med_vim::MapMode::{self, Insert, Normal, Operator, Visual};

const NVO: &[MapMode] = &[Normal, Visual, Operator];
const NV: &[MapMode] = &[Normal, Visual];
const NO: &[MapMode] = &[Normal, Operator];
const N: &[MapMode] = &[Normal];
const O: &[MapMode] = &[Operator];
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
    (I, "<C-s>", "editor.signature_help"),
    // To text on screen, by typing some of it and then its label. In place
    // of vim's `s`, which `cl` and `c` also are; after an operator `s` is
    // surround's, so there it is `z`.
    (NV, "s", "motion.flash"),
    (O, "z", "motion.flash"),
    // The same for an operator to work there, and come back.
    (O, "r", "motion.flash_remote"),
    // A syntax node around the cursor, by its label. In place of vim's
    // `S`, which `cc` also is; on a selection `S` is surround's.
    (NO, "S", "select.flash"),
    // `f` and `t` on every line on screen, with where they could go next
    // marked, and their key going on from there.
    (NVO, "f", "motion.flash_find_forward"),
    (NVO, "F", "motion.flash_find_backward"),
    (NVO, "t", "motion.flash_till_forward"),
    (NVO, "T", "motion.flash_till_backward"),
    (NV, "<Space>a", "editor.code_action"),
    (N, "<Space>=", "editor.format"),
    // Leader keys. `space` alone still moves right, after a pause.
    (N, "<Space>w", "workspace.save"),
    (N, "<Space>q", "workspace.close_tab"),
];

/// Editor keys that stand for other keys: modes, key sequence, the keys.
pub const EDITOR_KEYS_FOR_KEYS: &[(&[MapMode], &str, &str)] = &[
    // The new name is typed on the command line.
    (N, "<Space>r", ":rename<Space>"),
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
    (&["eval"], "workspace.eval"),
    (&["rename"], "editor.rename"),
    (&["format", "fmt"], "editor.format"),
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
    for (modes, keys, to) in EDITOR_KEYS_FOR_KEYS {
        for mode in *modes {
            keymap.bind_keys(*mode, keys, to);
        }
    }

    cx.set_global(PanelKeymaps::default());

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
            let app = APP_KEYS.iter().map(|(keys, _, command)| (*keys, *command));
            let ex = EX.iter().map(|(names, command)| (names[0], *command));
            for (keys, command) in editor.chain(app).chain(ex) {
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
