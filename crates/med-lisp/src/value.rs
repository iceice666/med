//! What a program computes with: values, symbols and errors.

use crate::eval::{Call, Lambda};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// A place in a source text, both counted from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The text is not a program.
    Syntax,
    /// The text ends inside a list or a string: more of it may follow.
    Incomplete,
    /// A name nothing is bound to.
    Unbound,
    /// A value of the wrong kind.
    Type,
    /// The wrong number of arguments.
    Arity,
    Arithmetic,
    /// The program ran too long or nested too deep.
    Limit,
    /// The program called `error`.
    Raised,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    /// The innermost form the error happened in, when that is known.
    pub span: Option<Span>,
    /// The forms around that one, innermost first, ending with the one at
    /// the top level. Only those still waiting for a value: a form that
    /// made a tail call is not among them.
    pub trace: Vec<Span>,
}

/// More forms than this around an error are not all kept.
const MAX_TRACE: usize = 16;

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            span: None,
            trace: Vec::new(),
        }
    }

    /// The error came up through the form at `span`: it happened there, if
    /// no form inside has claimed it, or else on that form's behalf.
    pub(crate) fn through(mut self, span: Option<Span>) -> Self {
        let Some(span) = span else {
            return self;
        };
        match self.span {
            None => self.span = Some(span),
            Some(inner) if inner == span || self.trace.last() == Some(&span) => {}
            // Past the limit the outermost form replaces the one before
            // it: where a program was started from is worth the most.
            Some(_) if self.trace.len() == MAX_TRACE => self.trace[MAX_TRACE - 1] = span,
            Some(_) => self.trace.push(span),
        }
        self
    }

    pub(crate) fn at(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub(crate) fn syntax(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Syntax, message)
    }

    pub(crate) fn wrong_type(expected: &str, got: &Value) -> Self {
        Self::new(ErrorKind::Type, format!("expected {expected}, got {got}"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(span) = self.span {
            write!(f, "{}:{}: ", span.line, span.column)?;
        }
        f.write_str(&self.message)?;
        // An error in a function says little without the line that called
        // it.
        match (self.span, self.trace.last()) {
            (Some(span), Some(outer)) if outer.line != span.line => {
                write!(f, " (from {}:{})", outer.line, outer.column)
            }
            _ => Ok(()),
        }
    }
}

impl std::error::Error for Error {}

// ---- symbols -----------------------------------------------------------

/// Declares the special forms: an enum of them, and their names in the
/// same order, which is the order the symbol table starts out in.
macro_rules! special_forms {
    ($($form:ident = $name:literal,)*) => {
        /// A form the evaluator handles itself, rather than by calling a
        /// function with evaluated arguments.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum Form {
            $($form,)*
        }

        const FORMS: &[(Form, &str)] = &[$((Form::$form, $name),)*];
    };
}

special_forms! {
    Quote = "quote",
    If = "if",
    Def = "def",
    Defn = "defn",
    Fn = "fn",
    Let = "let",
    Do = "do",
    Set = "set!",
    And = "and",
    Or = "or",
    Cond = "cond",
    When = "when",
    Unless = "unless",
    While = "while",
}

/// A name. Two symbols are the same exactly when their names are.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Symbol(u32);

struct Symbols {
    names: Vec<Rc<str>>,
    ids: HashMap<Rc<str>, u32>,
}

impl Symbols {
    fn intern(&mut self, name: &str) -> u32 {
        if let Some(id) = self.ids.get(name) {
            return *id;
        }
        let id = self.names.len() as u32;
        let name: Rc<str> = name.into();
        self.names.push(name.clone());
        self.ids.insert(name, id);
        id
    }
}

thread_local! {
    // Values are not `Send`, so neither do symbols need to be.
    static SYMBOLS: RefCell<Symbols> = {
        let mut symbols = Symbols { names: Vec::new(), ids: HashMap::new() };
        for (_, name) in FORMS {
            symbols.intern(name);
        }
        symbols.intern("&");
        RefCell::new(symbols)
    };
}

impl Symbol {
    /// `&`, which comes before the name the rest of the arguments go by.
    pub(crate) const REST: Symbol = Symbol(FORMS.len() as u32);

    pub fn new(name: &str) -> Self {
        Symbol(SYMBOLS.with_borrow_mut(|symbols| symbols.intern(name)))
    }

