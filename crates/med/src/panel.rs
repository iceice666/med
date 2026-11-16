//! Panels drawn from a layout written in Lisp.
//!
//! A layout is a function. Each time the panel is drawn it is called with
//! what the editor knows about the panel, and gives back a tree of rows,
//! columns, text, lists of rows and inputs, made with the functions
//! defined here:
//!
//! ```lisp
//! (defpanel 'notes :at 'right
//!   (fn (panel)
//!     (col :width 'full :height 'full :bg 'sidebar
//!       (text "Notes" :bold true)
//!       (input 'new :placeholder "Add a note…" :on-submit add-note)
//!       (rows notes :grow true :reveal selected
//!         (fn (note index)
//!           (row :height 24 :on-click (fn () (set! selected index))
//!                :bg (when (= index selected) 'list-active)
//!             (text note)))))))
//! ```
//!
//! What a panel shows is the program's own: values it keeps with `def` and
//! changes with `set!`. Because a layout is a program, what depends on
//! those values is an ordinary expression: there is no notation for
//! conditions or sizes beyond the language's own.
//!
//! The panels themselves, where they go and what keys they take, are in
//! `user_panel.rs`.

use crate::script;
use crate::user_panel::{PanelView, UserPanels};
use gpui::component::input::Input;
use gpui::component::text::TextView;
use gpui::component::{ActiveTheme as _, Theme};
use gpui::prelude::*;
use gpui::*;
use med_lisp::{Arity, Error, ErrorKind, Interp, Value};
use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

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
    /// A list of which only the rows in view are drawn: `item` is called
    /// for each value, and where in the list it is, to say how it looks.
    Rows {
        values: Rc<[Value]>,
        item: Value,
        /// The row to keep in view: scrolled to when it changes.
        reveal: Option<usize>,
        /// The row to have at the top: scrolled to when it changes.
        top: Option<usize>,
    },
    /// A line of text to type in.
    Input(InputSpec),
    /// Text read as Markdown: headings, lists, code, tables, links.
    /// With `scroll`, the node keeps its own height and scrolls what does
    /// not fit; without, it is as tall as what it shows.
    Markdown {
        text: SharedString,
        scroll: bool,
    },
    /// Where a bar draws the command line's cursor: nothing to see, but
    /// the input method puts its window there.
    Caret,
}

/// An input as a layout described it. The text in it is the panel's.
#[derive(Clone)]
pub(crate) struct InputSpec {
    pub name: SharedString,
    pub placeholder: SharedString,
    /// Called with the text whenever it changes.
    pub on_change: Option<Value>,
    /// Called with the text on enter.
    pub on_submit: Option<Value>,
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
    /// Take the room that is left over: a share of it, next to others.
    grow: Option<f32>,
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
    /// All four sides.
    border: Option<f32>,
    border_color: Option<Color>,
    rounded: Option<f32>,
    /// The size of text.
    size: Option<f32>,
    bold: bool,
    underline: bool,
    /// In the fixed-width font.
    mono: bool,
    nowrap: bool,
    /// Cut off what does not fit.
    clip: bool,
}

const STYLES: &str = "width height grow shrink padding-x padding-y padding-left padding-right \
     gap align justify bg color border border-top border-right border-bottom border-left \
     border-color rounded size bold underline mono nowrap clip";

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
pub(crate) enum Color {
    Theme(ThemeColor),
    Fixed(Hsla),
}

impl Color {
    pub(crate) fn of(self, theme: &Theme) -> Hsla {
        match self {
            Color::Theme(get) => get(theme),
            Color::Fixed(color) => color,
        }
    }
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
    ("status-bar", |theme| theme.status_bar),
    ("success", |theme| theme.success),
    ("tab-bar", |theme| theme.tab_bar),
    ("title-bar", |theme| theme.title_bar),
    ("title-bar-border", |theme| theme.title_bar_border),
    ("warning", |theme| theme.warning),
];

// ---- from a program's values -------------------------------------------

pub(crate) fn wrong(key: &str, expected: &str, got: &Value) -> Error {
    Error::new(
        ErrorKind::Type,
        format!("{key}: expected {expected}, got {got}"),
    )
}

