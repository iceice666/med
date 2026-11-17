//! med — a modal editor built on GPUI.

pub mod buffer_api;
mod cmdline;
pub mod commands;
pub mod defaults;
pub mod editor;
pub mod env;
mod github;
pub mod help;
pub mod host;
pub mod keymap;
pub mod lsp;
pub mod lsp_config;
mod pane;
pub mod panel;
mod projects;
pub mod script;
pub mod search;
mod session;
pub mod snippet;
pub mod structural;
pub mod syntax;
pub mod terminal;
#[cfg(test)]
mod tests;
pub mod theme;
pub mod undo_file;
pub mod user_panel;
pub mod workspace;

use commands::RunCommand;
use gpui::component::{ActiveTheme as _, TitleBar};
use gpui::*;
use std::path::{Path, PathBuf};
use std::time::Duration;
use workspace::Workspace;

#[derive(Clone)]
pub struct Settings {
    /// Editor font; the theme's monospace font when unset.
    pub font_family: Option<SharedString>,
    /// The size of the text in a file: the editor's, the terminal's where
    /// it has none of its own, and of what shows a file's text, its
    /// Markdown preview and the popups over it.
    pub font_size: f32,
    /// The size of the rest: tabs, bars, panels, the window's chrome, and
    /// what is laid out by it; the theme's when unset.
    pub ui_font_size: Option<f32>,
    /// The height of a row of text, as a multiple of the font size.
    pub line_height: f32,
    /// How heavy the text is drawn; bold text is drawn at 700 whatever this is.
    pub font_weight: FontWeight,
    /// OpenType features, as a tag and its value: `("ss01", 1)` for a
    /// stylistic set, `("calt", 0)` to turn ligatures off.
    pub font_features: Vec<(String, u32)>,
    /// Ligatures (`calt` and `liga`); off, they are turned off over what
    /// `font_features` says of them.
    pub ligatures: bool,
    /// Fonts to look in, in turn, for the characters the font has not got:
    /// CJK, symbols.
    pub font_fallbacks: Vec<SharedString>,
    /// The terminal's font; the editor's when unset.
    pub terminal_font_family: Option<SharedString>,
    /// The terminal's font size; the editor's when unset.
    pub terminal_font_size: Option<f32>,
    /// The height of a row in the terminal, as `line_height`.
    pub terminal_line_height: f32,
    /// Start language servers for opened files.
    pub lsp: bool,
    /// How long a key that is a binding by itself waits to become a longer
    /// one (`space` before `space f`), vim's `timeoutlen`; `None` to wait
    /// for the next key however long it takes.
    pub key_timeout: Option<Duration>,
    /// Have the language server format a file before it is written.
    pub format_on_save: bool,
    /// Columns to an indent and a tab stop, over what each file's language
    /// and its own indentation say; `None` to go by those.
    pub tab_width: Option<usize>,
    /// Number the lines by how far each is from the cursor's, as vim's
    /// `relativenumber` with `number`: the cursor's line keeps its own.
    pub relative_number: bool,
    /// Lines kept between the cursor and the top and bottom of the view,
    /// vim's `scrolloff`; as many as fit where the view is shorter.
    pub scroll_off: usize,
    /// Mark the cursor's line, vim's `cursorline`.
    pub cursor_line: bool,
    /// Display columns to mark down the view, counted from 1, vim's
    /// `colorcolumn`: a line not to write past.
    pub color_columns: Vec<usize>,
    /// Lines longer than the view go on to the next row of it, at any
    /// character, rather than off its right edge: vim's `wrap`.
    pub wrap: bool,
    /// Show what the language server says inline of the text, a type or
    /// a parameter's name, where lines do not wrap.
    pub inlay_hints: bool,
    /// Draw each token as the language server tells what it is, over what
    /// the syntax tree makes of it.
    pub semantic_tokens: bool,
    /// Draw a line down each step of the lines' indents, vim's
    /// indent-blankline.
    pub indent_guides: bool,
    /// Keep the first lines of the functions and types the top of the
    /// view is in standing at its top, VS Code's sticky scroll.
    pub sticky_scroll: bool,
    /// Mark beside each line what changed from the file as it is at
    /// HEAD: lines added, changed, or taken out.
    pub git_gutter: bool,
    /// The colors: `theme.rs`.
    pub theme: theme::ThemeChoice,
    /// Keep each file's undo history when it is saved, and take it up
    /// when it is opened again: vim's `undofile`.
    pub undo_file: bool,
    /// Where those histories are kept. `None` to keep none.
    pub undo_dir: Option<PathBuf>,
    /// Work out each project's environment (login shell, direnv) for its
    /// language servers and programs, rather than use med's own.
    pub project_env: bool,
    /// Where each project's session is kept: the panes and files it had
    /// open, written when med quits. `None` to keep none.
    pub session_dir: Option<PathBuf>,
    /// Where the projects opened are listed, the last first, for
    /// `(recent-projects)`. `None` to list none.
    pub projects_file: Option<PathBuf>,
    /// Where the files `load` takes from GitHub are kept: `github.rs`.
    /// `None` to take none.
    pub github_dir: Option<PathBuf>,
}

