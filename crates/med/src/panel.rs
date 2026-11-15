//! Panels drawn from a layout written in Lisp.
//!
//! A layout is a function. Each time the panel is drawn it is called with
//! what the panel has to show, and gives back a tree of rows, columns,
//! text and lists of rows, made with the functions defined here:
//!
//! ```lisp
//! (set-layout 'tree
//!   (fn (panel)
//!     (col :width 'full :height 'full :bg 'sidebar
//!       (text (get panel :root-name) :bold true)
//!       (rows 'entries :grow true
//!         (fn (entry)
//!           (row :height 24 :on-click "tree.open"
//!                :padding-left (* 14 (get entry :depth))
//!             (text (get entry :name))))))))
//! ```
//!
//! What a panel shows and does stays with its view, the layout's [`Host`].
//! The view hands out named values (`panel` and `entry` above) and is told
//! of clicks. Because a layout is a program, what depends on those values
//! is an ordinary expression: there is no notation for conditions or
//! sizes beyond the language's own.
//!
//! The built-in layouts are in `panels/`; `set-layout` in the init file
//! replaces one.

use crate::script;
use gpui::component::{ActiveTheme as _, Theme};
use gpui::prelude::*;
use gpui::*;
use med_lisp::{Arity, Error, ErrorKind, Interp, Value};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

/// The panels that have a layout.
const PANELS: &[&str] = &["tree"];

/// The layouts the editor starts out with, as a program that sets them.
pub(crate) const BUILT_IN: &str = include_str!("panels/tree.lisp");

// ---- the tree a layout gives back --------------------------------------

/// One box of a panel, as a layout described it.
pub struct Node {
    style: Style,
    /// Style added under the pointer.
    hover: Option<Style>,
    on_click: Option<Click>,
    content: Content,
}

enum Content {
    Row(Vec<Rc<Node>>),
    Col(Vec<Rc<Node>>),
    Text(SharedString),
    /// One of the host's lists, of which only the rows in view are drawn:
    /// `item` is called for each to say how it looks.
    Rows {
        source: SharedString,
        item: Value,
    },
}

/// What a click on a node does.
#[derive(Clone)]
pub enum Click {
    /// Run the command of this name.
    Command(SharedString),
    /// Call this function of the layout's.
    Function(Value),
}

/// The styles a layout can set, and how each is written. Anything left
/// out, or given as nil, stays as it would be.
#[derive(Default)]
struct Style {
    width: Option<Size>,
    height: Option<Size>,
    /// Take the room that is left over.
    grow: bool,
    /// `false` keeps the box at its size when room runs out.
    shrink: Option<bool>,
    padding_x: Option<f32>,
    padding_y: Option<f32>,
    padding_left: Option<f32>,
    padding_right: Option<f32>,
    /// Space between children.
    gap: Option<f32>,
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
    bold: bool,
    nowrap: bool,
}

const STYLES: &str = "width height grow shrink padding-x padding-y padding-left padding-right \
     gap align justify bg color border-top border-right border-bottom border-left border-color \
     bold nowrap";

/// Pixels, or `'full` for all of the parent.
enum Size {
    Pixels(f32),
    Full,
}

#[derive(Clone, Copy)]
enum Align {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy)]
enum Justify {
    Start,
    Center,
    End,
    Between,
}

/// A color of the theme by name, or `"#rrggbb"`.
#[derive(Clone, Copy)]
enum Color {
    Theme(ThemeColor),
    Fixed(Hsla),
}

/// Looks a color up in whichever theme is in use.
type ThemeColor = fn(&Theme) -> Hsla;