/// A name as a program writes one: `'name`, `:name` or `"name"`.
pub(crate) fn word(value: &Value) -> Option<String> {
    match value {
        Value::Symbol(name) | Value::Keyword(name) => Some(name.name().to_string()),
        Value::Str(name) => Some(name.to_string()),
        _ => None,
    }
}

pub(crate) fn number(key: &str, value: &Value) -> Result<f32, Error> {
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

pub(crate) fn color(key: &str, value: &Value) -> Result<Color, Error> {
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
pub(crate) fn choice<T: Copy>(key: &str, value: &Value, choices: &[(&str, T)]) -> Result<T, Error> {
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
            "grow" => {
                self.grow = match value {
                    Value::Bool(grow) => grow.then_some(1.),
                    share => match number(key, share)? {
                        share if share > 0. => Some(share),
                        _ => return Err(wrong(key, "true or a share", value)),
                    },
                }
            }
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
            "border" => self.border = Some(number(key, value)?),
            "border-top" => self.border_top = Some(number(key, value)?),
            "border-right" => self.border_right = Some(number(key, value)?),
            "border-bottom" => self.border_bottom = Some(number(key, value)?),
            "border-left" => self.border_left = Some(number(key, value)?),
            "border-color" => self.border_color = Some(color(key, value)?),
            "rounded" => self.rounded = Some(number(key, value)?),
            "size" => match number(key, value)? {
                size if size > 0. => self.size = Some(size),
                _ => return Err(wrong(key, "a size", value)),
            },
            "bold" => self.bold = value.is_truthy(),
            "underline" => self.underline = value.is_truthy(),
            "mono" => self.mono = value.is_truthy(),
            "nowrap" => self.nowrap = value.is_truthy(),
            "clip" => self.clip = value.is_truthy(),
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
    /// styles, and give back what follows them. `option` is asked first
    /// about each: it says whether the name was one of the node's own.
    fn take_styles<'a>(
        &mut self,
        mut args: &'a [Value],
        mut option: impl FnMut(&str, &Value) -> Result<bool, Error>,
    ) -> Result<&'a [Value], Error> {
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
                ("on-click", function) if is_function(function) => {
                    self.on_click = Some(Click::Function(function.clone()));
                }
                ("on-click", other) => {
                    return Err(wrong("on-click", "a command's name or a function", other));
                }
                (key, value) => {
                    if !option(key, value)? {
                        self.style.set(key, value)?;
                    }
                }
            }
            args = rest;
        }
        Ok(args)
    }

    /// The inputs in this node and below, but not those in the rows of a
    /// list: a row comes and goes as the list is scrolled.
    pub(crate) fn inputs<'a>(&'a self, into: &mut Vec<&'a InputSpec>) {
        match &self.content {
            Content::Row(children) | Content::Col(children) => {
                children.iter().for_each(|child| child.inputs(into));
            }
            Content::Input(spec) => into.push(spec),
            Content::Text(_) | Content::Rows { .. } | Content::Markdown { .. } | Content::Caret => {
            }
        }
    }
}

pub(crate) fn is_function(value: &Value) -> bool {
    matches!(value, Value::Fn(_) | Value::Native(_))
}

/// No name is the node's own: all are styles.
fn no_options(_: &str, _: &Value) -> Result<bool, Error> {
    Ok(false)
}

