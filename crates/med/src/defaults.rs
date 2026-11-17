//! What is built in: the names ex commands go by, and no keys.
//!
//! Every key, vim's included, is bound by the init file: the vim layout
//! is `examples/vim.mlisp`, and the keys that reach beyond the buffer, the
//! ones of flash and the app shortcuts are `examples/keys.mlisp`. A panel's
//! keys, and the keys that bring it up, are bound by the program that
//! defines the panel. Those files are not in the binary: their names are,
//! for `:install_default_config` to download them from GitHub, at the tag
//! of this version, and write them where an init file is looked for.

use crate::VimGlobal;
use crate::commands::Commands;
use crate::github::GitHubFile;
use crate::keymap::PanelKeymaps;
use gpui::App;
use med_vim::MapMode;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Ex command names: what can be typed after `:`, and the command it runs.
/// (Line ranges, `:s`, `:d`, `:y`, `:>`, `:<` and `:noh` are the engine's.)
pub const EX: &[(&[&str], &str)] = &[
    (&["w", "write"], "workspace.save"),
    (&["q", "quit"], "workspace.quit"),
    (&["bd", "bdelete"], "workspace.close_tab"),
    (&["wq", "x", "xit"], "workspace.save_and_quit"),
    (&["wa", "wall"], "workspace.save_all"),
    (&["qa", "qall"], "workspace.quit_window"),
    (&["mks", "mksession"], "workspace.save_session"),
    (&["project"], "workspace.open_project"),
    (&["e", "edit"], "workspace.edit"),
    (&["eval"], "workspace.eval"),
    (&["h", "help"], "workspace.help"),
    (
        &["install_default_config"],
        "workspace.install_default_config",
    ),
    (&["rename"], "editor.rename"),
    (&["format", "fmt"], "editor.format"),
    (&["preview"], "workspace.markdown_preview"),
    (&["term", "terminal"], "workspace.terminal"),
    (&["vs", "vsplit"], "workspace.split_right"),
    (&["sp", "split"], "workspace.split_down"),
    (&["clo", "close"], "workspace.close_pane"),
    (&["on", "only"], "workspace.only_pane"),
    (&["bn", "bnext", "tabn", "tabnext"], "workspace.next_tab"),
    (
        &["bp", "bprev", "bprevious", "tabp", "tabprevious"],
        "workspace.prev_tab",
    ),
    (&["b#"], "workspace.alternate_tab"),
    (&["tabnew", "tabe", "tabedit", "enew"], "workspace.new_tab"),
    (&["tabo", "tabonly"], "workspace.close_other_tabs"),
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
    ("workspace.new_tab", ExArg::File),
    ("workspace.open_project", ExArg::File),
    ("workspace.help", ExArg::Command),
];

/// The configuration `:install_default_config` writes: every file of
/// `examples/`, by its name there, `init.mlisp` loading the others, and
/// the theme files of `examples/themes/`. Only the names are in the
/// binary; the files are downloaded from [`REPOSITORY`].
pub const DEFAULT_CONFIG: &[&str] = &[
    "init.mlisp",
    "vim.mlisp",
    "keys.mlisp",
    "oil.mlisp",
    "tree.mlisp",
    "picker.mlisp",
    "structural.mlisp",
    "git.mlisp",
    "which-key.mlisp",
    "todo.mlisp",
    "dired.mlisp",
    "statusline.mlisp",
    "titlebar.mlisp",
    "rss.mlisp",
    "themes/catppuccin.json",
];

/// The GitHub repository [`DEFAULT_CONFIG`] is downloaded from, the files
/// in its `examples/`.
pub const REPOSITORY: &str = "iceice666/med";

/// The ref the examples are downloaded at unless another is given: the
/// tag of this version, so they are the ones it was made with.
pub fn release_ref() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// Where the files of [`DEFAULT_CONFIG`] come from.
#[derive(Clone, Debug)]
pub enum Source {
    /// [`REPOSITORY`] at this ref.
    GitHub(String),
    /// A directory laid out as `examples/` is, for the tests.
    #[cfg(test)]
    Dir(PathBuf),
}

