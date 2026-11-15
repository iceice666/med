//! What is built in: the names ex commands go by, and no keys.
//!
//! Every key, vim's included, is bound by the init file: the vim layout
//! is `examples/vim.lisp`, and the keys that reach beyond the buffer, the
//! ones of flash and the app shortcuts are `examples/keys.lisp`. A panel's
//! keys, and the keys that bring it up, are bound by the program that
//! defines the panel. Those files are in the binary as well, for
//! `:install_default_config` to write where an init file is looked for.

use crate::VimGlobal;
use crate::commands::Commands;
use crate::keymap::PanelKeymaps;
use gpui::App;
use med_vim::MapMode;
use std::io;
use std::path::{Path, PathBuf};

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
    (&["h", "help"], "workspace.help"),
    (
        &["install_default_config"],
        "workspace.install_default_config",
    ),
    (&["rename"], "editor.rename"),
    (&["format", "fmt"], "editor.format"),
    (&["bn", "bnext", "tabn", "tabnext"], "workspace.next_tab"),
    (
        &["bp", "bprev", "bprevious", "tabp", "tabprevious"],
        "workspace.prev_tab",
    ),
];

/// What the argument of a command is, for `Tab` to complete it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExArg {
    /// A path, relative to the project's root.
    File,
    /// A command, by any of its names.
    Command,
}

/// The commands whose argument `Tab` completes on the command line.
pub const EX_ARGS: &[(&str, ExArg)] = &[
    ("workspace.edit", ExArg::File),
    ("workspace.save", ExArg::File),
    ("workspace.help", ExArg::Command),
];

/// The configuration `:install_default_config` writes: every file of
/// `examples/`, by its name there, `init.lisp` loading the others.
pub const DEFAULT_CONFIG: &[(&str, &str)] = &[
    ("init.lisp", include_str!("../../../examples/init.lisp")),
    ("vim.lisp", include_str!("../../../examples/vim.lisp")),
    ("keys.lisp", include_str!("../../../examples/keys.lisp")),
    ("tree.lisp", include_str!("../../../examples/tree.lisp")),
    ("picker.lisp", include_str!("../../../examples/picker.lisp")),
    ("git.lisp", include_str!("../../../examples/git.lisp")),
    (
        "which-key.lisp",
        include_str!("../../../examples/which-key.lisp"),
    ),
    ("todo.lisp", include_str!("../../../examples/todo.lisp")),
    ("dired.lisp", include_str!("../../../examples/dired.lisp")),
    (
        "statusline.lisp",
        include_str!("../../../examples/statusline.lisp"),
    ),
    (
        "titlebar.lisp",
        include_str!("../../../examples/titlebar.lisp"),
    ),
];

/// Why the default configuration was not written.
#[derive(Debug)]
pub enum InstallError {
    /// These files are there already, and were not to be overwritten.
    Exists(Vec<&'static str>),
    Io(PathBuf, io::Error),
}

/// Write [`DEFAULT_CONFIG`] into `dir`, made if it is not there. Unless
/// `overwrite`, nothing is written while any of its files is there
/// already. Other files in `dir` are left as they are.
pub fn install_default_config(dir: &Path, overwrite: bool) -> Result<(), InstallError> {
    if !overwrite {
        let there: Vec<&str> = DEFAULT_CONFIG
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| dir.join(name).exists())
            .collect();
        if !there.is_empty() {
            return Err(InstallError::Exists(there));
        }
    }
    std::fs::create_dir_all(dir).map_err(|error| InstallError::Io(dir.to_path_buf(), error))?;
    for (name, text) in DEFAULT_CONFIG {
        let path = dir.join(name);
        std::fs::write(&path, text).map_err(|error| InstallError::Io(path, error))?;
    }
    Ok(())
}

/// What to say while nothing is bound in normal mode: that an editor with
/// no keys is not broken, and how to give it some. `:` works then, and
/// only then.
pub fn no_keys_hint(cx: &App) -> Option<String> {
    let keymap = &cx.global::<VimGlobal>().0.keymap;
    keymap.is_empty(MapMode::Normal).then(|| {
        let at = crate::config_dir()
            .map(|dir| format!(" to {}", dir.display()))
            .unwrap_or_default();
        format!("No key is bound: :install_default_config writes the example configuration{at}")
    })
}