/// Which row a `:reveal` or `:top` asks for.
fn row_index(key: &str, value: &Value) -> Result<usize, Error> {
    match value {
        Value::Int(index) if *index >= 0 => Ok(*index as usize),
        other => Err(wrong(key, "which row, from 0", other)),
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
    let rest = node.take_styles(args, no_options)?;
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
        match node.take_styles(&args[1..], no_options)? {
            [] => Ok(Value::foreign(node)),
            [extra, ..] => Err(Error::new(
                ErrorKind::Raised,
                format!("text: expected a style, got {extra}"),
            )),
        }
    });

    // (markdown value styles...): the value, as `str` would show it, read
    // as Markdown. Besides styles it takes `:scroll`: true keeps the node
    // the height its styles give it and scrolls the rest.
    interp.define_native("markdown", Arity::at_least(1), |_, args| {
        let text = SharedString::from(args[0].display());
        let mut scroll = false;
        let mut node = Node::new(Content::Text(SharedString::default()));
        let rest = node.take_styles(&args[1..], |key, value| {
            match key {
                "scroll" => scroll = value.is_truthy(),
                _ => return Ok(false),
            }
            Ok(true)
        })?;
        if let [extra, ..] = rest {
            return Err(Error::new(
                ErrorKind::Raised,
                format!("markdown: expected a style, got {extra}"),
            ));
        }
        node.content = Content::Markdown { text, scroll };
        Ok(Value::foreign(node))
    });

    // (caret styles...): where a bar has drawn the command line's cursor,
    // for the input method to put its window by. It has no width, and is
    // as tall as what it is in unless its styles say otherwise.
    interp.define_native("caret", Arity::at_least(0), |_, args| {
        let mut node = Node::new(Content::Caret);
        match node.take_styles(args, no_options)? {
            [] => Ok(Value::foreign(node)),
            [extra, ..] => Err(Error::new(
                ErrorKind::Raised,
                format!("caret: expected a style, got {extra}"),
            )),
        }
    });

    // (input name styles...): a line to type in, named so that the panel
    // can tell its inputs apart. Besides styles it takes `:placeholder`,
    // and `:on-change` and `:on-submit`, functions of the text.
    interp.define_native("input", Arity::at_least(1), |_, args| {
        let Some(name) = word(&args[0]).filter(|name| !name.is_empty()) else {
            return Err(wrong("input", "a name", &args[0]));
        };
        let mut spec = InputSpec {
            name: name.into(),
            placeholder: SharedString::default(),
            on_change: None,
            on_submit: None,
        };
        let mut node = Node::new(Content::Text(SharedString::default()));
        let rest = node.take_styles(&args[1..], |key, value| {
            match key {
                "placeholder" => spec.placeholder = value.display().into(),
                "on-change" | "on-submit" if !is_function(value) => {
                    return Err(wrong(key, "a function of the text", value));
                }
                "on-change" => spec.on_change = Some(value.clone()),
                "on-submit" => spec.on_submit = Some(value.clone()),
                _ => return Ok(false),
            }
            Ok(true)
        })?;
        if let [extra, ..] = rest {
            return Err(Error::new(
                ErrorKind::Raised,
                format!("input: expected a style, got {extra}"),
            ));
        }
        node.content = Content::Input(spec);
        Ok(Value::foreign(node))
    });

    // (rows list styles... function): the function is called with one
    // of the list's values, and where it is in the list if it takes two
    // arguments, and gives back how the row looks. All rows must come out
    // the same height, and the list needs a height of its own or `:grow`.
    // `:reveal` is the row to keep in view and `:top` the row to have at
    // the top: the list scrolls there when either changes.
    interp.define_native("rows", Arity::at_least(2), |_, args| {
        let values: Rc<[Value]> = match &args[0] {
            list @ (Value::Nil | Value::Pair(_)) => list.to_vec()?.into(),
            other => return Err(wrong("rows", "a list", other)),
        };
        let [middle @ .., item] = &args[1..] else {
            unreachable!("rows takes at least two arguments");
        };
        if !is_function(item) {
            return Err(wrong("rows", "a function for a row, last", item));
        }
        let (mut reveal, mut top) = (None, None);
        let mut node = Node::new(Content::Text(SharedString::default()));
        let rest = node.take_styles(middle, |key, value| {
            match key {
                "reveal" => reveal = Some(row_index(key, value)?),
                "top" => top = Some(row_index(key, value)?),
                _ => return Ok(false),
            }
            Ok(true)
        })?;
        if let [extra, ..] = rest {
            return Err(Error::new(
                ErrorKind::Raised,
                format!("rows: expected a style, got {extra}"),
            ));
        }
        node.content = Content::Rows {
            values,
            item: item.clone(),
            reveal,
            top,
        };
        Ok(Value::foreign(node))
    });

    // (set-layout panel function): from now on the panel is drawn by
    // calling the function with the panel's values.
    interp.define_native("set-layout", 2, |call, args| {
        let panel = word(&args[0]).unwrap_or_default();
        if !is_function(&args[1]) {
            return Err(wrong("set-layout", "a function", &args[1]));
        }
        let cx = script::app(call)?;
        let panels: Vec<&str> = cx.global::<UserPanels>().names().collect();
        if !panels.contains(&panel.as_str()) {
            return Err(Error::new(
                ErrorKind::Raised,
                format!("{} is not a panel ({})", args[0], panels.join(" ")),
            ));
        }
        cx.global_mut::<Layouts>().set(&panel, args[1].clone());
        cx.global_mut::<UserPanels>().layout_changed(&panel);
        cx.refresh_windows();
        Ok(Value::Nil)
    });
}

