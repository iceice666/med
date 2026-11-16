//! Key sequences and what they are bound to.
//!
//! A [`Keymap`] starts out empty, and a host fills it with [`Keymap::bind`]
//! and friends; med does it from the user's init file, whose example is
//! `examples/vim.mlisp`. [`Keymap::vim`] is that stock vim layout as a
//! table of key sequence → command name, kept for tests: the engine's own,
//! and med's check that the example still says the same. What stays in
//! the engine is vim's grammar — counts, registers, operator + motion, the
//! doubled operator (`dd`) — which works the same whatever the keys are.

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

    /// Nothing is bound.
    pub fn is_empty(&self) -> bool {
        self.root.value.is_none() && self.root.children.is_empty()
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

    /// The keys that can come after `keys`, in the order of their
    /// notation. None when `keys` start no binding; with no `keys`, the
    /// first key of every binding.
    pub fn next_keys(&self, keys: &[Key]) -> Vec<NextKey<&T>> {
        let Some(node) = self.node(keys) else {
            return Vec::new();
        };
        let mut next: Vec<NextKey<&T>> = node
            .children
            .iter()
            .map(|(key, child)| NextKey {
                key: key.clone(),
                value: child.value.as_ref(),
                prefix: !child.children.is_empty(),
            })
            .collect();
        next.sort_by_cached_key(|next| format_keys(std::slice::from_ref(&next.key)));
        next
    }

    /// Every binding: its keys and what they are bound to, in no order.
    pub fn bindings(&self) -> Vec<(Vec<Key>, &T)> {
        fn walk<'a, T>(node: &'a Node<T>, keys: &mut Vec<Key>, out: &mut Vec<(Vec<Key>, &'a T)>) {
            if let Some(value) = &node.value {
                out.push((keys.clone(), value));
            }
            for (key, child) in &node.children {
                keys.push(key.clone());
                walk(child, keys, out);
                keys.pop();
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut Vec::new(), &mut out);
        out
    }
}

/// Where a command is bound in a [`Keymap`]: see [`Keymap::keys_for`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeysFor {
    /// The buffers' table, or `None` for the mode's own.
    pub table: Option<String>,
    pub mode: MapMode,
    /// In [`format_keys`] notation.
    pub keys: String,
}

/// A key that can follow the ones typed so far: see [`KeyTrie::next_keys`].
#[derive(Clone, Debug, PartialEq)]
pub struct NextKey<T> {
    pub key: Key,
    /// What the keys are bound to with this one, if that is a binding.
    pub value: Option<T>,
    /// Longer bindings go on from this key.
    pub prefix: bool,
}

/// What keys are bound to, as a host that lists bindings sees it.
#[derive(Clone, Debug, PartialEq)]
pub enum Bound {
    /// A command, by name: the engine's or the host's.
    Command(String),
    /// Other keys, typed in their place.
    Keys(Vec<Key>),
}

/// Which table a binding lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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

/// The engine's key bindings, one table per [`MapMode`], and the tables
/// of buffers' own: each by its name, again one per mode, looked up first
/// in the buffers that say they are in it ([`Vim::tables`](crate::Vim)).
pub struct Keymap {
    maps: [KeyTrie<Action>; 4],
    tables: HashMap<String, [KeyTrie<Action>; 4]>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::empty()
    }
}

impl Keymap {
    /// No bindings at all: every key but text typed in insert mode is dead.
    pub fn empty() -> Self {
        Self {
            maps: std::array::from_fn(|_| KeyTrie::new()),
            tables: HashMap::new(),
        }
    }

    /// The stock vim layout, [`VIM_LAYOUT`].
    pub fn vim() -> Self {
        let mut keymap = Self::empty();
        for (modes, keys, command) in VIM_LAYOUT {
            for mode in *modes {
                keymap.bind(*mode, keys, command);
            }
        }
        keymap
    }

    /// A buffer's table, or with no name the mode's own; made if need be.
    fn map(&mut self, table: Option<&str>, mode: MapMode) -> &mut KeyTrie<Action> {
        match table {
            None => &mut self.maps[mode as usize],
            Some(name) => {
                let tables = self.tables.entry(name.to_string()).or_default();
                &mut tables[mode as usize]
            }
        }
    }

