//! The colors: gpui-component's themes, its own light and dark ones and
//! those of the theme files in `<config>/themes/`, picked by the `theme`
//! option.
//!
//! With no theme picked the window goes by the system's appearance, light
//! or dark, as it always did. One theme is that theme, whatever the
//! system's appearance; a light one and a dark one go by it again.
//!
//! The theme sets the size of what is not a file's text too, the `rem`
//! everything gpui-component lays out goes by: the `ui-font-size` option
//! over the theme file's own, and 16 where neither says.
//!
//! A program can read the theme's colors by name, with `theme-color`, and
//! put its own in their place, with `set-theme-color`: those stay over
//! whichever theme is put on, until they are taken away. When a theme put
//! on brings other colors, programs are told with the `theme-changed`
//! hook, as the window that is first to hear of it runs it.
//!
//! The editor and the terminal have colors of their own, med's
//! ([`MED_COLORS`]): the search's, the git gutter's, the terminal's
//! sixteen. Each is worked out from the theme's colors, as it was before
//! there were names for them, unless the theme file says otherwise in a
//! `"med"` section of the theme, next to its `"colors"`:
//!
//! ```json
//! { "name": "Catppuccin Mocha", "mode": "dark",
//!   "colors": { ... },
//!   "med": { "search": "#f9e2af59", "terminal-red": "#f38ba8" } }
//! ```
//!
//! gpui-component passes over what it does not know, so the file is
//! still a theme to it. Programs name these colors as they name the
//! theme's.

use crate::Settings;
use crate::panel::{Color, Named, THEME_COLORS};
use gpui::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use gpui::{App, Global, Hsla, Rgba, SharedString, Window, px};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// What the `theme` option names.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ThemeChoice {
    /// gpui-component's own, light or dark as the system is.
    #[default]
    System,
    /// This one, always.
    One(SharedString),
    /// A light one and a dark one, as the system is.
    Pair(SharedString, SharedString),
}

/// Put the theme files of `dir` in the registry, so that the init file
/// can name their themes. A file that is not a theme is passed over.
pub fn load_dir(dir: &Path, cx: &mut App) {
    let paths = theme_files(dir);
    for path in &paths {
        if let Ok(text) = std::fs::read_to_string(path) {
            ThemeRegistry::global_mut(cx)
                .load_themes_from_str(&text)
                .ok();
        }
    }
    read_med_colors(&paths, cx);
}

/// Read again the `"med"` sections of the theme files of `dir`, as they
/// are now: after one changed.
pub fn reload_med_colors(dir: &Path, cx: &mut App) {
    read_med_colors(&theme_files(dir), cx);
}

/// The `.json` files of `dir`, in the order of their names.
fn theme_files(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths
}

/// What the `"med"` sections of these files say, by theme. A color that is
/// not med's, or not a color, is passed over and the rest are kept.
fn read_med_colors(paths: &[PathBuf], cx: &mut App) {
    let mut by_theme: HashMap<SharedString, Vec<(usize, Hsla)>> = HashMap::new();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(file) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let themes = file.get("themes").and_then(|t| t.as_array());
        for theme in themes.into_iter().flatten() {
            let (Some(name), Some(med)) = (
                theme.get("name").and_then(|n| n.as_str()),
                theme.get("med").and_then(|m| m.as_object()),
            ) else {
                continue;
            };
            let colors = med.iter().filter_map(|(key, value)| {
                let index = MED_COLORS.iter().position(|(known, _)| known == key)?;
                let color = Rgba::try_from(value.as_str()?).ok()?;
                Some((index, color.into()))
            });
            by_theme
                .entry(SharedString::from(name.to_string()))
                .or_default()
                .extend(colors);
        }
    }
    cx.set_global(FileColors(by_theme));
}

fn find(name: &SharedString, cx: &App) -> Result<Rc<ThemeConfig>, String> {
    let registry = ThemeRegistry::global(cx);
    registry.themes().get(name).cloned().ok_or_else(|| {
        let mut names: Vec<&str> = registry.themes().keys().map(|n| n.as_ref()).collect();
        names.sort_unstable();
        format!("there is no theme {name:?} ({})", names.join(", "))
    })
}

/// Whether the themes named are there, and a pair is a light and a dark.
pub fn check(choice: &ThemeChoice, cx: &App) -> Result<(), String> {
    match choice {
        ThemeChoice::System => Ok(()),
        ThemeChoice::One(name) => find(name, cx).map(drop),
        ThemeChoice::Pair(light, dark) => {
            if find(light, cx)?.mode.is_dark() {
                return Err(format!("{light:?} is a dark theme, not a light one"));
            }
            if !find(dark, cx)?.mode.is_dark() {
                return Err(format!("{dark:?} is a light theme, not a dark one"));
            }
            Ok(())
        }
    }
}

/// Put on the theme the option picks, light or dark as the window, or
/// the system, is where that is for it to say. A theme no longer there
/// is gpui-component's own again. Programs are told if the colors they
/// can name are not what they were.
pub fn apply(window: Option<&mut Window>, cx: &mut App) {
    let before = colors(cx);
    put_on(window, cx);
    if colors(cx) != before {
        cx.default_global::<Changes>().made += 1;
    }
}