/// Load the table above, and start the panels' key tables empty.
pub fn init(cx: &mut App) {
    cx.set_global(PanelKeymaps::default());

    let commands = cx.global_mut::<Commands>();
    for (names, command) in EX {
        commands.ex(names, command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use med_vim::MapMode::{Insert, Normal, Operator, Visual};

    #[gpui::test]
    fn every_ex_name_names_a_command(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            crate::init_globals(cx);
            let registry = cx.global::<Commands>();
            for (names, command) in EX {
                assert!(
                    registry.get(command).is_some(),
                    ":{} is the name of unknown command {command}",
                    names[0]
                );
            }

            for (command, _) in EX_ARGS {
                assert!(registry.get(command).is_some(), "{command}");
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

    #[gpui::test]
    fn no_key_is_bound_without_an_init_file(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            crate::init_globals(cx);
            let keymap = &cx.global::<crate::VimGlobal>().0.keymap;
            for mode in [Normal, Visual, Operator, Insert] {
                assert_eq!(keymap.next_keys(mode, ""), [], "{mode:?}");
            }
        });
    }

    /// Every file of `examples/` is installed, and runs from where it is
    /// written without an error.
    #[gpui::test]
    fn installs_the_examples(cx: &mut gpui::TestAppContext) {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let mut names: Vec<String> = std::fs::read_dir(&examples)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".lisp"))
            .collect();
        names.sort();
        let mut installed: Vec<&str> = DEFAULT_CONFIG.iter().map(|(name, _)| *name).collect();
        installed.sort();
        assert_eq!(names, installed);

        let dir = std::env::temp_dir().join(format!("med-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config = dir.join("med");
        install_default_config(&config, false).unwrap();
        for (name, text) in DEFAULT_CONFIG {
            assert_eq!(std::fs::read_to_string(config.join(name)).unwrap(), *text);
        }

        // What is there already is kept, unless it is to be overwritten;
        // what is not one of them is kept either way.
        std::fs::write(config.join("keys.lisp"), "; mine\n").unwrap();
        std::fs::write(config.join("notes.lisp"), "; mine\n").unwrap();
        match install_default_config(&config, false) {
            Err(InstallError::Exists(there)) => assert_eq!(there.len(), DEFAULT_CONFIG.len()),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(config.join("keys.lisp")).unwrap(),
            "; mine\n"
        );
        install_default_config(&config, true).unwrap();
        assert_ne!(
            std::fs::read_to_string(config.join("keys.lisp")).unwrap(),
            "; mine\n"
        );
        assert!(config.join("notes.lisp").is_file());

        cx.update(|cx| {
            crate::init(cx);
            assert!(no_keys_hint(cx).is_some());
            assert!(crate::script::run_init(&config.join("init.lisp"), cx));
            assert_eq!(crate::script::take_message(cx), None);
            assert_eq!(no_keys_hint(cx), None);
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `examples/vim.lisp` is the engine's stock layout, binding for
    /// binding.
    #[gpui::test]
    fn the_example_vim_layout_is_the_engines(cx: &mut gpui::TestAppContext) {
        let source = include_str!("../../../examples/vim.lisp");
        cx.update(|cx| {
            crate::init_globals(cx);
            if let Err(error) = crate::script::eval(source, cx) {
                panic!("{error}");
            }
            let keymap = &cx.global::<crate::VimGlobal>().0.keymap;
            for (modes, keys, command) in med_vim::VIM_LAYOUT {
                for mode in *modes {
                    assert_eq!(
                        keymap.command(*mode, keys),
                        Some(*command),
                        "{mode:?} {keys}"
                    );
                }
            }
        });
        let binds = source
            .lines()
            .filter(|line| line.starts_with("(bind "))
            .count();
        assert_eq!(binds, med_vim::VIM_LAYOUT.len());
    }
}