/// Why the default configuration was not written.
#[derive(Debug)]
pub enum InstallError {
    /// These files are there already, and were not to be overwritten.
    Exists(Vec<&'static str>),
    Io(PathBuf, io::Error),
    /// A file could not be downloaded: what went wrong.
    Download(String),
}

/// Unless `overwrite`, the files of [`DEFAULT_CONFIG`] that are in `dir`
/// already, by their name or by the `.lisp` one they had before.
pub fn check_existing(dir: &Path, overwrite: bool) -> Result<(), InstallError> {
    if overwrite {
        return Ok(());
    }
    let there: Vec<&str> = DEFAULT_CONFIG
        .iter()
        .copied()
        .filter(|name| {
            let path = dir.join(name);
            path.exists() || path.with_extension("lisp").exists()
        })
        .collect();
    if there.is_empty() {
        Ok(())
    } else {
        Err(InstallError::Exists(there))
    }
}

/// The text of every file of [`DEFAULT_CONFIG`], in its order. They are
/// downloaded all at once, with `curl`, and it is all of them or an error.
/// This waits on the network: keep it off the main thread.
pub fn fetch(source: &Source) -> Result<Vec<(&'static str, String)>, InstallError> {
    match source {
        Source::GitHub(git_ref) => download(git_ref),
        #[cfg(test)]
        Source::Dir(from) => DEFAULT_CONFIG
            .iter()
            .map(|name| {
                let path = from.join(name);
                std::fs::read_to_string(&path)
                    .map(|text| (*name, text))
                    .map_err(|error| InstallError::Io(path, error))
            })
            .collect(),
    }
}

/// Every file of [`DEFAULT_CONFIG`] from [`REPOSITORY`] at `git_ref`, one
/// `curl` each, at the same time, by way of a directory of their own.
fn download(git_ref: &str) -> Result<Vec<(&'static str, String)>, InstallError> {
    let files = DEFAULT_CONFIG
        .iter()
        .map(|name| {
            GitHubFile::parse(&format!("{REPOSITORY}/examples/{name}@{git_ref}"))
                .map(|file| (*name, file))
                .map_err(InstallError::Download)
        })
        .collect::<Result<Vec<_>, _>>()?;

    static DOWNLOADS: AtomicUsize = AtomicUsize::new(0);
    let staging = std::env::temp_dir().join(format!(
        "med-default-config-{}-{}",
        std::process::id(),
        DOWNLOADS.fetch_add(1, Ordering::Relaxed)
    ));
    let fetched = std::thread::scope(|scope| {
        let downloads: Vec<_> = files
            .iter()
            .map(|(name, file)| {
                let to = staging.join(name);
                scope.spawn(move || {
                    file.fetch(&to).map_err(InstallError::Download)?;
                    std::fs::read_to_string(&to)
                        .map(|text| (*name, text))
                        .map_err(|error| InstallError::Io(to, error))
                })
            })
            .collect();
        downloads
            .into_iter()
            .map(|download| download.join().expect("a download panicked"))
            .collect::<Result<Vec<_>, _>>()
    });
    let _ = std::fs::remove_dir_all(&staging);
    fetched
}

/// Write `files` into `dir`, made if it is not there, over what is there
/// by those names. Other files in `dir` are left as they are.
pub fn write(dir: &Path, files: &[(&str, String)]) -> Result<(), InstallError> {
    std::fs::create_dir_all(dir).map_err(|error| InstallError::Io(dir.to_path_buf(), error))?;
    for (name, text) in files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| InstallError::Io(parent.to_path_buf(), error))?;
        }
        std::fs::write(&path, text).map_err(|error| InstallError::Io(path, error))?;
    }
    Ok(())
}

