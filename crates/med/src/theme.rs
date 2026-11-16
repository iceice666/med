//! The colors: gpui-component's themes, its own light and dark ones and
//! those of the theme files in `<config>/themes/`, picked by the `theme`
//! option.
//!
//! With no theme picked the window goes by the system's appearance, light
//! or dark, as it always did. One theme is that theme, whatever the
//! system's appearance; a light one and a dark one go by it again.

use crate::Settings;
use gpui::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use gpui::{App, SharedString, Window};
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
/// is gpui-component's own again.
pub fn apply(window: Option<&mut Window>, cx: &mut App) {
    let appearance: ThemeMode = window
        .as_ref()
        .map(|window| window.appearance())
        .unwrap_or_else(|| cx.window_appearance())
        .into();
    let choice = cx
        .try_global::<Settings>()
        .map(|s| s.theme.clone())
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
}