impl Settings {
    pub const FONT_SIZE: f32 = 14.;
    pub const KEY_TIMEOUT: Duration = Duration::from_millis(1000);
    pub const SCROLL_OFF: usize = 3;
    pub const LINE_HEIGHT: f32 = 1.5;
    pub const TERMINAL_LINE_HEIGHT: f32 = 1.4;

    /// The font the editor's text is drawn in; `mono`, the theme's
    /// monospace family, where no family is set.
    pub fn editor_font(&self, mono: &SharedString) -> Font {
        self.font(self.font_family.as_ref().unwrap_or(mono))
    }

    /// The font of the terminal: its own family, else the editor's.
    pub fn terminal_font(&self, mono: &SharedString) -> Font {
        let family = self
            .terminal_font_family
            .as_ref()
            .or(self.font_family.as_ref());
        self.font(family.unwrap_or(mono))
    }

    pub fn terminal_font_size(&self) -> f32 {
        self.terminal_font_size.unwrap_or(self.font_size)
    }

    fn font(&self, family: &SharedString) -> Font {
        let mut features = self.font_features.clone();
        if !self.ligatures {
            features.retain(|(tag, _)| tag != "calt" && tag != "liga");
            features.extend([("calt".into(), 0), ("liga".into(), 0)]);
        }
        let fallbacks = self.font_fallbacks.iter().map(|f| f.to_string()).collect();
        Font {
            family: family.clone(),
            features: FontFeatures(std::sync::Arc::new(features)),
            fallbacks: (!self.font_fallbacks.is_empty())
                .then(|| FontFallbacks::from_fonts(fallbacks)),
            weight: self.font_weight,
            style: FontStyle::Normal,
        }
    }
}

impl Global for Settings {}

/// The editor's font, for the fixed-width text outside it: popups, panels
/// and the status bar.
pub fn mono_font(cx: &App) -> Font {
    cx.global::<Settings>()
        .editor_font(&cx.theme().mono_font_family)
}

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