fn put_on(window: Option<&mut Window>, cx: &mut App) {
    let appearance: ThemeMode = window
        .as_ref()
        .map(|window| window.appearance())
        .unwrap_or_else(|| cx.window_appearance())
        .into();
    let (choice, ui_font_size) = cx
        .try_global::<Settings>()
        .map(|s| (s.theme.clone(), s.ui_font_size))
        .unwrap_or_default();
    let registry = ThemeRegistry::global(cx);
    let mut light = registry.default_light_theme().clone();
    let mut dark = registry.default_dark_theme().clone();
    let mode = match &choice {
        ThemeChoice::System => appearance,
        ThemeChoice::One(name) => match find(name, cx) {
            Ok(theme) => {
                let mode = theme.mode;
                if mode.is_dark() {
                    dark = theme;
                } else {
                    light = theme;
                }
                mode
            }
            Err(_) => appearance,
        },
        ThemeChoice::Pair(l, d) => {
            if let (Ok(l), Ok(d)) = (find(l, cx), find(d, cx)) {
                (light, dark) = (l, d);
            }
            appearance
        }
    };
    let theme = Theme::global_mut(cx);
    theme.light_theme = light;
    theme.dark_theme = dark;
    Theme::change(mode, window, cx);
    // Putting the theme on again keeps the last size where the file has
    // none: say it every time.
    Theme::update(cx, |theme| {
        let config = match theme.mode.is_dark() {
            true => &theme.dark_theme,
            false => &theme.light_theme,
        };
        theme.font_size = px(ui_font_size.or(config.font_size).unwrap_or(UI_FONT_SIZE));
    });
    // The programs' colors go over the file's, the theme's first: med's
    // are worked out from those, and can be named by them too.
    let own = cx.default_global::<OwnColors>().0.clone();
    work_out_med_colors(cx);
    for (_, named, color) in &own {
        if let Named::Theme(_, set) = *named {
            let color = color.of(cx);
            Theme::update(cx, |theme| set(theme, color));
        }
    }
    work_out_med_colors(cx);
    for (_, named, color) in &own {
        if let Named::Med(index) = *named {
            let color = color.of(cx);
            cx.global_mut::<MedColors>().0[index] = color;
        }
    }
}

/// Med's colors, from the theme in use and what its file says of them.
fn work_out_med_colors(cx: &mut App) {
    let theme = Theme::global(cx);
    let mut colors: Vec<Hsla> = MED_COLORS.iter().map(|(_, of)| of(theme)).collect();
    let file = cx
        .try_global::<FileColors>()
        .and_then(|file| file.0.get(theme.theme_name()));
    for &(index, color) in file.into_iter().flatten() {
        colors[index] = color;
    }
    cx.set_global(MedColors(colors));
}

/// gpui-component's own `rem`.
const UI_FONT_SIZE: f32 = 16.;

