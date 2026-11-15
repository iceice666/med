//! Key sequences and what they are bound to.
//!
//! [`Keymap::vim`] is the stock vim layout, written as a table of key
//! sequence → command name; a host changes it with [`Keymap::bind`] and
//! friends. What stays in the engine is vim's grammar — counts, registers,
//! operator + motion, the doubled operator (`dd`) — which works the same
//! whatever the keys are.

use crate::command::{self, Action};
use crate::vim::Key;
use std::collections::HashMap;

/// Parse vim key notation: `"gd"`, `"<C-w>"`, `"<Space>f"`, `"d<Esc>"`.
/// Names are case-insensitive; a `<` that does not open a known name is a
/// literal `<`.
pub fn parse_keys(keys: &str) -> Vec<Key> {
    let mut out = Vec::new();
    let mut rest = keys;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
            && let Some(key) = named_key(&rest[1..end])
        {
            out.push(key);
            rest = &rest[end + 1..];
            continue;
        }
        out.push(Key::Char(c));
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn named_key(name: &str) -> Option<Key> {
    let name = name.to_ascii_lowercase();
    Some(match name.as_str() {
        "esc" => Key::Esc,
        "cr" | "enter" | "return" => Key::Enter,
        "bs" => Key::Backspace,
        "del" => Key::Delete,
        "tab" => Key::Tab,
        "s-tab" => Key::BackTab,
        "space" => Key::Char(' '),
        "lt" => Key::Char('<'),
        "bar" => Key::Char('|'),
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "c-space" => Key::Ctrl(' '),
        _ => {
            let mut chars = name.strip_prefix("c-")?.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Ctrl(c),
                _ => return None,
            }
        }
    })
}

/// `keys` in the notation [`parse_keys`] reads.
pub fn format_keys(keys: &[Key]) -> String {
    let mut out = String::new();
    for key in keys {
        match key {
            Key::Char(' ') => out.push_str("<Space>"),
            Key::Char('<') => out.push_str("<lt>"),
            Key::Char(c) => out.push(*c),
            Key::Text(_) => {}
            Key::Ctrl(' ') => out.push_str("<C-Space>"),
            Key::Ctrl(c) => {
                out.push_str("<C-");
                out.push(*c);
                out.push('>');
            }
            Key::Esc => out.push_str("<Esc>"),
            Key::Enter => out.push_str("<CR>"),
            Key::Backspace => out.push_str("<BS>"),
            Key::Delete => out.push_str("<Del>"),
            Key::Tab => out.push_str("<Tab>"),
            Key::BackTab => out.push_str("<S-Tab>"),
            Key::Left => out.push_str("<Left>"),
            Key::Right => out.push_str("<Right>"),
            Key::Up => out.push_str("<Up>"),
            Key::Down => out.push_str("<Down>"),
            Key::Home => out.push_str("<Home>"),
            Key::End => out.push_str("<End>"),
            Key::PageUp => out.push_str("<PageUp>"),
            Key::PageDown => out.push_str("<PageDown>"),
        }
    }
    out
}

/// What the keys typed so far amount to.
#[derive(Clone, Debug, PartialEq)]
pub enum Lookup<T> {
    /// Not a binding, nor the start of one.
    None,
    /// The start of at least one longer binding.
    Pending,
    /// A binding that no longer one starts with.
    Found(T),
}

/// Values keyed by key sequence.
pub struct KeyTrie<T> {
    root: Node<T>,
}

struct Node<T> {
    value: Option<T>,
    children: HashMap<Key, Node<T>>,
}

impl<T> Default for Node<T> {
    fn default() -> Self {
        Self {
            value: None,
            children: HashMap::new(),
        }
    }
}

impl<T> Default for KeyTrie<T> {
    fn default() -> Self {
        Self {
            root: Node::default(),
        }
    }
}