// ---- what a layout is told -------------------------------------------

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

// ---- drawing -----------------------------------------------------------

impl Style {
    fn apply<E: Styled>(&self, mut el: E, cx: &App) -> E {
        let theme = cx.theme();
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
        if let Some(share) = self.grow {
            el = el.flex_1().flex_grow(share).min_w_0().min_h_0();
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
        if let Some(width) = self.border {
            el = el.border(px(width));
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
        if let Some(radius) = self.rounded {
            el = el.rounded(px(radius));
        }
        // Before the weight: the font has one of its own.
        if self.mono {
            el = el.font(crate::mono_font(cx));
        }
        if let Some(size) = self.size {
            el = el.text_size(px(size));
        }
        if self.bold {
            el = el.font_weight(FontWeight::SEMIBOLD);
        }
        if self.underline {
            el = el.underline();
        }
        if self.nowrap {
            el = el.whitespace_nowrap();
        }
        if self.clip {
            el = el.overflow_hidden();
        }
        el
    }
}

/// Numbers that tell the nodes of one panel, or of one row, apart.
#[derive(Default)]
struct Draw {
    /// The next id for a node that is clicked.
    ids: Cell<usize>,
    /// The next list of the panel: which scroll position is its.
    lists: Cell<usize>,
    /// The nodes are in a row of a list.
    in_row: bool,
}

fn next(counter: &Cell<usize>) -> usize {
    counter.replace(counter.get() + 1)
}

/// Call a layout's function and take what it gives back for a node.
fn describe(function: &Value, args: &[Value], cx: &mut App) -> Result<Rc<Node>, Error> {
    let value = script::call_to_draw(function, args, cx)?;
    node_of(&value).ok_or_else(|| {
        Error::new(
            ErrorKind::Type,
            format!("a layout gives back a row, col, text, rows, input or caret, not {value}"),
        )
    })
}

/// What is drawn in place of a layout that failed: the reason.
pub(crate) fn problem(error: &Error, cx: &App) -> AnyElement {
    div()
        .p_2()
        .text_color(cx.theme().danger)
        .child(error.to_string())
        .into_any_element()
}

/// What the layout of `panel` makes of `vars`.
pub(crate) fn layout(panel: &str, vars: Vars, cx: &mut App) -> Result<Rc<Node>, Error> {
    match cx.global::<Layouts>().layouts.get(panel).cloned() {
        Some(function) => describe(&function, &[vars.into_list()], cx),
        None => Err(Error::new(
            ErrorKind::Raised,
            format!("the panel {panel} has no layout"),
        )),
    }
}

/// The elements of a panel, from what its layout gave back.
pub(crate) fn draw(node: &Node, view: &PanelView, cx: &Context<PanelView>) -> AnyElement {
    build(node, view, &Draw::default(), None, cx)
}

/// `row` is which row of a list the node is the whole of.
fn build(
    node: &Node,
    view: &PanelView,
    draw: &Draw,
    row: Option<usize>,
    cx: &Context<PanelView>,
) -> AnyElement {
    let mut el = match &node.content {
        Content::Rows { .. } => return build_rows(node, view, draw, cx),
        Content::Input(spec) => {
            let el = node.style.apply(div(), cx);
            return match view.input(&spec.name) {
                Some(state) => el.child(Input::new(state).appearance(false)),
                None => el.child("an input cannot be in a row of a list"),
            }
            .into_any_element();
        }
        Content::Row(_) => div().flex(),
        Content::Col(_) => div().flex().flex_col(),
        Content::Text(text) => div().child(text.clone()),
        Content::Caret => {
            let workspace = view.workspace();
            let caret = canvas(
                move |bounds, window, cx| {
                    let editor = workspace
                        .upgrade()
                        .and_then(|workspace| workspace.read(cx).active_editor().cloned());
                    if let Some(editor) = editor {
                        editor.update(cx, |editor, _| editor.place_cmdline_caret(bounds, window));
                    }
                },
                |_, _, _, _| {},
            )
            .size_full();
            div().flex_shrink_0().w(px(0.)).h_full().child(caret)
        }
        Content::Markdown { text, scroll } => {
            // Its own id, so that two in a panel do not share what they
            // have parsed or how far they are scrolled.
            let id = ElementId::NamedInteger("markdown".into(), next(&draw.ids) as u64);
            let view = TextView::markdown(id, text.clone()).scrollable(*scroll);
            match scroll {
                // The list it scrolls needs a height to fill.
                true => div().flex().flex_col().min_h_0().child(view.size_full()),
                false => div().child(view),
            }
        }
    };
    el = node.style.apply(el, cx);
    if let Some(hover) = &node.hover {
        el = el.hover(|style| hover.apply(style, cx));
    }
    if let Content::Row(children) | Content::Col(children) = &node.content {
        el = el.children(
            children
                .iter()
                .map(|child| build(child, view, draw, None, cx)),
        );
    }

    // Only a node that is clicked needs an id, and a row: without one, the
    // ids inside two rows would be the same.
    let id = match (row, &node.on_click) {
        (Some(index), _) => index,
        (None, Some(_)) => next(&draw.ids),
        (None, None) => return el.into_any_element(),
    };
    let mut el = el.id(id);
    if let Some(action) = node.on_click.clone() {
        el = el.on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
            view.click(&action, window, cx);
        }));
    }
    el.into_any_element()
}

