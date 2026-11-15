//! med — a modal editor built on GPUI.

pub mod buffer_api;
mod cmdline;
pub mod commands;
pub mod defaults;
pub mod editor;
pub mod help;
pub mod host;
pub mod keymap;
pub mod lsp;
pub mod panel;
pub mod script;
pub mod search;
pub mod syntax;
#[cfg(test)]
mod tests;
pub mod user_panel;
pub mod workspace;

use commands::RunCommand;
use gpui::component::TitleBar;
use gpui::*;
use std::path::{Path, PathBuf};
use std::time::Duration;
use workspace::Workspace;

#[derive(Clone)]
pub struct Settings {
    /// Editor font; the theme's monospace font when unset.
    pub font_family: Option<SharedString>,
    pub font_size: f32,
    /// Start language servers for opened files.
    pub lsp: bool,
    /// How long a key that is a binding by itself waits to become a longer
    /// one (`space` before `space f`), vim's `timeoutlen`; `None` to wait
    /// for the next key however long it takes.
    pub key_timeout: Option<Duration>,
    /// Have the language server format a file before it is written.
    pub format_on_save: bool,
}

impl Settings {
    pub const KEY_TIMEOUT: Duration = Duration::from_millis(1000);
}

impl Global for Settings {}

/// Vim state shared by every editor: registers, last search, last change.
pub struct VimGlobal(pub med_vim::Shared);

impl Global for VimGlobal {}

/// Work out the project root and the files to open from the command line:
/// `med` (current directory), `med dir`, or `med file...`.
fn parse_args() -> (PathBuf, Vec<PathBuf>) {
    let cwd = launch_dir();
    let absolute = |path: PathBuf| {
        let path = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
        path.canonicalize().unwrap_or(path)
    };
    let mut root = None;
    let mut files = Vec::new();
    for arg in std::env::args_os().skip(1).map(PathBuf::from).map(absolute) {
        if arg.is_dir() {
            root.get_or_insert(arg);
        } else {
            files.push(arg);
        }
    }
    let root = root.unwrap_or_else(|| {
        // For a lone file, the repository it lives in makes a better root
        // than wherever the shell happened to be.
        files
            .first()
            .and_then(|file| med_git::Repo::discover(file))
            .map(|repo| repo.root)
            .or_else(|| files.first().and_then(|f| f.parent().map(PathBuf::from)))
            .unwrap_or_else(|| default_root(&cwd))
    });
    (root, files)
}

/// The directory med was started from. An AppImage's launcher changes into
/// the image's own `usr/` before running med, leaving the real one in
/// `$OWD`; go back there so relative arguments, and anything med runs,
/// see the directory the user did.
fn launch_dir() -> PathBuf {
    let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    if var("APPIMAGE").is_some()
        && let Some(owd) = var("OWD")
        && std::env::set_current_dir(&owd).is_ok()
    {
        return PathBuf::from(owd);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// The root when the command line names none: the current directory, unless
/// that is `/`, which is where Finder and `open` start an app bundle. Home
/// is what someone double-clicking med.app means; `med /` still opens `/`.
fn default_root(cwd: &Path) -> PathBuf {
    if cwd.parent().is_none()
        && let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty())
    {
        return PathBuf::from(home);
    }
    cwd.to_path_buf()
}

/// Where the user's files are: `$MED_CONFIG_DIR`, `$XDG_CONFIG_HOME/med` or
/// `~/.config/med`.
pub fn config_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    if let Some(dir) = var("MED_CONFIG_DIR") {
        Some(PathBuf::from(dir))
    } else if let Some(dir) = var("XDG_CONFIG_HOME") {
        Some(PathBuf::from(dir).join("med"))
    } else {
        Some(PathBuf::from(var("HOME")?).join(".config").join("med"))
    }
}

/// The globals views read: vim state, commands, key tables, panels, the
/// script interpreter. Needs no window system, so headless code can
/// stop here.
pub fn init_globals(cx: &mut App) {
    let mut vim = med_vim::Shared::default();
    vim.auto_pairs = true;
    cx.set_global(VimGlobal(vim));
    commands::init(cx);
    defaults::init(cx);
    panel::init(cx);
    user_panel::init(cx);
    script::init(cx);
}

/// Globals and key bindings; everything a window needs before it opens.
pub fn init(cx: &mut App) {
    gpui::init(cx);
    // gpui-component starts out light; follow the system instead.
    gpui::component::Theme::sync_system_appearance(None, cx);
    cx.set_global(Settings {
        font_family: std::env::var("MED_FONT").ok().map(SharedString::from),
        font_size: std::env::var("MED_FONT_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(14.),
        lsp: std::env::var_os("MED_NO_LSP").is_none(),
        key_timeout: Some(Settings::KEY_TIMEOUT),
        format_on_save: false,
    });
    init_globals(cx);
    // With no window to ask about unsaved work, quitting just quits.
    cx.on_action(|action: &RunCommand, cx| {
        if action.0 == "workspace.quit_all" {
            cx.quit();
        }
    });
}

/// Run the editor: parse the command line, open the window, block until quit.
pub fn run() {
    let (root, files) = parse_args();
    gpui::application()
        .with_assets(gpui::assets::Assets)
        .run(move |cx| {
            init(cx);
            // Not in `init`: tests must not depend on what is in $HOME.
            script::load_init(cx);
            cx.activate(true);

            let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
            gpui::open_window(
                // The title bar is drawn by the workspace, which a program
                // can give a layout of its own (`defpanel :at 'title`).
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("med".into()),
                        ..TitleBar::title_bar_options()
                    }),
                    ..TitleBar::window_options()
                },
                cx,
                |window, cx| cx.new(|cx| Workspace::new(root, files, window, cx)),
            )
            .expect("failed to open window");
        });
}
