//! Key tables for the views that are not editors.
//!
//! An editor's keys go through the vim engine and its
//! [`Keymap`](med_vim::Keymap). The file tree and the git screen have no
//! modes or operators, just key sequences bound to commands, kept here by
//! context name (`tree`, `git`, `git.files`…).

use crate::editor::translate_key;
use gpui::{App, Global, Keystroke, SharedString};
use med_vim::{Key, KeyTrie, Lookup, Mode, parse_keys};
use std::collections::HashMap;

#[derive(Default)]
pub struct PanelKeymaps {
    contexts: HashMap<String, KeyTrie<SharedString>>,
}

impl Global for PanelKeymaps {}

impl PanelKeymaps {
    /// Bind `keys` (vim notation, see [`parse_keys`]) to a command while
    /// `context` is active.
    pub fn bind(&mut self, context: &str, keys: &str, command: &str) {
        self.contexts
            .entry(context.to_string())
            .or_default()
            .insert(&parse_keys(keys), SharedString::new(command));
    }

    /// Remove a binding. False if `keys` were not bound.
    pub fn unbind(&mut self, context: &str, keys: &str) -> bool {
        self.contexts
            .get_mut(context)
            .and_then(|trie| trie.remove(&parse_keys(keys)))
            .is_some()
    }

    /// The command `keys` are bound to in `context`.
    pub fn command(&self, context: &str, keys: &str) -> Option<&str> {
        let command = self.contexts.get(context)?.get(&parse_keys(keys))?;
        Some(command.as_ref())
    }

    /// The first of `contexts` that has anything to say about `keys`
    /// decides.
    fn lookup(&self, contexts: &[&str], keys: &[Key]) -> Lookup<SharedString> {
        contexts
            .iter()
            .filter_map(|context| self.contexts.get(*context))
            .map(|trie| trie.lookup(keys))
            .find(|found| *found != Lookup::None)
            .unwrap_or(Lookup::None)
    }
}

/// What a key press in a panel came to.
#[derive(Debug, PartialEq)]
pub enum Press {
    Command(SharedString),
    /// The start of a longer binding.
    Pending,
    Unbound,
}

/// The keys of a binding typed so far.
#[derive(Default)]
pub struct KeySequence(Vec<Key>);

impl KeySequence {
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Add `key` and look the sequence up in `contexts`, most specific
    /// first.
    pub fn press(&mut self, key: Key, contexts: &[&str], cx: &App) -> Press {
        let keymaps = cx.global::<PanelKeymaps>();
        self.0.push(key);
        loop {
            match keymaps.lookup(contexts, &self.0) {
                Lookup::Found(command) => {
                    self.0.clear();
                    return Press::Command(command);
                }
                Lookup::Pending => return Press::Pending,
                // Not a sequence after all: the last key may still be a
                // binding of its own.
                Lookup::None if self.0.len() > 1 => {
                    self.0.drain(..self.0.len() - 1);
                }
                Lookup::None => {
                    self.0.clear();
                    return Press::Unbound;
                }
            }
        }
    }
}

/// A keystroke as a panel sees it. `None` for the ones that belong to the
/// application (`cmd-…`) or the system (`alt-…`).
pub fn panel_key(keystroke: &Keystroke) -> Option<Key> {
    let modifiers = &keystroke.modifiers;
    if modifiers.alt {
        return None;
    }
    if modifiers.shift && keystroke.key == "tab" && !modifiers.platform {
        return Some(Key::BackTab);
    }
    translate_key(keystroke, Mode::Normal)
}
