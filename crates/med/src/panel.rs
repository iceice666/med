//! Panels drawn from a layout file.
//!
//! A layout is a tree of rows, columns, text and lists, written in JSON. It
//! says how a panel looks. What the panel shows and does stays with its
//! view, the layout's [`Host`]: the view hands out named values (`{name}`),
//! flags (`selected`) and lists (`entries`), and runs the command a node
//! names when it is clicked.
//!
//! The built-in layouts are in `panels/`. A file of the same name in
//! `<config>/panels/` replaces one, where `<config>` is `$MED_CONFIG_DIR`,
//! `$XDG_CONFIG_HOME/med` or `~/.config/med`.

use gpui::component::{ActiveTheme as _, Theme};
use gpui::prelude::*;
use gpui::*;
use serde::{Deserialize, Deserializer, de};
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;

/// The layouts that ship with the editor: name, JSON.
const BUILT_IN: &[(&str, &str)] = &[("file_tree", include_str!("panels/file_tree.json"))];

// ---- the layout tree ---------------------------------------------------

/// One box of a layout.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    #[serde(rename = "type")]
    kind: Kind,
    #[serde(default)]
    style: Style,
    /// Styles added while their flags hold; a later one wins.
    #[serde(default)]
    when: Vec<Rule>,
    /// Style added under the pointer.
    #[serde(default)]
    hover: Option<Style>,
    /// Flags that must hold for the node to be there at all.
    #[serde(default, rename = "if")]
    only_if: Vec<Flag>,
    /// The command a click runs.
    #[serde(default)]
    on_click: Option<SharedString>,
    /// `row` and `col`: what is inside.
    #[serde(default)]
    children: Vec<Node>,
    /// `text`: what it says, `{name}` standing for a value of the host.
    #[serde(default)]
    text: Option<SharedString>,
    /// `list`: which of the host's lists to show.
    #[serde(default)]
    source: Option<SharedString>,
    /// `list`: how one row of it looks.
    #[serde(default, deserialize_with = "shared")]
    item: Option<Rc<Node>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Row,
    Col,
    Text,
    /// Rows of equal height, of which only the visible ones are built.
    List,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    #[serde(rename = "if")]
    flags: Vec<Flag>,
    style: Style,
}

/// A flag of the host, or with a leading `!` its absence.
#[derive(Debug)]
struct Flag {
    name: String,
    wanted: bool,
}

impl<'de> Deserialize<'de> for Flag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(match text.strip_prefix('!') {
            Some(name) => Flag {
                name: name.to_string(),
                wanted: false,
            },
            None => Flag {
                name: text,
                wanted: true,
            },
        })
    }
}