/// Write [`DEFAULT_CONFIG`] from `source` into `dir`. Unless `overwrite`,
/// nothing is written while any of its files is there already; nothing is
/// written either unless every file could be had. This waits on the
/// network: keep it off the main thread.
pub fn install_default_config(
    dir: &Path,
    source: &Source,
    overwrite: bool,
) -> Result<(), InstallError> {
    check_existing(dir, overwrite)?;
    let files = fetch(source)?;
    write(dir, &files)
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

/// Why no key is bound, for the welcome popup: there is no init file, or
/// the one there binds none. `None` while keys are bound.
pub fn no_keys_reason(cx: &App) -> Option<String> {
    let keymap = &cx.global::<VimGlobal>().0.keymap;
    if !keymap.is_empty(MapMode::Normal) {
        return None;
    }
    let Some(dir) = crate::config_dir() else {
        return Some("there is no configuration directory: $HOME is not set.".to_string());
    };
    let init = ["init.mlisp", "init.lisp"]
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file());
    Some(match init {
        Some(path) => format!("{} binds none in normal mode.", path.display()),
        None => format!("there is no init.mlisp in {}.", dir.display()),
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
        let files = |dir: &str, ext: &str| -> Vec<String> {
            std::fs::read_dir(examples.join(dir))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with(ext))
                .map(|name| Path::new(dir).join(name).to_string_lossy().into_owned())
                .collect()
        };
        let mut names = files("", ".mlisp");
        names.extend(files("themes", ".json"));
        names.sort();
        let mut installed: Vec<&str> = DEFAULT_CONFIG.to_vec();
        installed.sort();
        assert_eq!(names, installed);

        let dir = std::env::temp_dir().join(format!("med-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config = dir.join("med");
        // An init file by the name it had before counts as being there.
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("init.lisp"), "; mine\n").unwrap();
        let source = Source::Dir(examples.clone());
        match install_default_config(&config, &source, false) {
            Err(InstallError::Exists(there)) => assert_eq!(there, ["init.mlisp"]),
            other => panic!("{other:?}"),
        }
        std::fs::remove_file(config.join("init.lisp")).unwrap();
        install_default_config(&config, &source, false).unwrap();
        for name in DEFAULT_CONFIG {
            assert_eq!(
                std::fs::read_to_string(config.join(name)).unwrap(),
                std::fs::read_to_string(examples.join(name)).unwrap()
            );
        }

        // What is there already is kept, unless it is to be overwritten;
        // what is not one of them is kept either way.
        std::fs::write(config.join("keys.mlisp"), "; mine\n").unwrap();
        std::fs::write(config.join("notes.mlisp"), "; mine\n").unwrap();
        match install_default_config(&config, &source, false) {
            Err(InstallError::Exists(there)) => assert_eq!(there.len(), DEFAULT_CONFIG.len()),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(config.join("keys.mlisp")).unwrap(),
            "; mine\n"
        );
        install_default_config(&config, &source, true).unwrap();
        assert_ne!(
            std::fs::read_to_string(config.join("keys.mlisp")).unwrap(),
            "; mine\n"
        );
        assert!(config.join("notes.mlisp").is_file());

        cx.update(|cx| {
            crate::init(cx);
            assert!(no_keys_hint(cx).is_some());
            assert!(crate::script::run_init(&config.join("init.mlisp"), cx));
            assert_eq!(crate::script::take_message(cx), None);
            assert_eq!(no_keys_hint(cx), None);
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The files are downloaded from med's repository, at the tag of this
    /// version, and nothing is written when one cannot be had.
    #[test]
    fn the_examples_are_downloaded_at_this_version() {
        assert_eq!(release_ref(), concat!("v", env!("CARGO_PKG_VERSION")));
        let file = GitHubFile::parse(&format!(
            "{REPOSITORY}/examples/{}@{}",
            DEFAULT_CONFIG[0],
            release_ref()
        ))
        .unwrap();
        assert_eq!(
            file.to_string(),
            format!("gh:iceice666/med/examples/init.mlisp@{}", release_ref())
        );

        let dir = std::env::temp_dir().join(format!("med-install-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let source = Source::GitHub("a b".to_string());
        match install_default_config(&dir, &source, false) {
            Err(InstallError::Download(why)) => assert!(why.contains("is not a ref"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(!dir.exists());
    }

    /// What `:install_default_config` downloads is there at this version's
    /// tag. Needs the network and a released version:
    /// `cargo test -p med -- --ignored downloads`.
    #[test]
    #[ignore = "downloads from GitHub"]
    fn downloads_the_examples() {
        let files = fetch(&Source::GitHub(release_ref())).unwrap();
        assert_eq!(files.len(), DEFAULT_CONFIG.len());
        for (name, text) in files {
            assert!(!text.is_empty(), "{name}");
        }
    }

    /// `examples/vim.mlisp` is the engine's stock layout, binding for
    /// binding.
    #[gpui::test]
    fn the_example_vim_layout_is_the_engines(cx: &mut gpui::TestAppContext) {
        let source = include_str!("../../../examples/vim.mlisp");
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