fn build_rows(node: &Node, view: &PanelView, draw: &Draw, cx: &Context<PanelView>) -> AnyElement {
    let Content::Rows {
        values,
        item,
        reveal,
        top,
    } = &node.content
    else {
        unreachable!("build_rows is for lists");
    };
    // With the place in the list, if the function has a name for it.
    let with_index = item.arity().is_some_and(|arity| !arity.allows(1));
    let rows = cx.processor({
        let (values, item) = (values.clone(), item.clone());
        move |view: &mut PanelView, range: Range<usize>, _, cx| {
            range
                .map(|index| {
                    let value = values.get(index).cloned().unwrap_or(Value::Nil);
                    let args = [value, Value::Int(index as i64)];
                    let args = if with_index { &args[..] } else { &args[..1] };
                    let draw = Draw {
                        in_row: true,
                        ..Draw::default()
                    };
                    match describe(&item, args, cx) {
                        Ok(node) => build(&node, view, &draw, Some(index), cx),
                        Err(error) => problem(&error, cx),
                    }
                })
                .collect::<Vec<_>>()
        }
    });
    // A list in a row of another is told apart by the row's own numbers,
    // and keeps no scroll position: its row may be gone the next time.
    let list = if draw.in_row {
        uniform_list(("rows-in-row", next(&draw.ids)), values.len(), rows)
    } else {
        let index = next(&draw.lists);
        let handle = view.list_scroll(index, *reveal, *top);
        uniform_list(("rows", index), values.len(), rows).track_scroll(&handle)
    };
    node.style.apply(list, cx).into_any_element()
}

// ---- the layouts in use ------------------------------------------------

/// Every panel's layout, by name: the function that describes it.
#[derive(Default)]
pub struct Layouts {
    layouts: HashMap<String, Value>,
}

impl Global for Layouts {}