/// The theme's colors a layout can name.
const THEME_COLORS: &[(&str, ThemeColor)] = &[
    ("accent", |theme| theme.accent),
    ("accent-foreground", |theme| theme.accent_foreground),
    ("background", |theme| theme.background),
    ("border", |theme| theme.border),
    ("danger", |theme| theme.danger),
    ("foreground", |theme| theme.foreground),
    ("info", |theme| theme.info),
    ("link", |theme| theme.link),
    ("list-active", |theme| theme.list_active),
    ("list-hover", |theme| theme.list_hover),
    ("muted", |theme| theme.muted),
    ("muted-foreground", |theme| theme.muted_foreground),
    ("popover", |theme| theme.popover),
    ("popover-foreground", |theme| theme.popover_foreground),
    ("primary", |theme| theme.primary),
    ("selection", |theme| theme.selection),
    ("sidebar", |theme| theme.sidebar),
    ("sidebar-border", |theme| theme.sidebar_border),
    ("sidebar-foreground", |theme| theme.sidebar_foreground),
    ("success", |theme| theme.success),
    ("warning", |theme| theme.warning),
];

// ---- from a program's values -------------------------------------------

fn wrong(key: &str, expected: &str, got: &Value) -> Error {
    Error::new(
        ErrorKind::Type,
        format!("{key}: expected {expected}, got {got}"),
    )
}

/// A name as a program writes one: `'name`, `:name` or `"name"`.
fn word(value: &Value) -> Option<String> {
    match value {
        Value::Symbol(name) | Value::Keyword(name) => Some(name.name().to_string()),
        Value::Str(name) => Some(name.to_string()),
        _ => None,
    }
}

fn number(key: &str, value: &Value) -> Result<f32, Error> {
    match value.as_float() {
        Ok(number) if number.is_finite() => Ok(number as f32),
        _ => Err(wrong(key, "a number", value)),
    }
}

fn size(key: &str, value: &Value) -> Result<Size, Error> {
    match word(value).as_deref() {
        Some("full") => Ok(Size::Full),
        Some(_) => Err(wrong(key, "a number or 'full", value)),
        None => number(key, value)
            .map(Size::Pixels)
            .map_err(|_| wrong(key, "a number or 'full", value)),
    }
}

fn color(key: &str, value: &Value) -> Result<Color, Error> {
    let Some(name) = word(value) else {
        return Err(wrong(key, "a color", value));
    };
    if name.starts_with('#') {
        return match Rgba::try_from(name.as_str()) {
            Ok(color) => Ok(Color::Fixed(color.into())),
            Err(_) => Err(wrong(key, "a color like \"#1e1e2e\"", value)),
        };
    }
    match THEME_COLORS.iter().find(|(known, _)| *known == name) {
        Some((_, get)) => Ok(Color::Theme(*get)),
        None => {
            let known: Vec<&str> = THEME_COLORS.iter().map(|(name, _)| *name).collect();
            Err(Error::new(
                ErrorKind::Raised,
                format!(
                    "{key}: {name} is not a color of the theme ({})",
                    known.join(" ")
                ),
            ))
        }
    }
}

/// One of `choices`, by name.
fn choice<T: Copy>(key: &str, value: &Value, choices: &[(&str, T)]) -> Result<T, Error> {
    let name = word(value);
    match choices
        .iter()
        .find(|(known, _)| Some(*known) == name.as_deref())
    {
        Some((_, choice)) => Ok(*choice),
        None => {
            let known: Vec<String> = choices.iter().map(|(name, _)| format!("'{name}")).collect();
            Err(wrong(key, &known.join(", "), value))
        }
    }
}

