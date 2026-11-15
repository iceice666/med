//! med — a modal editor built on GPUI.

pub mod commands;
pub mod defaults;
pub mod editor;
pub mod file_tree;
pub mod git_view;
pub mod keymap;
pub mod lsp;
pub mod panel;
pub mod picker;
pub mod syntax;
#[cfg(test)]
mod tests;
pub mod workspace;

use commands::RunCommand;
use gpui::*;
use std::path::PathBuf;
use workspace::Workspace;

#[derive(Clone)]
pub struct Settings {
    /// Editor font; the theme's monospace font when unset.
    pub font_family: Option<SharedString>,
    pub font_size: f32,
    /// Start language servers for opened files.
    pub lsp: bool,
}

impl Global for Settings {}

/// Vim state shared by every editor: registers, last search, last change.
pub struct VimGlobal(pub med_vim::Shared);

impl Global for VimGlobal {}

/// Work out the project root and the files to open from the command line:
/// `med` (current directory), `med dir`, or `med file...`.
fn parse_args() -> (PathBuf, Vec<PathBuf>) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
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
            .unwrap_or(cwd.clone())
    });
    (root, files)
}

/// The globals views read: vim state, commands, key tables, panel layouts.
/// Needs no window system, so headless code can stop here.
pub fn init_globals(cx: &mut App) {
    cx.set_global(VimGlobal(med_vim::Shared::default()));
    commands::init(cx);
    defaults::init(cx);
    panel::init(cx);
}

/// Globals and key bindings; everything a window needs before it opens.
pub fn init(cx: &mut App) {
    gpui::init(cx);
    // gpui-component starts out light; follow the system instead.
    gpui::component::Theme::sync_system_appearance(None, cx);
    picker::init(cx);
    cx.set_global(Settings {
        font_family: std::env::var("MED_FONT").ok().map(SharedString::from),
        font_size: std::env::var("MED_FONT_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(14.),
        lsp: std::env::var_os("MED_NO_LSP").is_none(),
    });
    init_globals(cx);
    defaults::bind_app_keys(cx);
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
            panel::load_user_layouts(cx);
            cx.activate(true);

            let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
            gpui::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("med".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| Workspace::new(root, files, window, cx)),
            )
            .expect("failed to open window");
        });
}
