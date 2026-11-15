//! Vim-style modal editing for med, independent of any UI.

mod ex;
pub mod motion;
mod textobj;
mod vim;

#[cfg(test)]
mod tests;

pub use ex::build_regex;
pub use vim::{
    Clipboard, Ctx, Effect, Key, Lang, MemClipboard, Mode, Register, Scroll, Search, Shared,
    Viewport, Vim, find_match,
};