impl Style {
    /// Set the style `key` names. nil leaves it alone, so that a style can
    /// be the value of a `when`.
    fn set(&mut self, key: &str, value: &Value) -> Result<(), Error> {
        if value.is_nil() {
            return Ok(());
        }
        match key {
            "width" => self.width = Some(size(key, value)?),
            "height" => self.height = Some(size(key, value)?),
            "grow" => self.grow = value.is_truthy(),
            "shrink" => self.shrink = Some(value.is_truthy()),
            "padding-x" => self.padding_x = Some(number(key, value)?),
            "padding-y" => self.padding_y = Some(number(key, value)?),
            "padding-left" => self.padding_left = Some(number(key, value)?),
            "padding-right" => self.padding_right = Some(number(key, value)?),
            "gap" => self.gap = Some(number(key, value)?),
            "align" => {
                let choices = [
                    ("start", Align::Start),
                    ("center", Align::Center),
                    ("end", Align::End),
                ];
                self.align = Some(choice(key, value, &choices)?);
            }
            "justify" => {
                let choices = [
                    ("start", Justify::Start),
                    ("center", Justify::Center),
                    ("end", Justify::End),
                    ("between", Justify::Between),
                ];
                self.justify = Some(choice(key, value, &choices)?);
            }
            "bg" => self.bg = Some(color(key, value)?),
            "color" => self.color = Some(color(key, value)?),
            "border-top" => self.border_top = Some(number(key, value)?),
            "border-right" => self.border_right = Some(number(key, value)?),
            "border-bottom" => self.border_bottom = Some(number(key, value)?),
            "border-left" => self.border_left = Some(number(key, value)?),
            "border-color" => self.border_color = Some(color(key, value)?),
            "bold" => self.bold = value.is_truthy(),
            "nowrap" => self.nowrap = value.is_truthy(),
            _ => {
                return Err(Error::new(
                    ErrorKind::Raised,
                    format!("{key} is not a style ({STYLES} hover on-click)"),
                ));
            }
        }
        Ok(())
    }

    /// A style from a list that goes name, value, name, value.
    fn from_list(key: &str, list: &Value) -> Result<Style, Error> {
        let items = list
            .to_vec()
            .map_err(|_| wrong(key, "a list of styles", list))?;
        let mut style = Style::default();
        for pair in items.chunks(2) {
            match pair {
                [Value::Keyword(name), value] => style.set(&name.name(), value)?,
                _ => return Err(wrong(key, "a list like (list :bg 'muted)", list)),
            }
        }
        Ok(style)
    }
}

impl Node {
    fn new(content: Content) -> Node {
        Node {
            style: Style::default(),
            hover: None,
            on_click: None,
            content,
        }
    }

    /// Take the `:name value` pairs `args` starts with for this node's
    /// styles, and give back what follows them.
    fn take_styles<'a>(&mut self, mut args: &'a [Value]) -> Result<&'a [Value], Error> {
        while let [Value::Keyword(key), rest @ ..] = args {
            let key = key.name();
            let [value, rest @ ..] = rest else {
                return Err(Error::new(
                    ErrorKind::Raised,
                    format!(":{key} is not followed by a value"),
                ));
            };
            match (&*key, value) {
                (_, Value::Nil) => {}
                ("hover", list) => self.hover = Some(Style::from_list("hover", list)?),
                ("on-click", Value::Str(command)) => {
                    self.on_click = Some(Click::Command(SharedString::from(command.to_string())));
                }
                ("on-click", function @ (Value::Fn(_) | Value::Native(_))) => {
                    self.on_click = Some(Click::Function(function.clone()));
                }
                ("on-click", other) => {
                    return Err(wrong("on-click", "a command's name or a function", other));
                }
                (key, value) => self.style.set(key, value)?,
            }
            args = rest;
        }
        Ok(args)
    }
}

/// The node a program's value stands for.
fn node_of(value: &Value) -> Option<Rc<Node>> {
    match value {
        // A string where a node is expected is text.
        Value::Str(text) => Some(Rc::new(Node::new(Content::Text(SharedString::from(
            text.to_string(),
        ))))),
        other => other.as_foreign::<Node>(),
    }
}