impl<T: Clone> KeyTrie<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `keys`, replacing what they were bound to. An empty sequence
    /// binds nothing.
    pub fn insert(&mut self, keys: &[Key], value: T) {
        if keys.is_empty() {
            return;
        }
        let mut node = &mut self.root;
        for key in keys {
            node = node.children.entry(key.clone()).or_default();
        }
        node.value = Some(value);
    }

    pub fn remove(&mut self, keys: &[Key]) -> Option<T> {
        fn remove<T>(node: &mut Node<T>, keys: &[Key]) -> Option<T> {
            let Some((first, rest)) = keys.split_first() else {
                return node.value.take();
            };
            let child = node.children.get_mut(first)?;
            let removed = remove(child, rest);
            // Leave no dead prefix behind to be waited on.
            if child.value.is_none() && child.children.is_empty() {
                node.children.remove(first);
            }
            removed
        }
        remove(&mut self.root, keys)
    }

    fn node(&self, keys: &[Key]) -> Option<&Node<T>> {
        keys.iter()
            .try_fold(&self.root, |node, key| node.children.get(key))
    }

    /// The binding for exactly `keys`.
    pub fn get(&self, keys: &[Key]) -> Option<&T> {
        self.node(keys)?.value.as_ref()
    }

    /// A sequence that is both a binding and the start of longer ones is
    /// [`Lookup::Pending`]: the next key (or [`KeyTrie::longest_prefix`]
    /// when none comes) decides.
    pub fn lookup(&self, keys: &[Key]) -> Lookup<T> {
        match self.node(keys) {
            Some(node) if !node.children.is_empty() => Lookup::Pending,
            Some(Node {
                value: Some(value), ..
            }) => Lookup::Found(value.clone()),
            _ => Lookup::None,
        }
    }

    /// The longest leading part of `keys` that is bound: its length and
    /// its value.
    pub fn longest_prefix(&self, keys: &[Key]) -> Option<(usize, T)> {
        let mut node = &self.root;
        let mut best = None;
        for (index, key) in keys.iter().enumerate() {
            let Some(child) = node.children.get(key) else {
                break;
            };
            if let Some(value) = &child.value {
                best = Some((index + 1, value.clone()));
            }
            node = child;
        }
        best
    }
}

/// Which table a binding lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapMode {
    Normal,
    /// Both visual modes.
    Visual,
    /// After an operator, while it waits for a motion (`d` in `dw`).
    Operator,
    Insert,
}

impl MapMode {
    pub const ALL: [MapMode; 4] = [
        MapMode::Normal,
        MapMode::Visual,
        MapMode::Operator,
        MapMode::Insert,
    ];
}

/// The engine's key bindings, one table per [`MapMode`].
pub struct Keymap {
    maps: [KeyTrie<Action>; 4],
}

impl Default for Keymap {
    fn default() -> Self {
        Self::vim()
    }
}

impl Keymap {
    /// No bindings at all: every key but text typed in insert mode is dead.
    pub fn empty() -> Self {
        Self {
            maps: std::array::from_fn(|_| KeyTrie::new()),
        }
    }

    /// The stock vim layout.
    pub fn vim() -> Self {
        let mut keymap = Self::empty();
        for (modes, keys, command) in DEFAULTS {
            for mode in *modes {
                keymap.bind(*mode, keys, command);
            }
        }
        keymap
    }

    fn map(&mut self, mode: MapMode) -> &mut KeyTrie<Action> {
        &mut self.maps[mode as usize]
    }

    /// Bind `keys` (in [`parse_keys`] notation) to a command: one of
    /// [`command_names`](crate::command_names), or any other name, which is
    /// then the host's to run and arrives as
    /// [`Effect::Command`](crate::Effect::Command).
    pub fn bind(&mut self, mode: MapMode, keys: &str, command: &str) {
        let action = match command::builtin(command) {
            Some(builtin) => Action::Builtin(builtin),
            None => Action::Host(command.into()),
        };
        self.map(mode).insert(&parse_keys(keys), action);
    }

    /// Bind `keys` to other keys, which are typed in their place and are
    /// themselves subject to the keymap (vim's `:map`, not `:noremap`).
    pub fn bind_keys(&mut self, mode: MapMode, keys: &str, to: &str) {
        let action = Action::Keys(parse_keys(to).into());
        self.map(mode).insert(&parse_keys(keys), action);
    }

    /// Remove a binding. False if `keys` were not bound.
    pub fn unbind(&mut self, mode: MapMode, keys: &str) -> bool {
        self.map(mode).remove(&parse_keys(keys)).is_some()
    }

    /// The command `keys` are bound to; `None` for unbound keys and for
    /// keys bound to other keys.
    pub fn command(&self, mode: MapMode, keys: &str) -> Option<&str> {
        match self.maps[mode as usize].get(&parse_keys(keys))? {
            Action::Builtin(builtin) => command::name(*builtin),
            Action::Host(name) => Some(name),
            Action::Keys(_) => None,
        }
    }

    pub(crate) fn lookup(&self, mode: MapMode, keys: &[Key]) -> Lookup<Action> {
        self.maps[mode as usize].lookup(keys)
    }

    pub(crate) fn longest_prefix(&self, mode: MapMode, keys: &[Key]) -> Option<(usize, Action)> {
        self.maps[mode as usize].longest_prefix(keys)
    }
}

use MapMode::{Insert, Normal, Operator, Visual};

/// Normal, visual and operator-pending: motions and operators.
const NVO: &[MapMode] = &[Normal, Visual, Operator];
const NV: &[MapMode] = &[Normal, Visual];
const VO: &[MapMode] = &[Visual, Operator];
const N: &[MapMode] = &[Normal];
const V: &[MapMode] = &[Visual];
const I: &[MapMode] = &[Insert];