    pub fn name(self) -> Rc<str> {
        SYMBOLS.with_borrow(|symbols| symbols.names[self.0 as usize].clone())
    }

    /// The special form this names, if any.
    pub(crate) fn form(self) -> Option<Form> {
        FORMS.get(self.0 as usize).map(|(form, _)| *form)
    }
}

impl fmt::Debug for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

// ---- values ------------------------------------------------------------

#[derive(Clone)]
pub enum Value {
    /// Nothing, which is also the empty list.
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    Symbol(Symbol),
    /// `:name`, a symbol that stands for itself.
    Keyword(Symbol),
    /// A list is a pair of its first value and the rest of the list.
    Pair(Rc<Pair>),
    /// A function written in the language.
    Fn(Rc<Lambda>),
    /// A function of the host.
    Native(Rc<Native>),
}

pub struct Pair {
    pub car: Value,
    pub cdr: Value,
    /// Where the list starting here was read from.
    pub span: Option<Span>,
}

impl Drop for Pair {
    /// Lets go of the rest of the list in a loop: left to the compiler, a
    /// long list would be dropped one stack frame per element.
    fn drop(&mut self) {
        let mut rest = std::mem::replace(&mut self.cdr, Value::Nil);
        while let Value::Pair(pair) = rest {
            match Rc::try_unwrap(pair) {
                Ok(mut pair) => rest = std::mem::replace(&mut pair.cdr, Value::Nil),
                // Someone else still holds the rest.
                Err(_) => break,
            }
        }
    }
}

/// How many arguments a function takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arity {
    min: usize,
    max: Option<usize>,
}

impl Arity {
    pub const fn exactly(count: usize) -> Self {
        Self {
            min: count,
            max: Some(count),
        }
    }

    pub const fn at_least(min: usize) -> Self {
        Self { min, max: None }
    }

    pub const fn between(min: usize, max: usize) -> Self {
        Self {
            min,
            max: Some(max),
        }
    }

    pub(crate) fn check(self, name: &str, got: usize) -> Result<()> {
        if got >= self.min && self.max.is_none_or(|max| got <= max) {
            return Ok(());
        }
        let expected = match self.max {
            Some(max) if max == self.min => format!("{max}"),
            Some(max) => format!("{} to {max}", self.min),
            None => format!("at least {}", self.min),
        };
        // "1 argument", "0 to 2 arguments", "at least 1 argument".
        let plural = if self.max.unwrap_or(self.min) == 1 {
            ""
        } else {
            "s"
        };
        Err(Error::new(
            ErrorKind::Arity,
            format!("{name}: expected {expected} argument{plural}, got {got}"),
        ))
    }
}

impl From<usize> for Arity {
    fn from(count: usize) -> Self {
        Self::exactly(count)
    }
}

pub type NativeFn = dyn Fn(&mut Call<'_>, &[Value]) -> Result<Value>;

pub struct Native {
    pub(crate) name: Rc<str>,
    pub(crate) arity: Arity,
    pub(crate) run: Box<NativeFn>,
}

impl Value {
    pub fn symbol(name: &str) -> Self {
        Value::Symbol(Symbol::new(name))
    }

    pub fn keyword(name: &str) -> Self {
        Value::Keyword(Symbol::new(name))
    }

    pub fn cons(car: Value, cdr: Value) -> Self {
        Value::Pair(Rc::new(Pair {
            car,
            cdr,
            span: None,
        }))
    }

    pub fn list(items: impl IntoIterator<Item = Value>) -> Self {
        let items: Vec<Value> = items.into_iter().collect();
        items
            .into_iter()
            .rev()
            .fold(Value::Nil, |rest, item| Value::cons(item, rest))
    }

    /// Everything counts as true but `nil` and `false`.
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }

    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil)
    }

    /// The values of a list. Stops at the first thing that is not a pair,
    /// which for a list built by `list` or read from text is its end.
    pub fn iter(&self) -> ListIter<'_> {
        ListIter { rest: self }
    }

    /// The values of a list, refusing anything else.
    pub fn to_vec(&self) -> Result<Vec<Value>> {
        let mut items = self.iter();
        let values: Vec<Value> = items.by_ref().cloned().collect();
        match items.rest() {
            Value::Nil => Ok(values),
            _ => Err(Error::wrong_type("a list", self)),
        }
    }

    pub fn as_int(&self) -> Result<i64> {
        match self {
            Value::Int(value) => Ok(*value),
            other => Err(Error::wrong_type("an integer", other)),
        }
    }

    /// A number of either kind, as a float.
    pub fn as_float(&self) -> Result<f64> {
        match self {
            Value::Int(value) => Ok(*value as f64),
            Value::Float(value) => Ok(*value),
            other => Err(Error::wrong_type("a number", other)),
        }
    }

    pub fn as_str(&self) -> Result<&str> {
        match self {
            Value::Str(value) => Ok(value),
            other => Err(Error::wrong_type("a string", other)),
        }
    }

    pub fn as_symbol(&self) -> Result<Symbol> {
        match self {
            Value::Symbol(value) => Ok(*value),
            other => Err(Error::wrong_type("a symbol", other)),
        }
    }

    /// The value as `str` shows it: a string is its text, without quotes.
    pub fn display(&self) -> String {
        match self {
            Value::Str(text) => text.to_string(),
            other => other.to_string(),
        }
    }
}