/// The children of a row or column: nodes, strings, lists of those (as
/// `map` makes), and nil for nothing (as a `when` that does not hold).
fn children(args: &[Value], into: &mut Vec<Rc<Node>>) -> Result<(), Error> {
    for value in args {
        match value {
            Value::Nil => {}
            Value::Pair(_) => children(&value.to_vec()?, into)?,
            other => match node_of(other) {
                Some(node) => into.push(node),
                None => {
                    return Err(Error::new(
                        ErrorKind::Type,
                        format!("expected something to draw, got {other}"),
                    ));
                }
            },
        }
    }
    Ok(())
}

fn container(args: &[Value], content: fn(Vec<Rc<Node>>) -> Content) -> Result<Value, Error> {
    let mut node = Node::new(Content::Text(SharedString::default()));
    let rest = node.take_styles(args)?;
    let mut inside = Vec::new();
    children(rest, &mut inside)?;
    node.content = content(inside);
    Ok(Value::foreign(node))
}

/// The functions a layout is written with.
pub(crate) fn define_functions(interp: &mut Interp) {
    // (row styles... children...), (col styles... children...)
    let any = Arity::at_least(0);
    interp.define_native("row", any, |_, args| container(args, Content::Row));
    interp.define_native("col", any, |_, args| container(args, Content::Col));

    // (text value styles...): the value as `str` would show it.
    interp.define_native("text", Arity::at_least(1), |_, args| {
        let text = SharedString::from(args[0].display());
        let mut node = Node::new(Content::Text(text));
        match node.take_styles(&args[1..])? {
            [] => Ok(Value::foreign(node)),
            [extra, ..] => Err(Error::new(
                ErrorKind::Raised,
                format!("text: expected a style, got {extra}"),
            )),
        }
    });

    // (rows list-name styles... function): the function is called with
    // one row of the host's list and gives back how it looks. All rows
    // must come out the same height.
    interp.define_native("rows", Arity::at_least(2), |_, args| {
        let Some(source) = word(&args[0]) else {
            return Err(wrong("rows", "the name of a list", &args[0]));
        };
        let [middle @ .., item] = &args[1..] else {
            unreachable!("rows takes at least two arguments");
        };
        if !matches!(item, Value::Fn(_) | Value::Native(_)) {
            return Err(wrong("rows", "a function for a row, last", item));
        }
        let mut node = Node::new(Content::Rows {
            source: SharedString::from(source),
            item: item.clone(),
        });
        match node.take_styles(middle)? {
            [] => Ok(Value::foreign(node)),
            [extra, ..] => Err(Error::new(
                ErrorKind::Raised,
                format!("rows: expected a style, got {extra}"),
            )),
        }
    });

    // (set-layout panel function): from now on the panel is drawn by
    // calling the function with the panel's values.
    interp.define_native("set-layout", 2, |call, args| {
        let panel = word(&args[0]).unwrap_or_default();
        if !PANELS.contains(&panel.as_str()) {
            return Err(Error::new(
                ErrorKind::Raised,
                format!(
                    "{} is not a panel with a layout ({})",
                    args[0],
                    PANELS.join(" ")
                ),
            ));
        }
        if !matches!(args[1], Value::Fn(_) | Value::Native(_)) {
            return Err(wrong("set-layout", "a function", &args[1]));
        }
        let cx = script::app(call)?;
        let layouts = &mut cx.global_mut::<Layouts>().layouts;
        layouts.insert(panel, args[1].clone());
        cx.refresh_windows();
        Ok(Value::Nil)
    });
}

// ---- what a host tells a layout ----------------------------------------

