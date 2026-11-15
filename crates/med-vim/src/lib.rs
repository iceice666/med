//! Vim-style modal editing for med, independent of any UI.

mod command;
mod ex;
mod flash;
mod keymap;
pub mod motion;
mod textobj;
mod vim;

#[cfg(test)]
mod tests;

pub use command::command_names;
pub use ex::build_regex;
pub use flash::{FlashMatch, FlashView};
pub use keymap::{Bound, KeyTrie, Keymap, Lookup, MapMode, NextKey, format_keys, parse_keys};
pub use vim::{
    Awaiting, Clipboard, Ctx, Effect, Key, Lang, MemClipboard, Mode, PAIRS, PendingKeys, Register,
    Scroll, Search, Shared, SyntaxTree, Viewport, Vim, find_match,
};