fn shared<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Rc<Node>>, D::Error> {
    Ok(Option::<Node>::deserialize(deserializer)?.map(Rc::new))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Style {
    width: Option<Size>,
    height: Option<Size>,
    /// Take the room that is left over.
    grow: Option<bool>,
    /// `false` keeps the box at its size when room runs out.
    shrink: Option<bool>,
    padding_x: Option<Number>,
    padding_y: Option<Number>,
    padding_left: Option<Number>,
    padding_right: Option<Number>,
    /// Space between children.
    gap: Option<Number>,
    /// Children across the direction of the row or column.
    align: Option<Align>,
    /// Children along it.
    justify: Option<Justify>,
    bg: Option<Color>,
    color: Option<Color>,
    border_top: Option<f32>,
    border_right: Option<f32>,
    border_bottom: Option<f32>,
    border_left: Option<f32>,
    border_color: Option<Color>,
    bold: Option<bool>,
    nowrap: Option<bool>,
}

/// Pixels, or `"full"` for all of the parent.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Size {
    Pixels(f32),
    Named(NamedSize),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NamedSize {
    Full,
}

/// Pixels, fixed or growing with a number of the host:
/// `{ "base": 10, "step": 14, "times": "depth" }`.
#[derive(Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum Number {
    Fixed(f32),
    Scaled {
        #[serde(default)]
        base: f32,
        step: f32,
        times: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Align {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Justify {
    Start,
    Center,
    End,
    Between,
}

/// A color of the theme by name, or `#rrggbb`.
#[derive(Clone, Copy, Debug)]
enum Color {
    Theme(ThemeColor),
    Fixed(Hsla),
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ThemeColor {
    Accent,
    AccentForeground,
    Background,
    Border,
    Danger,
    Foreground,
    Info,
    Link,
    ListActive,
    ListHover,
    Muted,
    MutedForeground,
    Popover,
    PopoverForeground,
    Primary,
    Selection,
    Sidebar,
    SidebarBorder,
    SidebarForeground,
    Success,
    Warning,
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.starts_with('#') {
            return Rgba::try_from(text.as_str())
                .map(|color| Color::Fixed(color.into()))
                .map_err(|_| de::Error::custom(format!("{text} is not a color")));
        }
        ThemeColor::deserialize(de::value::StrDeserializer::new(&text)).map(Color::Theme)
    }
}

impl Color {
    fn get(self, theme: &Theme) -> Hsla {
        let name = match self {
            Color::Fixed(color) => return color,
            Color::Theme(name) => name,
        };
        match name {
            ThemeColor::Accent => theme.accent,
            ThemeColor::AccentForeground => theme.accent_foreground,
            ThemeColor::Background => theme.background,
            ThemeColor::Border => theme.border,
            ThemeColor::Danger => theme.danger,
            ThemeColor::Foreground => theme.foreground,
            ThemeColor::Info => theme.info,
            ThemeColor::Link => theme.link,
            ThemeColor::ListActive => theme.list_active,
            ThemeColor::ListHover => theme.list_hover,
            ThemeColor::Muted => theme.muted,
            ThemeColor::MutedForeground => theme.muted_foreground,
            ThemeColor::Popover => theme.popover,
            ThemeColor::PopoverForeground => theme.popover_foreground,
            ThemeColor::Primary => theme.primary,
            ThemeColor::Selection => theme.selection,
            ThemeColor::Sidebar => theme.sidebar,
            ThemeColor::SidebarBorder => theme.sidebar_border,
            ThemeColor::SidebarForeground => theme.sidebar_foreground,
            ThemeColor::Success => theme.success,
            ThemeColor::Warning => theme.warning,
        }
    }
}

impl Node {
    /// Read a layout, refusing what would not draw as written.
    pub fn parse(json: &str) -> Result<Rc<Node>, String> {
        let node: Node = serde_json::from_str(json).map_err(|error| error.to_string())?;
        node.check("layout")?;
        Ok(Rc::new(node))
    }

    /// Whether the fields that are set are ones this kind of node has.
    fn check(&self, at: &str) -> Result<(), String> {
        let has = [
            ("children", !self.children.is_empty()),
            ("text", self.text.is_some()),
            ("source", self.source.is_some()),
            ("item", self.item.is_some()),
            ("hover", self.hover.is_some()),
            ("on_click", self.on_click.is_some()),
        ];
        let (name, needs, allows): (_, &[&str], &[&str]) = match self.kind {
            Kind::Row => ("row", &[], &["children", "hover", "on_click"]),
            Kind::Col => ("col", &[], &["children", "hover", "on_click"]),
            Kind::Text => ("text", &["text"], &["hover", "on_click"]),
            Kind::List => ("list", &["source", "item"], &[]),
        };
        for (field, set) in has {
            if set && !needs.contains(&field) && !allows.contains(&field) {
                return Err(format!("{at}: a {name} has no \"{field}\""));
            }
            if !set && needs.contains(&field) {
                return Err(format!("{at}: a {name} needs \"{field}\""));
            }
        }
        for (index, child) in self.children.iter().enumerate() {
            child.check(&format!("{at}.children[{index}]"))?;
        }
        if let Some(item) = &self.item {
            item.check(&format!("{at}.item"))?;
        }
        Ok(())
    }
}

// ---- what a host tells a layout ----------------------------------------

/// Named values a layout can use: text for `{name}`, flags for `if`, and
/// numbers for sizes that scale.
#[derive(Clone, Default)]
pub struct Vars {
    text: Vec<(&'static str, SharedString)>,
    flags: Vec<(&'static str, bool)>,
    numbers: Vec<(&'static str, f32)>,
}

impl Vars {
    pub fn text(mut self, name: &'static str, value: impl Into<SharedString>) -> Self {
        self.text.push((name, value.into()));
        self
    }

    pub fn flag(mut self, name: &'static str, value: bool) -> Self {
        self.flags.push((name, value));
        self
    }

    pub fn number(mut self, name: &'static str, value: f32) -> Self {
        self.numbers.push((name, value));
        self
    }

    /// Add `inner`'s values, hiding the ones of the same name.
    fn extend(&mut self, inner: Vars) {
        self.text.extend(inner.text);
        self.flags.extend(inner.flags);
        self.numbers.extend(inner.numbers);
    }

    fn holds(&self, flags: &[Flag]) -> bool {
        flags.iter().all(|flag| {
            let set = self
                .flags
                .iter()
                .rev()
                .find(|(name, _)| *name == flag.name)
                .is_some_and(|(_, set)| *set);
            set == flag.wanted
        })
    }

    fn pixels(&self, number: &Number) -> Pixels {
        match number {
            Number::Fixed(value) => px(*value),
            Number::Scaled { base, step, times } => {
                let times = self
                    .numbers
                    .iter()
                    .rev()
                    .find(|(name, _)| name == times)
                    .map_or(0., |(_, value)| *value);
                px(base + step * times)
            }
        }
    }

    /// `template` with each `{name}` replaced by its value. A name the host
    /// does not have is left as written.
    fn expand(&self, template: &SharedString) -> SharedString {
        if !template.contains('{') {
            return template.clone();
        }
        let mut out = String::new();
        let mut rest = template.as_ref();
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let value = after.find('}').and_then(|close| {
                let name = &after[..close];
                let value = self.text.iter().rev().find(|(known, _)| *known == name)?;
                Some((close, &value.1))
            });
            match value {
                Some((close, value)) => {
                    out.push_str(value);
                    rest = &after[close + 1..];
                }
                None => {
                    out.push('{');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out.into()
    }
}

/// A view whose looks come from a layout.
pub trait Host: 'static + Sized {
    /// The values every node of the layout can use.
    fn vars(&self, window: &Window, cx: &App) -> Vars;

    /// How many rows the list `source` has.
    fn row_count(&self, source: &str) -> usize;

    /// The values of one row, on top of [`Host::vars`].
    fn row_vars(&self, source: &str, index: usize, cx: &App) -> Vars;

    /// Where the list `source` keeps its scroll position.
    fn scroll_handle(&self, source: &str) -> Option<&UniformListScrollHandle>;

    /// A node was clicked: run its command. `row` is the list and row the
    /// node is in.
    fn click(
        &mut self,
        command: &str,
        row: Option<(&str, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    );
}

// ---- drawing -----------------------------------------------------------

impl Style {
    fn apply<E: Styled>(&self, mut el: E, vars: &Vars, theme: &Theme) -> E {
        match &self.width {
            Some(Size::Pixels(width)) => el = el.w(px(*width)),
            Some(Size::Named(NamedSize::Full)) => el = el.w_full(),
            None => {}
        }
        match &self.height {
            Some(Size::Pixels(height)) => el = el.h(px(*height)),
            Some(Size::Named(NamedSize::Full)) => el = el.h_full(),
            None => {}
        }
        if self.grow == Some(true) {
            el = el.flex_1().min_w_0().min_h_0();
        }
        if self.shrink == Some(false) {
            el = el.flex_shrink_0();
        }
        if let Some(padding) = &self.padding_x {
            el = el.px(vars.pixels(padding));
        }
        if let Some(padding) = &self.padding_y {
            el = el.py(vars.pixels(padding));
        }
        if let Some(padding) = &self.padding_left {
            el = el.pl(vars.pixels(padding));
        }
        if let Some(padding) = &self.padding_right {
            el = el.pr(vars.pixels(padding));
        }
        if let Some(gap) = &self.gap {
            el = el.gap(vars.pixels(gap));
        }
        match self.align {
            Some(Align::Start) => el = el.items_start(),
            Some(Align::Center) => el = el.items_center(),
            Some(Align::End) => el = el.items_end(),
            None => {}
        }
        match self.justify {
            Some(Justify::Start) => el = el.justify_start(),
            Some(Justify::Center) => el = el.justify_center(),
            Some(Justify::End) => el = el.justify_end(),
            Some(Justify::Between) => el = el.justify_between(),
            None => {}
        }
        if let Some(color) = self.bg {
            el = el.bg(color.get(theme));
        }
        if let Some(color) = self.color {
            el = el.text_color(color.get(theme));
        }
        if let Some(width) = self.border_top {
            el = el.border_t(px(width));
        }
        if let Some(width) = self.border_right {
            el = el.border_r(px(width));
        }
        if let Some(width) = self.border_bottom {
            el = el.border_b(px(width));
        }
        if let Some(width) = self.border_left {
            el = el.border_l(px(width));
        }
        if let Some(color) = self.border_color {
            el = el.border_color(color.get(theme));
        }
        if self.bold == Some(true) {
            el = el.font_weight(FontWeight::SEMIBOLD);
        }
        if self.nowrap == Some(true) {
            el = el.whitespace_nowrap();
        }
        el
    }
}

/// Where in a layout a node is being built.
struct Place<'a> {
    vars: &'a Vars,
    /// The list and row the node is in.
    row: Option<(&'a SharedString, usize)>,
    /// Tells the node from its siblings.
    key: usize,
    /// The node is a whole row of a list.
    is_row: bool,
}

/// Build `layout` for `host`. Call it from the host's `render`.
pub fn render<H: Host>(layout: &Node, host: &H, window: &Window, cx: &Context<H>) -> AnyElement {
    let vars = host.vars(window, cx);
    let place = Place {
        vars: &vars,
        row: None,
        key: 0,
        is_row: false,
    };
    build(layout, host, place, cx).unwrap_or_else(|| Empty.into_any_element())
}

fn build<H: Host>(node: &Node, host: &H, place: Place, cx: &Context<H>) -> Option<AnyElement> {
    let vars = place.vars;
    if !vars.holds(&node.only_if) {
        return None;
    }
    let theme = cx.theme();
    if node.kind == Kind::List {
        return Some(build_list(node, host, vars, cx));
    }

    let mut el = match node.kind {
        Kind::Row => div().flex(),
        Kind::Col => div().flex().flex_col(),
        Kind::Text | Kind::List => div(),
    };
    el = node.style.apply(el, vars, theme);
    for rule in &node.when {
        if vars.holds(&rule.flags) {
            el = rule.style.apply(el, vars, theme);
        }
    }
    if let Some(hover) = &node.hover {
        el = el.hover(|style| hover.apply(style, vars, theme));
    }
    if let Some(text) = &node.text {
        el = el.child(vars.expand(text));
    }
    el = el.children(node.children.iter().enumerate().filter_map(|(key, child)| {
        let place = Place {
            vars,
            row: place.row,
            key,
            is_row: false,
        };
        build(child, host, place, cx)
    }));

    // Only a node that is clicked needs an id, and a row: without one, the
    // ids inside two rows would be the same.
    if node.on_click.is_none() && !place.is_row {
        return Some(el.into_any_element());
    }
    let mut el = el.id(place.key);
    if let Some(command) = node.on_click.clone() {
        let row = place.row.map(|(source, index)| (source.clone(), index));
        el = el.on_click(cx.listener(move |host, _: &ClickEvent, window, cx| {
            let row = row
                .as_ref()
                .map(|(source, index)| (source.as_ref(), *index));
            host.click(&command, row, window, cx);
        }));
    }
    Some(el.into_any_element())
}

fn build_list<H: Host>(node: &Node, host: &H, vars: &Vars, cx: &Context<H>) -> AnyElement {
    let theme = cx.theme();
    // `Node::check` saw to both.
    let (Some(source), Some(item)) = (node.source.clone(), node.item.clone()) else {
        return Empty.into_any_element();
    };
    let rows = cx.processor({
        let source = source.clone();
        move |host: &mut H, range: Range<usize>, window, cx| {
            let panel = host.vars(window, cx);
            range
                .map(|index| {
                    let mut vars = panel.clone();
                    vars.extend(host.row_vars(&source, index, cx));
                    let place = Place {
                        vars: &vars,
                        row: Some((&source, index)),
                        key: index,
                        is_row: true,
                    };
                    // The list counts on one element per row, shown or not.
                    build(&item, host, place, cx).unwrap_or_else(|| div().into_any_element())
                })
                .collect::<Vec<_>>()
        }
    });
    let mut list = uniform_list(source.clone(), host.row_count(&source), rows);
    if let Some(handle) = host.scroll_handle(&source) {
        list = list.track_scroll(handle);
    }
    list = node.style.apply(list, vars, theme);
    for rule in &node.when {
        if vars.holds(&rule.flags) {
            list = rule.style.apply(list, vars, theme);
        }
    }
    list.into_any_element()
}

// ---- the layouts in use ------------------------------------------------

/// Every panel's layout, by name.
pub struct Layouts {
    layouts: HashMap<&'static str, Rc<Node>>,
    /// What was wrong with the user's files when they were last read.
    pub problems: Vec<String>,
}

impl Global for Layouts {}

impl Layouts {
    fn built_in() -> Self {
        let layouts = BUILT_IN
            .iter()
            .map(|(name, json)| {
                let layout = Node::parse(json)
                    .unwrap_or_else(|error| panic!("built-in layout {name}: {error}"));
                (*name, layout)
            })
            .collect();
        Self {
            layouts,
            problems: Vec::new(),
        }
    }

    /// The layout of the panel `name`, which must be a built-in one.
    pub fn get(&self, name: &str) -> Rc<Node> {
        self.layouts[name].clone()
    }

    /// Use `layout` for the panel `name` from now on. Windows already open
    /// pick it up when they are next drawn ([`App::refresh_windows`]).
    pub fn set(&mut self, name: &str, layout: Rc<Node>) {
        if let Some(slot) = self.layouts.get_mut(name) {
            *slot = layout;
        }
    }

    /// Replace each layout that has a `<name>.json` in `dir`. A file that
    /// cannot be used leaves the built-in layout in place.
    pub fn load_dir(&mut self, dir: &Path) {
        for (name, _) in BUILT_IN {
            let path = dir.join(format!("{name}.json"));
            let json = match std::fs::read_to_string(&path) {
                Ok(json) => json,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    self.problems.push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            match Node::parse(&json) {
                Ok(layout) => self.set(name, layout),
                Err(error) => self.problems.push(format!("{}: {error}", path.display())),
            }
        }
    }
}

/// Start out with the built-in layouts.
pub fn init(cx: &mut App) {
    cx.set_global(Layouts::built_in());
}

/// Read the user's layouts again, on top of the built-in ones.
pub fn load_user_layouts(cx: &mut App) {
    let mut layouts = Layouts::built_in();
    if let Some(dir) = crate::config_dir() {
        layouts.load_dir(&dir.join("panels"));
    }
    cx.set_global(layouts);
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in GPUI's `test` attribute.
    use super::{Flag, Kind, Layouts, Node, Number, Vars};
    use gpui::px;

    fn vars() -> Vars {
        Vars::default()
            .text("name", "main.rs")
            .flag("selected", true)
            .flag("dir", false)
            .number("depth", 2.)
    }

    #[test]
    fn built_in_layouts_are_valid() {
        Layouts::built_in();
    }

    #[test]
    fn says_what_is_wrong_with_a_layout() {
        let error = |json: &str| Node::parse(json).unwrap_err();
        assert_eq!(
            error(r#"{"type":"col","children":[{"type":"text"}]}"#),
            "layout.children[0]: a text needs \"text\""
        );
        assert_eq!(
            error(r#"{"type":"list","source":"entries","item":{"type":"row","text":"x"}}"#),
            "layout.item: a row has no \"text\""
        );
        assert!(error(r#"{"type":"row","colour":"red"}"#).contains("unknown field `colour`"));
        assert!(error(r#"{"type":"row","style":{"bg":"sidebr"}}"#).contains("unknown variant"));
        assert!(error(r##"{"type":"row","style":{"bg":"#12"}}"##).contains("not a color"));
        assert!(Node::parse(r##"{"type":"row","style":{"bg":"#1e1e2e"}}"##).is_ok());
    }

    #[test]
    fn values_fill_in_text_flags_and_sizes() {
        let vars = vars();
        assert_eq!(
            vars.expand(&"{name} ({nope}) {".into()),
            "main.rs ({nope}) {"
        );

        let flags = |json: &str| serde_json::from_str::<Vec<Flag>>(json).unwrap();
        assert!(vars.holds(&flags(r#"["selected", "!dir"]"#)));
        assert!(!vars.holds(&flags(r#"["selected", "dir"]"#)));
        assert!(vars.holds(&flags(r#"["!unknown"]"#)));

        let number = |json: &str| serde_json::from_str::<Number>(json).unwrap();
        assert_eq!(vars.pixels(&number("8")), px(8.));
        assert_eq!(
            vars.pixels(&number(r#"{"base":10,"step":14,"times":"depth"}"#)),
            px(38.)
        );

        // A row's values hide the panel's of the same name.
        let mut inner = vars.clone();
        inner.extend(Vars::default().flag("selected", false));
        assert!(inner.holds(&flags(r#"["!selected"]"#)));
    }

    #[test]
    fn a_users_file_replaces_a_layout_unless_it_is_broken() {
        let dir = std::env::temp_dir().join(format!("med-panel-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("file_tree.json");

        std::fs::write(&file, r#"{"type":"text","text":"mine"}"#).unwrap();
        let mut layouts = Layouts::built_in();
        layouts.load_dir(&dir);
        assert!(layouts.problems.is_empty());
        assert_eq!(layouts.get("file_tree").kind, Kind::Text);

        std::fs::write(&file, r#"{"type":"text"}"#).unwrap();
        let mut layouts = Layouts::built_in();
        layouts.load_dir(&dir);
        assert_eq!(layouts.problems.len(), 1);
        assert!(layouts.problems[0].ends_with("a text needs \"text\""));
        assert_eq!(layouts.get("file_tree").kind, Kind::Col);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