/// Named values for a layout: what it gets with `(get panel :name)`.
#[derive(Default)]
pub struct Vars(Vec<(&'static str, Value)>);

impl Vars {
    pub fn set(mut self, name: &'static str, value: impl Into<Value>) -> Self {
        self.0.push((name, value.into()));
        self
    }

    /// The list a program reads them from: name, value, name, value.
    fn into_list(self) -> Value {
        let items = self
            .0
            .into_iter()
            .flat_map(|(name, value)| [Value::keyword(name), value]);
        Value::list(items)
    }
}

/// A view whose looks come from a layout.
pub trait Host: 'static + Sized {
    /// The values of the panel as a whole.
    fn vars(&self, window: &Window, cx: &App) -> Vars;

    /// How many rows the list `source` has.
    fn row_count(&self, source: &str) -> usize;

    /// The values of one row.
    fn row_vars(&self, source: &str, index: usize, cx: &App) -> Vars;

    /// Where the list `source` keeps its scroll position.
    fn scroll_handle(&self, source: &str) -> Option<&UniformListScrollHandle>;

    /// A node was clicked. `row` is the list and row the node is in.
    fn click(
        &mut self,
        action: &Click,
        row: Option<(&str, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    );
}

// ---- drawing -----------------------------------------------------------

impl Style {
    fn apply<E: Styled>(&self, mut el: E, theme: &Theme) -> E {
        let color = |color: Color| match color {
            Color::Theme(get) => get(theme),
            Color::Fixed(color) => color,
        };
        match self.width {
            Some(Size::Pixels(width)) => el = el.w(px(width)),
            Some(Size::Full) => el = el.w_full(),
            None => {}
        }
        match self.height {
            Some(Size::Pixels(height)) => el = el.h(px(height)),
            Some(Size::Full) => el = el.h_full(),
            None => {}
        }
        if self.grow {
            el = el.flex_1().min_w_0().min_h_0();
        }
        if self.shrink == Some(false) {
            el = el.flex_shrink_0();
        }
        if let Some(padding) = self.padding_x {
            el = el.px(px(padding));
        }
        if let Some(padding) = self.padding_y {
            el = el.py(px(padding));
        }
        if let Some(padding) = self.padding_left {
            el = el.pl(px(padding));
        }
        if let Some(padding) = self.padding_right {
            el = el.pr(px(padding));
        }
        if let Some(gap) = self.gap {
            el = el.gap(px(gap));
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
        if let Some(bg) = self.bg {
            el = el.bg(color(bg));
        }
        if let Some(text) = self.color {
            el = el.text_color(color(text));
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
        if let Some(border) = self.border_color {
            el = el.border_color(color(border));
        }
        if self.bold {
            el = el.font_weight(FontWeight::SEMIBOLD);
        }
        if self.nowrap {
            el = el.whitespace_nowrap();
        }
        el
    }
}

/// Where in a panel a node is being built.
#[derive(Clone, Copy)]
struct Place<'a> {
    /// The list and row the node is in.
    row: Option<(&'a SharedString, usize)>,
    /// Tells the node from its siblings.
    key: usize,
    /// The node is a whole row of a list.
    is_row: bool,
}

/// Call a layout's function and take what it gives back for a node.
fn describe(function: &Value, vars: Vars, cx: &mut App) -> Result<Rc<Node>, Error> {
    let value = script::call_to_draw(function, &[vars.into_list()], cx)?;
    node_of(&value).ok_or_else(|| {
        Error::new(
            ErrorKind::Type,
            format!("a layout gives back a row, col, text or rows, not {value}"),
        )
    })
}

/// What is drawn in place of a layout that failed: the reason.
fn problem(error: &Error, cx: &App) -> AnyElement {
    div()
        .p_2()
        .text_color(cx.theme().danger)
        .child(error.to_string())
        .into_any_element()
}

/// Draw the panel `panel` for `host`. Call it from the host's `render`.
pub fn render<H: Host>(panel: &str, host: &H, window: &Window, cx: &mut Context<H>) -> AnyElement {
    let layout = cx.global::<Layouts>().layouts.get(panel).cloned();
    let node = match layout {
        Some(function) => describe(&function, host.vars(window, cx), cx),
        None => Err(Error::new(
            ErrorKind::Raised,
            format!("the panel {panel} has no layout"),
        )),
    };
    let place = Place {
        row: None,
        key: 0,
        is_row: false,
    };
    match node {
        Ok(node) => build(&node, host, place, cx),
        Err(error) => problem(&error, cx),
    }
}

fn build<H: Host>(node: &Node, host: &H, place: Place, cx: &Context<H>) -> AnyElement {
    let theme = cx.theme();
    let mut el = match &node.content {
        Content::Rows { source, item } => return build_rows(node, source, item, host, cx),
        Content::Row(_) => div().flex(),
        Content::Col(_) => div().flex().flex_col(),
        Content::Text(text) => div().child(text.clone()),
    };
    el = node.style.apply(el, theme);
    if let Some(hover) = &node.hover {
        el = el.hover(|style| hover.apply(style, theme));
    }
    if let Content::Row(children) | Content::Col(children) = &node.content {
        el = el.children(children.iter().enumerate().map(|(key, child)| {
            let place = Place {
                row: place.row,
                key,
                is_row: false,
            };
            build(child, host, place, cx)
        }));
    }

    // Only a node that is clicked needs an id, and a row: without one, the
    // ids inside two rows would be the same.
    if node.on_click.is_none() && !place.is_row {
        return el.into_any_element();
    }
    let mut el = el.id(place.key);
    if let Some(action) = node.on_click.clone() {
        let row = place.row.map(|(source, index)| (source.clone(), index));
        el = el.on_click(cx.listener(move |host, _: &ClickEvent, window, cx| {
            let row = row
                .as_ref()
                .map(|(source, index)| (source.as_ref(), *index));
            host.click(&action, row, window, cx);
        }));
    }
    el.into_any_element()
}

fn build_rows<H: Host>(
    node: &Node,
    source: &SharedString,
    item: &Value,
    host: &H,
    cx: &Context<H>,
) -> AnyElement {
    let rows = cx.processor({
        let (source, item) = (source.clone(), item.clone());
        move |host: &mut H, range: Range<usize>, _, cx| {
            range
                .map(|index| {
                    let vars = host.row_vars(&source, index, cx);
                    let place = Place {
                        row: Some((&source, index)),
                        key: index,
                        is_row: true,
                    };
                    match describe(&item, vars, cx) {
                        Ok(node) => build(&node, host, place, cx),
                        Err(error) => problem(&error, cx),
                    }
                })
                .collect::<Vec<_>>()
        }
    });
    let mut list = uniform_list(source.clone(), host.row_count(source), rows);
    if let Some(handle) = host.scroll_handle(source) {
        list = list.track_scroll(handle);
    }
    node.style.apply(list, cx.theme()).into_any_element()
}

// ---- the layouts in use ------------------------------------------------

/// Every panel's layout, by name: the function that describes it.
#[derive(Default)]
pub struct Layouts {
    layouts: HashMap<String, Value>,
}

impl Global for Layouts {}

/// Start out with no layouts: the interpreter sets the built-in ones.
pub fn init(cx: &mut App) {
    cx.set_global(Layouts::default());
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in GPUI's `test` attribute.
    use super::{Click, Color, Content, Node, Size, define_functions};
    use med_lisp::Interp;
    use std::rc::Rc;

    fn lisp() -> Interp {
        let mut interp = Interp::new();
        define_functions(&mut interp);
        interp
    }

    fn node(source: &str) -> Rc<Node> {
        let value = lisp().eval_str(source, &mut ()).unwrap();
        value.as_foreign::<Node>().unwrap()
    }

    fn error(source: &str) -> String {
        match lisp().eval_str(source, &mut ()) {
            Ok(value) => panic!("{source}: expected an error, got {value}"),
            Err(error) => error.to_string(),
        }
    }

    fn inside(node: &Node) -> &[Rc<Node>] {
        match &node.content {
            Content::Row(children) | Content::Col(children) => children,
            _ => panic!("not a row or a column"),
        }
    }

    fn text(node: &Node) -> &str {
        match &node.content {
            Content::Text(text) => text,
            _ => panic!("not text"),
        }
    }

    #[test]
    fn the_built_in_layouts_are_programs() {
        assert!(med_lisp::read(super::BUILT_IN).is_ok());
    }

    #[test]
    fn functions_describe_a_tree() {
        let row = node(
            r##"(row :height 24 :width 'full :grow true :bg "#1e1e2e" :color 'muted
                     :hover (list :bg 'list-hover) :on-click "tree.open"
                  (text "a" :bold true)
                  "plain"
                  (text 12))"##,
        );
        assert!(matches!(row.content, Content::Row(_)));
        assert!(matches!(row.style.height, Some(Size::Pixels(24.))));
        assert!(matches!(row.style.width, Some(Size::Full)));
        assert!(row.style.grow);
        assert!(matches!(row.style.bg, Some(Color::Fixed(_))));
        assert!(matches!(row.style.color, Some(Color::Theme(_))));
        assert!(row.hover.as_ref().is_some_and(|hover| hover.bg.is_some()));
        assert!(matches!(&row.on_click, Some(Click::Command(name)) if name == "tree.open"));
        let children = inside(&row);
        let texts: Vec<&str> = children.iter().map(|child| text(child)).collect();
        assert_eq!(texts, ["a", "plain", "12"]);
        assert!(children[0].style.bold && !children[1].style.bold);

        let col = node("(col (rows 'entries :grow true (fn (entry) (text entry))))");
        let [list] = inside(&col) else {
            panic!("expected one child");
        };
        assert!(matches!(&list.content, Content::Rows { source, .. } if source == "entries"));
        assert!(list.style.grow);

        let click = node("(text \"x\" :on-click (fn () 1))");
        assert!(matches!(click.on_click, Some(Click::Function(_))));
    }

    #[test]
    fn what_depends_on_a_value_is_an_expression() {
        // nil is no style and no child, so `when` and `if` do for both.
        let row = node(
            "(def selected true) (def depth 3)
             (row :padding-left (+ 10 (* 14 depth))
                  :bg (when selected 'list-active)
                  :color (when (not selected) 'warning)
               (when selected (text \"on\"))
               (when (not selected) (text \"off\"))
               (map (fn (n) (text n)) '(1 2)))",
        );
        assert_eq!(row.style.padding_left, Some(52.));
        assert!(row.style.bg.is_some() && row.style.color.is_none());
        let texts: Vec<&str> = inside(&row).iter().map(|child| text(child)).collect();
        assert_eq!(texts, ["on", "1", "2"]);
    }

    #[test]
    fn says_what_is_wrong_with_a_layout() {
        assert!(error("(row :colour 'muted)").starts_with("1:1: colour is not a style (width "));
        assert!(
            error("(text \"x\"\n  :bg 'sidebr)")
                .starts_with("1:1: bg: sidebr is not a color of the theme (accent ")
        );
        assert_eq!(
            error("(row :bg \"#12\")"),
            "1:1: bg: expected a color like \"#1e1e2e\", got \"#12\""
        );
        assert_eq!(
            error("(row :height \"tall\")"),
            "1:1: height: expected a number or 'full, got \"tall\""
        );
        assert_eq!(
            error("(row :align 'middle)"),
            "1:1: align: expected 'start, 'center, 'end, got middle"
        );
        assert_eq!(
            error("(row :height)"),
            "1:1: :height is not followed by a value"
        );
        assert_eq!(error("(row 5)"), "1:1: expected something to draw, got 5");
        assert_eq!(
            error("(row :on-click 5)"),
            "1:1: on-click: expected a command's name or a function, got 5"
        );
        assert_eq!(
            error("(row :hover 'muted)"),
            "1:1: hover: expected a list of styles, got muted"
        );
        assert_eq!(
            error("(text \"x\" 5)"),
            "1:1: text: expected a style, got 5"
        );
        assert_eq!(
            error("(rows 'entries 5)"),
            "1:1: rows: expected a function for a row, last, got 5"
        );
    }
}