pub struct ListIter<'a> {
    rest: &'a Value,
}

impl<'a> ListIter<'a> {
    /// What is left of the list.
    pub fn rest(&self) -> &'a Value {
        self.rest
    }
}

impl<'a> Iterator for ListIter<'a> {
    type Item = &'a Value;

    fn next(&mut self) -> Option<&'a Value> {
        match self.rest {
            Value::Pair(pair) => {
                self.rest = &pair.cdr;
                Some(&pair.car)
            }
            _ => None,
        }
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Value::Bool(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Value::Int(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Value::Float(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Value::Str(value.into())
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Value::Str(value.into())
    }
}

/// Lists are equal when their values are, numbers when they are the same
/// number (`1` and `1.0` are), functions only to themselves.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        let (mut a, mut b) = (self, other);
        loop {
            return match (a, b) {
                (Value::Nil, Value::Nil) => true,
                (Value::Bool(a), Value::Bool(b)) => a == b,
                (Value::Int(a), Value::Int(b)) => a == b,
                (Value::Float(a), Value::Float(b)) => a == b,
                (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => {
                    *a as f64 == *b
                }
                (Value::Str(a), Value::Str(b)) => a == b,
                (Value::Symbol(a), Value::Symbol(b)) => a == b,
                (Value::Keyword(a), Value::Keyword(b)) => a == b,
                (Value::Fn(a), Value::Fn(b)) => Rc::ptr_eq(a, b),
                (Value::Native(a), Value::Native(b)) => Rc::ptr_eq(a, b),
                (Value::Pair(x), Value::Pair(y)) => {
                    if Rc::ptr_eq(x, y) {
                        true
                    } else if x.car != y.car {
                        false
                    } else {
                        // The rest in a loop, so a long list is no deeper
                        // than a short one.
                        a = &x.cdr;
                        b = &y.cdr;
                        continue;
                    }
                }
                _ => false,
            };
        }
    }
}

/// A value as it would be written in a program.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => f.write_str("nil"),
            Value::Bool(value) => write!(f, "{value}"),
            Value::Int(value) => write!(f, "{value}"),
            // `{:?}` keeps the point of a whole number: 1.0, not 1.
            Value::Float(value) => write!(f, "{value:?}"),
            Value::Str(text) => {
                f.write_str("\"")?;
                for c in text.chars() {
                    match c {
                        '"' => f.write_str("\\\"")?,
                        '\\' => f.write_str("\\\\")?,
                        '\n' => f.write_str("\\n")?,
                        '\t' => f.write_str("\\t")?,
                        '\r' => f.write_str("\\r")?,
                        c => write!(f, "{c}")?,
                    }
                }
                f.write_str("\"")
            }
            Value::Symbol(symbol) => f.write_str(&symbol.name()),
            Value::Keyword(symbol) => write!(f, ":{}", symbol.name()),
            Value::Pair(_) => {
                f.write_str("(")?;
                let mut items = self.iter();
                for (index, item) in items.by_ref().enumerate() {
                    if index > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{item}")?;
                }
                if !items.rest().is_nil() {
                    write!(f, " . {}", items.rest())?;
                }
                f.write_str(")")
            }
            Value::Fn(lambda) => match lambda.name() {
                Some(name) => write!(f, "#<fn {}>", name.name()),
                None => f.write_str("#<fn>"),
            },
            Value::Native(native) => write!(f, "#<native {}>", native.name),
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
