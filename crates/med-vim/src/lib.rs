//! Vim-style modal editing for med, independent of any UI.

mod command;
mod ex;
mod keymap;
pub mod motion;
mod textobj;
mod vim;

#[cfg(test)]
mod tests;

pub use command::command_names;
pub use ex::build_regex;
pub use keymap::{KeyTrie, Keymap, Lookup, MapMode, format_keys, parse_keys};
pub use vim::{
    Clipboard, Ctx, Effect, Key, Lang, MemClipboard, Mode, Register, Scroll, Search, Shared,
    Viewport, Vim, find_match,
};