const DEFAULTS: &[(&[MapMode], &str, &str)] = &[
    (NVO, "h", "motion.left"),
    (NVO, "<Left>", "motion.left"),
    (NVO, "<BS>", "motion.left"),
    (NVO, "<C-h>", "motion.left"),
    (NVO, "l", "motion.right"),
    (NVO, "<Right>", "motion.right"),
    (NVO, "<Space>", "motion.right"),
    (NVO, "j", "motion.down"),
    (NVO, "<Down>", "motion.down"),
    (NVO, "<C-j>", "motion.down"),
    (NVO, "<C-n>", "motion.down"),
    (NVO, "gj", "motion.down"),
    (NVO, "k", "motion.up"),
    (NVO, "<Up>", "motion.up"),
    (NVO, "<C-p>", "motion.up"),
    (NVO, "gk", "motion.up"),
    (NVO, "w", "motion.word_forward"),
    (NVO, "W", "motion.big_word_forward"),
    (NVO, "b", "motion.word_backward"),
    (NVO, "B", "motion.big_word_backward"),
    (NVO, "e", "motion.word_end"),
    (NVO, "E", "motion.big_word_end"),
    (NVO, "ge", "motion.word_end_backward"),
    (NVO, "gE", "motion.big_word_end_backward"),
    (NVO, "0", "motion.line_start"),
    (NVO, "<Home>", "motion.line_start"),
    (NVO, "^", "motion.first_non_blank"),
    (NVO, "$", "motion.line_end"),
    (NVO, "<End>", "motion.line_end"),
    (NVO, "G", "motion.goto_line"),
    (NVO, "gg", "motion.goto_first_line"),
    (NVO, "f", "motion.find_forward"),
    (NVO, "F", "motion.find_backward"),
    (NVO, "t", "motion.till_forward"),
    (NVO, "T", "motion.till_backward"),
    (NVO, ";", "motion.repeat_find"),
    (NVO, ",", "motion.repeat_find_reverse"),
    (NVO, "%", "motion.match_pair"),
    (NVO, "}", "motion.paragraph_forward"),
    (NVO, "{", "motion.paragraph_backward"),
    (NVO, "H", "motion.screen_top"),
    (NVO, "M", "motion.screen_middle"),
    (NVO, "L", "motion.screen_bottom"),
    (NVO, "<C-d>", "motion.half_page_down"),
    (NVO, "<C-u>", "motion.half_page_up"),
    (NVO, "<C-f>", "motion.page_down"),
    (NVO, "<PageDown>", "motion.page_down"),
    (NVO, "<C-b>", "motion.page_up"),
    (NVO, "<PageUp>", "motion.page_up"),
    (NVO, "n", "motion.search_next"),
    (NVO, "N", "motion.search_prev"),
    (NVO, "*", "motion.word_search_forward"),
    (NVO, "#", "motion.word_search_backward"),
    (NVO, "+", "motion.next_line"),
    (NVO, "<CR>", "motion.next_line"),
    (NVO, "-", "motion.prev_line"),
    (NVO, "_", "motion.current_line"),
    (NVO, "<Bar>", "motion.column"),
    (NVO, "d", "operator.delete"),
    (NVO, "c", "operator.change"),
    (NVO, "y", "operator.yank"),
    (NVO, ">", "operator.indent"),
    (NVO, "<lt>", "operator.outdent"),
    (NVO, "gu", "operator.lowercase"),
    (NVO, "gU", "operator.uppercase"),
    (NVO, "g~", "operator.toggle_case"),
    (NVO, "gc", "operator.comment"),
    (NVO, "\"", "register.select"),
    (VO, "i", "textobj.inner"),
    (VO, "a", "textobj.around"),
    (NV, "<Esc>", "mode.normal"),
    (NV, "<C-[>", "mode.normal"),
    (NV, "<C-c>", "mode.normal"),
    (NV, "v", "mode.visual"),
    (NV, "V", "mode.visual_line"),
    (NV, "gv", "visual.reselect"),
    (NV, "r", "edit.replace_char"),
    (NV, "J", "edit.join_lines"),
    (NV, "gJ", "edit.join_lines_no_space"),
    (NV, "p", "edit.put_after"),
    (NV, "P", "edit.put_before"),
    (NV, "<Del>", "edit.delete_char"),
    (NV, ":", "cmdline.ex"),
    (NV, "/", "cmdline.search_forward"),
    (NV, "?", "cmdline.search_backward"),
    (NV, "zz", "view.center"),
    (NV, "z.", "view.center"),
    (NV, "zt", "view.top"),
    (NV, "z<CR>", "view.top"),
    (NV, "zb", "view.bottom"),
    (NV, "z-", "view.bottom"),
    (NV, "<C-e>", "view.scroll_down"),
    (NV, "<C-y>", "view.scroll_up"),
    (N, "i", "mode.insert"),
    (N, "a", "mode.append"),
    (N, "I", "mode.insert_line_start"),
    (N, "A", "mode.append_line_end"),
    (N, "o", "edit.open_below"),
    (N, "O", "edit.open_above"),
    (N, "x", "edit.delete_char"),
    (N, "X", "edit.delete_char_before"),
    (N, "s", "edit.substitute_char"),
    (N, "S", "edit.change_lines"),
    (N, "Y", "edit.yank_lines"),
    (N, "D", "edit.delete_to_line_end"),
    (N, "C", "edit.change_to_line_end"),
    (N, "~", "edit.toggle_case_char"),
    (N, ".", "edit.repeat"),
    (N, "u", "edit.undo"),
    (N, "<C-r>", "edit.redo"),
    (V, "o", "visual.swap_ends"),
    (V, "x", "operator.delete"),
    (V, "s", "operator.change"),
    (V, "u", "operator.lowercase"),
    (V, "U", "operator.uppercase"),
    (V, "~", "operator.toggle_case"),
    (V, "Y", "edit.yank_lines"),
    (V, "D", "edit.delete_lines"),
    (V, "X", "edit.delete_lines"),
    (V, "C", "edit.change_lines"),
    (V, "S", "edit.change_lines"),
    (V, "R", "edit.change_lines"),
    (I, "<Esc>", "insert.escape"),
    (I, "<C-[>", "insert.escape"),
    (I, "<C-c>", "insert.escape"),
    (I, "<CR>", "insert.newline"),
    (I, "<C-j>", "insert.newline"),
    (I, "<C-m>", "insert.newline"),
    (I, "<BS>", "insert.backspace"),
    (I, "<C-h>", "insert.backspace"),
    (I, "<Del>", "insert.delete"),
    (I, "<Tab>", "insert.tab"),
    (I, "<C-w>", "insert.delete_word"),
    (I, "<C-u>", "insert.delete_line"),
    (I, "<C-t>", "insert.indent"),
    (I, "<C-d>", "insert.outdent"),
    (I, "<Left>", "insert.left"),
    (I, "<Right>", "insert.right"),
    (I, "<Up>", "insert.up"),
    (I, "<Down>", "insert.down"),
    (I, "<PageUp>", "insert.page_up"),
    (I, "<PageDown>", "insert.page_down"),
    (I, "<Home>", "insert.home"),
    (I, "<End>", "insert.end"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notation_round_trips() {
        let keys = parse_keys("g<C-w><Space><lt>d<Esc><S-Tab><c-space>");
        assert_eq!(
            keys,
            [
                Key::Char('g'),
                Key::Ctrl('w'),
                Key::Char(' '),
                Key::Char('<'),
                Key::Char('d'),
                Key::Esc,
                Key::BackTab,
                Key::Ctrl(' '),
            ]
        );
        assert_eq!(
            format_keys(&keys),
            "g<C-w><Space><lt>d<Esc><S-Tab><C-Space>"
        );
        // Not a key name: the bracket is just a character.
        assert_eq!(
            parse_keys("<x>"),
            [Key::Char('<'), Key::Char('x'), Key::Char('>')]
        );
        assert_eq!(parse_keys("<<"), [Key::Char('<'), Key::Char('<')]);
    }

    #[test]
    fn trie_lookup() {
        let mut trie = KeyTrie::new();
        trie.insert(&parse_keys("g"), 1);
        trie.insert(&parse_keys("gd"), 2);
        trie.insert(&parse_keys("x"), 3);
        assert_eq!(trie.lookup(&parse_keys("x")), Lookup::Found(3));
        assert_eq!(trie.lookup(&parse_keys("g")), Lookup::Pending);
        assert_eq!(trie.lookup(&parse_keys("gd")), Lookup::Found(2));
        assert_eq!(trie.lookup(&parse_keys("gx")), Lookup::None);
        assert_eq!(trie.longest_prefix(&parse_keys("gx")), Some((1, 1)));
        assert_eq!(trie.longest_prefix(&parse_keys("q")), None);

        assert_eq!(trie.remove(&parse_keys("gd")), Some(2));
        assert_eq!(trie.lookup(&parse_keys("g")), Lookup::Found(1));
        assert_eq!(trie.remove(&parse_keys("g")), Some(1));
        assert_eq!(trie.lookup(&parse_keys("g")), Lookup::None);
        assert_eq!(trie.remove(&parse_keys("g")), None);
    }

    #[test]
    fn defaults_name_real_commands() {
        let keymap = Keymap::vim();
        for (modes, keys, command) in DEFAULTS {
            assert!(
                command::builtin(command).is_some(),
                "{keys} is bound to unknown command {command}"
            );
            for mode in *modes {
                assert_eq!(keymap.command(*mode, keys), Some(*command), "{keys}");
            }
        }
    }
}