impl Layouts {
    /// Draw `panel` with `function` from now on.
    pub(crate) fn set(&mut self, panel: &str, function: Value) {
        self.layouts.insert(panel.to_string(), function);
    }

    /// Forget every layout, as before the init file is run again.
    pub(crate) fn clear(&mut self) {
        self.layouts.clear();
    }
}

/// Start out with no layouts: programs define the panels.
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
        assert_eq!(row.style.grow, Some(1.));
        assert!(matches!(row.style.bg, Some(Color::Fixed(_))));
        assert!(matches!(row.style.color, Some(Color::Theme(_))));
        assert!(row.hover.as_ref().is_some_and(|hover| hover.bg.is_some()));
        assert!(matches!(&row.on_click, Some(Click::Command(name)) if name == "tree.open"));
        let children = inside(&row);
        let texts: Vec<&str> = children.iter().map(|child| text(child)).collect();
        assert_eq!(texts, ["a", "plain", "12"]);
        assert!(children[0].style.bold && !children[1].style.bold);

        let col = node("(col (rows '(1 2 3) :height 90 :reveal 2 :top 0 (fn (n index) (text n))))");
        let [list] = inside(&col) else {
            panic!("expected one child");
        };
        assert!(matches!(
            &list.content,
            Content::Rows { values, reveal: Some(2), top: Some(0), .. } if values.len() == 3
        ));
        assert!(matches!(list.style.height, Some(Size::Pixels(90.))));
        let list = node("(rows nil :grow 3 (fn (n) (text n)))");
        assert_eq!(list.style.grow, Some(3.));

        let input = node(
            "(input 'query :grow true :placeholder \"Find…\"
                    :on-change (fn (text) text) :on-submit (fn (text) text))",
        );
        let Content::Input(spec) = &input.content else {
            panic!("expected an input");
        };
        assert_eq!(spec.name.as_ref(), "query");
        assert_eq!(spec.placeholder.as_ref(), "Find…");
        assert!(spec.on_change.is_some() && spec.on_submit.is_some());
        assert_eq!(input.style.grow, Some(1.));
        // The inputs of a panel are found wherever they are in it.
        let col = node("(col (row (input 'a)) (input 'b) (text \"c\"))");
        let mut inputs = Vec::new();
        col.inputs(&mut inputs);
        let names: Vec<&str> = inputs.iter().map(|spec| spec.name.as_ref()).collect();
        assert_eq!(names, ["a", "b"]);

        let doc = node("(markdown \"# Title\" :grow true :scroll true :padding-x 8)");
        assert!(matches!(
            &doc.content,
            Content::Markdown { text, scroll: true } if text.as_ref() == "# Title"
        ));
        assert_eq!(doc.style.grow, Some(1.));
        let doc = node("(markdown 12)");
        assert!(matches!(
            &doc.content,
            Content::Markdown { text, scroll: false } if text.as_ref() == "12"
        ));

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
    fn a_caret_is_a_node_with_styles() {
        let caret = node("(caret :height 14)");
        assert!(matches!(caret.content, Content::Caret));
        assert!(matches!(caret.style.height, Some(Size::Pixels(h)) if h == 14.));
        assert!(node("(text \"x\" :underline true)").style.underline);
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
        assert_eq!(error("(caret 5)"), "1:1: caret: expected a style, got 5");
        assert_eq!(
            error("(rows nil 5)"),
            "1:1: rows: expected a function for a row, last, got 5"
        );
        assert_eq!(
            error("(rows 5 (fn (row) (text row)))"),
            "1:1: rows: expected a list, got 5"
        );
        assert_eq!(
            error("(rows nil :reveal -1 (fn (row) (text row)))"),
            "1:1: reveal: expected which row, from 0, got -1"
        );
        assert_eq!(
            error("(input 'query :on-change 5)"),
            "1:1: on-change: expected a function of the text, got 5"
        );
        assert_eq!(error("(input 5)"), "1:1: input: expected a name, got 5");
        assert_eq!(
            error("(row :grow 0)"),
            "1:1: grow: expected true or a share, got 0"
        );
    }
}
