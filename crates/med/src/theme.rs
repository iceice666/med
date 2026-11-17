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

use crate::Settings;
use crate::panel::{Color, SetThemeColor, THEME_COLORS};
use gpui::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use gpui::{App, Global, Hsla, Rgba, SharedString, Window, px};
use std::path::Path;
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
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    for path in paths {
        if let Ok(text) = std::fs::read_to_string(&path) {
            ThemeRegistry::global_mut(cx)
                .load_themes_from_str(&text)
                .ok();
        }
    }
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
    // none: say it every time. The programs' colors go over the file's.
    let own = cx.default_global::<OwnColors>().0.clone();
    Theme::update(cx, |theme| {
        let config = match theme.mode.is_dark() {
            true => &theme.dark_theme,
            false => &theme.light_theme,
        };
        theme.font_size = px(ui_font_size.or(config.font_size).unwrap_or(UI_FONT_SIZE));
        for (_, set, color) in &own {
            set(theme, color.of(theme));
        }
    });
}

/// gpui-component's own `rem`.
const UI_FONT_SIZE: f32 = 16.;

/// The colors programs put in place of the theme's, by name, in the
/// order they were given: one can be another of the theme's, as it is
/// after those before it.
#[derive(Default)]
struct OwnColors(Vec<(&'static str, SetThemeColor, Color)>);

impl Global for OwnColors {}

/// Put `color` in place of the theme's `name`, over every theme from now
/// on, or with `None` the theme's own back.
pub(crate) fn set_color(
    name: &'static str,
    set: SetThemeColor,
    color: Option<Color>,
    cx: &mut App,
) {
    let own = &mut cx.default_global::<OwnColors>().0;
    own.retain(|(known, ..)| *known != name);
    match color {
        Some(color) => {
            own.push((name, set, color));
            Theme::update(cx, |theme| set(theme, color.of(theme)));
        }
        None => put_on(None, cx),
    }
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
    THEME_COLORS.iter().map(|(_, get, _)| get(theme)).collect()
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