/// The colors programs put in place of the theme's, by name, in the
/// order they were given: one can be another of the theme's, as it is
/// after those before it.
#[derive(Default)]
struct OwnColors(Vec<(&'static str, Named, Color)>);

impl Global for OwnColors {}

/// Put `color` in place of the theme's `name`, over every theme from now
/// on, or with `None` the theme's own back.
pub(crate) fn set_color(name: &'static str, named: Named, color: Option<Color>, cx: &mut App) {
    let own = &mut cx.default_global::<OwnColors>().0;
    own.retain(|(known, ..)| *known != name);
    if let Some(color) = color {
        own.push((name, named, color));
    }
    put_on(None, cx);
}

/// Take away every color programs put in place of the theme's, as
/// before the init file is run again.
pub(crate) fn clear_colors(cx: &mut App) {
    let own = &mut cx.default_global::<OwnColors>().0;
    if !own.is_empty() {
        own.clear();
        put_on(None, cx);
    }
}

/// How many times a theme was put on that brought other colors, and how
/// many of those programs have been told of.
#[derive(Default)]
pub(crate) struct Changes {
    pub(crate) made: u64,
    pub(crate) told: u64,
}

impl Global for Changes {}

/// The colors a program can name, as they are now.
fn colors(cx: &App) -> Vec<Hsla> {
    let theme = Theme::global(cx);
    let med = (0..MED_COLORS.len()).map(|index| med_color_at(index, cx));
    THEME_COLORS
        .iter()
        .map(|(_, get, _)| get(theme))
        .chain(med)
        .collect()
}

/// A color as `"#rrggbb"`, or `"#rrggbbaa"` where it can be seen through.
pub(crate) fn hex(color: Hsla) -> String {
    let rgba = Rgba::from(color);
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    let rgb = format!(
        "#{:02x}{:02x}{:02x}",
        byte(rgba.r),
        byte(rgba.g),
        byte(rgba.b)
    );
    match byte(rgba.a) {
        255 => rgb,
        alpha => format!("{rgb}{alpha:02x}"),
    }
}

// ---- med's own colors ----------------------------------------------------

/// Each of med's colors: its name, and how it is worked out from the
/// theme where the theme file does not say.
macro_rules! med_colors {
    ($($variant:ident $name:literal $of:expr;)*) => {
        /// One of [`MED_COLORS`].
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum MedColor {
            $($variant,)*
        }

        pub(crate) const MED_COLORS: &[(&str, fn(&Theme) -> Hsla)] = &[$(($name, $of),)*];
    };
}

med_colors! {
    // A shade of the text over whatever is behind it: darker in a light
    // theme, lighter in a dark one.
    // The cursor of a pane the keyboard is not in: an outline.
    CursorInactive "cursor-inactive" |t| t.caret.opacity(0.7);
    ColorColumn "color-column" |t| t.foreground.opacity(0.07);
    IndentGuide "indent-guide" |t| t.foreground.opacity(0.12);
    IndentGuideActive "indent-guide-active" |t| t.foreground.opacity(0.35);
    Fold "fold" |t| t.foreground.opacity(0.1);
    Search "search" |t| t.warning.opacity(0.35);
    // What `motion.flash` found: the place `enter` goes to stands out
    // among the others, and a label from everything.
    FlashTarget "flash-target" |t| t.warning.opacity(0.7);
    FlashLabel "flash-label" |t| t.danger;
    FlashLabelForeground "flash-label-foreground" |t| t.background;
    GitAdded "git-added" |t| t.green;
    GitModified "git-modified" |t| t.blue;
    GitDeleted "git-deleted" |t| t.red;
    DiagnosticError "diagnostic-error" |t| t.danger;
    DiagnosticWarning "diagnostic-warning" |t| t.warning;
    DiagnosticInfo "diagnostic-info" |t| t.info;
    DiagnosticHint "diagnostic-hint" |t| t.muted_foreground;
    TerminalForeground "terminal-foreground" |t| t.foreground;
    TerminalBackground "terminal-background" |t| t.background;
    TerminalCursor "terminal-cursor" |t| t.foreground;
    // Drawn over the cells as it is: what it lets through is not used.
    TerminalSelection "terminal-selection" |t| t.selection;
    // xterm's, on a light background or a dark one.
    TerminalBlack "terminal-black" |t| ansi(t, 0);
    TerminalRed "terminal-red" |t| ansi(t, 1);
    TerminalGreen "terminal-green" |t| ansi(t, 2);
    TerminalYellow "terminal-yellow" |t| ansi(t, 3);
    TerminalBlue "terminal-blue" |t| ansi(t, 4);
    TerminalMagenta "terminal-magenta" |t| ansi(t, 5);
    TerminalCyan "terminal-cyan" |t| ansi(t, 6);
    TerminalWhite "terminal-white" |t| ansi(t, 7);
    TerminalBrightBlack "terminal-bright-black" |t| ansi(t, 8);
    TerminalBrightRed "terminal-bright-red" |t| ansi(t, 9);
    TerminalBrightGreen "terminal-bright-green" |t| ansi(t, 10);
    TerminalBrightYellow "terminal-bright-yellow" |t| ansi(t, 11);
    TerminalBrightBlue "terminal-bright-blue" |t| ansi(t, 12);
    TerminalBrightMagenta "terminal-bright-magenta" |t| ansi(t, 13);
    TerminalBrightCyan "terminal-bright-cyan" |t| ansi(t, 14);
    TerminalBrightWhite "terminal-bright-white" |t| ansi(t, 15);
    // Behind a program's popup panel, over the rest of the window.
    PopupOverlay "popup-overlay" |t| t.overlay.opacity(0.25);
}

/// The terminal's color `index` of the sixteen, as xterm has it.
fn ansi(theme: &Theme, index: usize) -> Hsla {
    let palette = match theme.is_dark() {
        true => med_term::Palette::dark(),
        false => med_term::Palette::light(),
    };
    let rgb = palette.ansi[index];
    let channel = |v: u8| v as f32 / 255.;
    Rgba {
        r: channel(rgb.r),
        g: channel(rgb.g),
        b: channel(rgb.b),
        a: 1.,
    }
    .into()
}

/// Med's colors as they are now, in the order of [`MED_COLORS`].
struct MedColors(Vec<Hsla>);

impl Global for MedColors {}

/// What the theme files say of med's colors, by theme.
struct FileColors(HashMap<SharedString, Vec<(usize, Hsla)>>);

impl Global for FileColors {}

/// One of med's colors, in the theme in use.
pub(crate) fn med_color(color: MedColor, cx: &App) -> Hsla {
    med_color_at(color as usize, cx)
}

/// The one of [`MED_COLORS`] at `index`.
pub(crate) fn med_color_at(index: usize, cx: &App) -> Hsla {
    match cx.try_global::<MedColors>() {
        Some(colors) => colors.0[index],
        None => MED_COLORS[index].1(Theme::global(cx)),
    }
}