    fn get_map(&self, table: Option<&str>, mode: MapMode) -> Option<&KeyTrie<Action>> {
        match table {
            None => Some(&self.maps[mode as usize]),
            Some(name) => Some(&self.tables.get(name)?[mode as usize]),
        }
    }

    /// Have a table for buffers to be in, by `name`, with nothing bound in
    /// it yet if there was none.
    pub fn define_table(&mut self, name: &str) {
        self.tables.entry(name.to_string()).or_default();
    }

    /// Whether there is a buffers' table called `name`.
    pub fn has_table(&self, name: &str) -> bool {
        self.tables.contains_key(name)
    }

    /// The names of the buffers' tables, in no order.
    pub fn table_names(&self) -> impl Iterator<Item = &str> {
        self.tables.keys().map(String::as_str)
    }

    /// Bind `keys` (in [`parse_keys`] notation) to a command: one of
    /// [`command_names`](crate::command_names), or any other name, which is
    /// then the host's to run and arrives as
    /// [`Effect::Command`](crate::Effect::Command).
    pub fn bind(&mut self, mode: MapMode, keys: &str, command: &str) {
        self.bind_in(None, mode, keys, command);
    }

    /// [`Keymap::bind`] in a buffers' table, or with none the mode's own.
    pub fn bind_in(&mut self, table: Option<&str>, mode: MapMode, keys: &str, command: &str) {
        let action = match command::builtin(command) {
            Some(builtin) => Action::Builtin(builtin),
            None => Action::Host(command.into()),
        };
        self.map(table, mode).insert(&parse_keys(keys), action);
    }

    /// Bind `keys` to other keys, which are typed in their place and are
    /// themselves subject to the keymap (vim's `:map`, not `:noremap`).
    pub fn bind_keys(&mut self, mode: MapMode, keys: &str, to: &str) {
        self.bind_keys_in(None, mode, keys, to);
    }

    /// [`Keymap::bind_keys`] in a buffers' table, or the mode's own.
    pub fn bind_keys_in(&mut self, table: Option<&str>, mode: MapMode, keys: &str, to: &str) {
        let action = Action::Keys(parse_keys(to).into());
        self.map(table, mode).insert(&parse_keys(keys), action);
    }

    /// Remove a binding. False if `keys` were not bound.
    pub fn unbind(&mut self, mode: MapMode, keys: &str) -> bool {
        self.unbind_in(None, mode, keys)
    }