/// Where med keeps what it remembers between runs: `$MED_STATE_DIR`,
/// `$XDG_STATE_HOME/med` or `~/.local/state/med`.
pub fn state_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    if let Some(dir) = var("MED_STATE_DIR") {
        Some(PathBuf::from(dir))
    } else if let Some(dir) = var("XDG_STATE_HOME") {
        Some(PathBuf::from(dir).join("med"))
    } else {
        Some(
            PathBuf::from(var("HOME")?)
                .join(".local")
                .join("state")
                .join("med"),
        )
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
    // The window's root moves focus on `tab`, before any view hears of it:
    // here `tab` is a key for the editor, panels and terminal to have.
    cx.bind_keys([
        KeyBinding::new("tab", NoAction, Some("Root")),
        KeyBinding::new("shift-tab", NoAction, Some("Root")),
    ]);
    // gpui-component starts out light; follow the system instead.
    theme::apply(None, cx);
    cx.set_global(Settings {
        font_family: std::env::var("MED_FONT").ok().map(SharedString::from),
        font_size: std::env::var("MED_FONT_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(Settings::FONT_SIZE),
        ui_font_size: None,
        line_height: Settings::LINE_HEIGHT,
        font_weight: FontWeight::NORMAL,
        font_features: Vec::new(),
        ligatures: true,
        font_fallbacks: Vec::new(),
        terminal_font_family: None,
        terminal_font_size: None,
        terminal_line_height: Settings::TERMINAL_LINE_HEIGHT,
        lsp: std::env::var_os("MED_NO_LSP").is_none(),
        key_timeout: Some(Settings::KEY_TIMEOUT),
        format_on_save: false,
        tab_width: None,
        relative_number: false,
        scroll_off: Settings::SCROLL_OFF,
        cursor_line: true,
        color_columns: Vec::new(),
        wrap: false,
        inlay_hints: false,
        semantic_tokens: true,
        indent_guides: false,
        sticky_scroll: false,
        // Turned on by `run`: tests must not start git for every file.
        git_gutter: false,
        theme: theme::ThemeChoice::System,
        undo_file: false,
        // Turned on by `run`: tests must not start the user's shell.
        project_env: false,
        // Turned on by `run` too: tests must not write to $HOME.
        session_dir: None,
        projects_file: None,
        undo_dir: None,
        github_dir: None,
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
            let settings = cx.global_mut::<Settings>();
            settings.project_env = std::env::var_os("MED_NO_PROJECT_ENV").is_none();
            settings.git_gutter = true;
            settings.session_dir = std::env::var_os("MED_NO_SESSION")
                .is_none()
                .then(state_dir)
                .flatten()
                .map(|dir| dir.join("sessions"));
            settings.undo_dir = state_dir().map(|dir| dir.join("undo"));
            settings.projects_file = state_dir().map(|dir| dir.join("projects"));
            settings.github_dir = state_dir().map(|dir| dir.join("github"));
            // The theme files, for the init file to name, and then watched:
            // a theme changed there is changed here. A directory that is not
            // there is not made.
            if let Some(themes) = config_dir().map(|dir| dir.join("themes"))
                && themes.is_dir()
            {
                theme::load_dir(&themes, cx);
                // The watcher reads the files again into the registry, and
                // says nothing more: the theme is put on again from there.
                // Putting it on never changes the registry.
                let dir = themes.clone();
                cx.observe_global::<gpui::component::ThemeRegistry>(move |cx| {
                    theme::reload_med_colors(&dir, cx);
                    theme::apply(None, cx);
                })
                .detach();
                gpui::component::ThemeRegistry::watch_dir(themes, cx, |_| {}).ok();
            }
            // Not in `init`: tests must not depend on what is in $HOME.
            script::load_init(cx);
            cx.activate(true);
            // There is nothing to do with no window: the last one closed,
            // med quits.
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            open_workspace(root, files, cx);
        });
}

/// Open a window on the project at `root`, with `files` open in it, or
/// with its session if there are none.
pub fn open_workspace(root: PathBuf, files: Vec<PathBuf>, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
    gpui::open_window(
        // The title bar is drawn by the workspace, which a program can give
        // a layout of its own (`defpanel :at 'title`).
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
}

/// Bring up the window on the project at `root`: the one there is, or a
/// new one. Done once whatever is being done now is done, as it looks at
/// every workspace.
pub fn open_project(root: PathBuf, cx: &mut App) {
    cx.defer(move |cx| {
        let open = workspace::all(cx).into_iter().find(|(_, workspace)| {
            let read = |workspace: &Workspace, _: &App| workspace.root() == root;
            workspace.read_with(cx, read).unwrap_or(false)
        });
        match open {
            Some((window, _)) => {
                window
                    .update(cx, |_, window, _| window.activate_window())
                    .ok();
            }
            None => open_workspace(root, Vec::new(), cx),
        }
    });
}