    /// [`Keymap::unbind`] in a buffers' table, or the mode's own.
    pub fn unbind_in(&mut self, table: Option<&str>, mode: MapMode, keys: &str) -> bool {
        if table.is_some_and(|name| !self.has_table(name)) {
            return false;
        }
        self.map(table, mode).remove(&parse_keys(keys)).is_some()
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

    /// The keys that can come after `keys` in `mode`, with what each is
    /// then bound to: what there is to show someone who has typed `keys`
    /// and wonders what next.
    pub fn next_keys(&self, mode: MapMode, keys: &str) -> Vec<NextKey<Bound>> {
        self.next_keys_in(None, mode, keys)
    }

    /// [`Keymap::next_keys`] in a buffers' table, or the mode's own.
    pub fn next_keys_in(
        &self,
        table: Option<&str>,
        mode: MapMode,
        keys: &str,
    ) -> Vec<NextKey<Bound>> {
        let Some(map) = self.get_map(table, mode) else {
            return Vec::new();
        };
        let bound = |action: &Action| match action {
            Action::Builtin(builtin) => {
                command::name(*builtin).map(|name| Bound::Command(name.to_string()))
            }
            Action::Host(name) => Some(Bound::Command(name.to_string())),
            Action::Keys(keys) => Some(Bound::Keys(keys.to_vec())),
        };
        map.next_keys(&parse_keys(keys))
            .into_iter()
            .map(|next| NextKey {
                key: next.key,
                value: next.value.and_then(bound),
                prefix: next.prefix,
            })
            .collect()
    }

    /// Every binding of `command`, in the modes' tables and the buffers',
    /// the modes' first, then by table name, mode and keys. Keys bound to
    /// other keys are not followed.
    pub fn keys_for(&self, command: &str) -> Vec<KeysFor> {
        let is_it = |action: &Action| match action {
            Action::Builtin(builtin) => command::name(*builtin) == Some(command),
            Action::Host(name) => name.as_ref() == command,
            Action::Keys(_) => false,
        };
        let tables = std::iter::once((None, &self.maps))
            .chain(self.tables.iter().map(|(name, maps)| (Some(name), maps)));
        let mut found = Vec::new();
        for (table, maps) in tables {
            for mode in MapMode::ALL {
                for (keys, action) in maps[mode as usize].bindings() {
                    if is_it(action) {
                        found.push(KeysFor {
                            table: table.cloned(),
                            mode,
                            keys: format_keys(&keys),
                        });
                    }
                }
            }
        }
        found.sort();
        found
    }

    /// Nothing is bound in `mode`.
    pub fn is_empty(&self, mode: MapMode) -> bool {
        self.maps[mode as usize].is_empty()
    }

    /// The tables of `tables` that there are, in `mode`.
    fn buffer_maps<'a>(
        &'a self,
        tables: &'a [String],
        mode: MapMode,
    ) -> impl Iterator<Item = &'a KeyTrie<Action>> {
        tables
            .iter()
            .filter_map(move |name| self.tables.get(name))
            .map(move |maps| &maps[mode as usize])
    }

    /// What `keys` are in `mode` for a buffer in `tables`: the first of
    /// them that has anything to say about the keys decides, and then the
    /// mode's own table.
    pub(crate) fn lookup(&self, tables: &[String], mode: MapMode, keys: &[Key]) -> Lookup<Action> {
        let buffer = self.buffer_maps(tables, mode).map(|map| map.lookup(keys));
        if let Some(found) = buffer.into_iter().find(|found| *found != Lookup::None) {
            return found;
        }
        match self.fallback(mode, keys) {
            Some(action) => Lookup::Found(action),
            None => self.maps[mode as usize].lookup(keys),
        }
    }

    /// The longest leading part of `keys` that is bound, in the first of
    /// the tables to have one.
    pub(crate) fn longest_prefix(
        &self,
        tables: &[String],
        mode: MapMode,
        keys: &[Key],
    ) -> Option<(usize, Action)> {
        if let Some(found) = self
            .buffer_maps(tables, mode)
            .find_map(|map| map.longest_prefix(keys))
        {
            return Some(found);
        }
        match self.fallback(mode, keys.get(..1).unwrap_or_default()) {
            Some(action) => Some((1, action)),
            None => self.maps[mode as usize].longest_prefix(keys),
        }
    }

    /// While nothing at all is bound in normal mode, `:` still opens the
    /// command line: an editor whose init file binds no key can then be
    /// given some (`:install_default_config`, `:eval`). The first binding
    /// of normal mode, whatever it is, takes this away.
    fn fallback(&self, mode: MapMode, keys: &[Key]) -> Option<Action> {
        let bare = mode == MapMode::Normal && self.is_empty(MapMode::Normal);
        (bare && keys == [Key::Char(':')])
            .then(|| command::builtin("cmdline.ex").map(Action::Builtin))
            .flatten()
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

/// The stock vim layout: modes, key sequence, command. `examples/vim.mlisp`
/// is the same table as an init file binds it.
pub const VIM_LAYOUT: &[(&[MapMode], &str, &str)] = &[
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
    (NVO, "'", "motion.mark_line"),
    (NVO, "`", "motion.mark"),
    (NVO, "]f", "motion.next_function"),
    (NVO, "[f", "motion.prev_function"),
    (NVO, "]t", "motion.next_type"),
    (NVO, "[t", "motion.prev_type"),
    (NVO, "]a", "motion.next_argument"),
    (NVO, "[a", "motion.prev_argument"),
    (NVO, "]c", "motion.next_comment"),
    (NVO, "[c", "motion.prev_comment"),
    (VO, "i", "textobj.inner"),
    (VO, "a", "textobj.around"),
    (NV, "<Esc>", "mode.normal"),
    (NV, "<C-[>", "mode.normal"),
    (NV, "<C-c>", "mode.normal"),
    (NV, "v", "mode.visual"),
    (NV, "V", "mode.visual_line"),
    (NV, "<C-v>", "mode.visual_block"),
    (NV, "]x", "select.expand"),
    (NV, "[x", "select.shrink"),
    (NV, "@", "macro.play"),
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
    (N, "m", "mark.set"),
    (N, "q", "macro.record"),
    // `ys`, `ds`, `cs`: the `s` comes after the operator.
    (&[Operator], "s", "surround.operator"),
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
    (V, "S", "surround.add"),
    (V, "I", "visual.insert"),
    (V, "A", "visual.append"),
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

        let next = |keys: &str| -> Vec<(String, Option<i32>, bool)> {
            trie.next_keys(&parse_keys(keys))
                .into_iter()
                .map(|next| (format_keys(&[next.key]), next.value.copied(), next.prefix))
                .collect()
        };
        assert_eq!(
            next(""),
            [
                ("g".to_string(), Some(1), true),
                ("x".to_string(), Some(3), false)
            ]
        );
        assert_eq!(next("g"), [("d".to_string(), Some(2), false)]);
        assert_eq!(next("x"), []);
        assert_eq!(next("q"), []);

        assert_eq!(trie.remove(&parse_keys("gd")), Some(2));
        assert_eq!(trie.lookup(&parse_keys("g")), Lookup::Found(1));
        assert_eq!(trie.remove(&parse_keys("g")), Some(1));
        assert_eq!(trie.lookup(&parse_keys("g")), Lookup::None);
        assert_eq!(trie.remove(&parse_keys("g")), None);
    }

    #[test]
    fn layout_names_real_commands() {
        assert_eq!(Keymap::default().next_keys(MapMode::Normal, ""), []);
        let keymap = Keymap::vim();
        for (modes, keys, command) in VIM_LAYOUT {
            assert!(
                command::builtin(command).is_some(),
                "{keys} is bound to unknown command {command}"
            );
            for mode in *modes {
                assert_eq!(keymap.command(*mode, keys), Some(*command), "{keys}");
            }
        }
    }

    #[test]
    fn lists_the_keys_that_can_follow() {
        let mut keymap = Keymap::vim();
        keymap.bind(MapMode::Normal, "gx", "host.open_link");
        keymap.bind_keys(MapMode::Normal, "gY", "y$");
        let after_g = keymap.next_keys(MapMode::Normal, "g");
        let found = |key: char| {
            let next = after_g.iter().find(|next| next.key == Key::Char(key));
            next.unwrap_or_else(|| panic!("no g{key}")).clone()
        };
        assert_eq!(
            found('g').value,
            Some(Bound::Command("motion.goto_first_line".into()))
        );
        assert_eq!(
            found('x').value,
            Some(Bound::Command("host.open_link".into()))
        );
        assert_eq!(found('Y').value, Some(Bound::Keys(parse_keys("y$"))));
        assert!(!found('g').prefix);
        // In the order of their notation, whatever order they were bound in.
        let keys: Vec<String> = after_g
            .iter()
            .map(|next| format_keys(std::slice::from_ref(&next.key)))
            .collect();
        assert!(keys.is_sorted(), "{keys:?}");

        // `g` is no binding of its own, only the start of some.
        let first = keymap.next_keys(MapMode::Normal, "");
        let g = first.iter().find(|next| next.key == Key::Char('g'));
        assert_eq!(g.map(|g| (g.value.clone(), g.prefix)), Some((None, true)));
        assert_eq!(keymap.next_keys(MapMode::Insert, "g"), []);
    }

    #[test]
    fn finds_the_keys_of_a_command() {
        let mut keymap = Keymap::empty();
        keymap.bind(MapMode::Visual, "<Space>w", "host.save");
        keymap.bind(MapMode::Normal, "<Space>w", "host.save");
        keymap.bind(MapMode::Normal, "w", "motion.word_forward");
        keymap.bind_in(Some("listing"), MapMode::Insert, "<C-s>", "host.save");
        // Keys that stand for other keys are not the command's own.
        keymap.bind_keys(MapMode::Normal, "W", "<Space>w");
        let found = |table: Option<&str>, mode, keys: &str| KeysFor {
            table: table.map(str::to_string),
            mode,
            keys: keys.to_string(),
        };
        assert_eq!(
            keymap.keys_for("host.save"),
            [
                found(None, MapMode::Normal, "<Space>w"),
                found(None, MapMode::Visual, "<Space>w"),
                found(Some("listing"), MapMode::Insert, "<C-s>"),
            ]
        );
        assert_eq!(
            keymap.keys_for("motion.word_forward"),
            [found(None, MapMode::Normal, "w")]
        );
        assert_eq!(keymap.keys_for("host.nothing"), []);
    }
}
