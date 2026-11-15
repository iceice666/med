//! From text to values.
//!
//! ```text
//! ; a comment, to the end of the line
//! 42  -1.5  "text\n"  name  :keyword  nil  true  false
//! (a list)  'quoted
//! ```

use crate::value::{Error, ErrorKind, Pair, Result, Span, Symbol, Value};
use std::iter::Peekable;
use std::rc::Rc;
use std::str::Chars;

/// Lists inside lists further than this are refused rather than read with
/// one stack frame each.
const MAX_NESTING: usize = 500;

/// Every form in `source`.
pub fn read(source: &str) -> Result<Vec<Value>> {
    Ok(read_spanned(source)?
        .into_iter()
        .map(|(form, _)| form)
        .collect())
}

/// Every form in `source`, with where it starts.
pub(crate) fn read_spanned(source: &str) -> Result<Vec<(Value, Span)>> {
    let mut reader = Reader {
        chars: source.chars().peekable(),
        line: 1,
        column: 1,
        nesting: 0,
    };
    let mut forms = Vec::new();
    loop {
        reader.skip_blanks();
        let span = reader.span();
        match reader.chars.peek() {
            None => return Ok(forms),
            Some(_) => forms.push((reader.form()?, span)),
        }
    }
}

struct Reader<'a> {
    chars: Peekable<Chars<'a>>,
    line: u32,
    column: u32,
    nesting: usize,
}

/// A character that ends a name or a number.
fn is_delimiter(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | ')' | '"' | ';' | '\'')
}

impl Reader<'_> {
    fn span(&self) -> Span {
        Span {
            line: self.line,
            column: self.column,
        }
    }

    fn next(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        if c == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(c)
    }

    /// Whitespace and comments.
    fn skip_blanks(&mut self) {
        while let Some(c) = self.chars.peek().copied() {
            if c == ';' {
                while self.chars.peek().is_some_and(|c| *c != '\n') {
                    self.next();
                }
            } else if c.is_whitespace() {
                self.next();
            } else {
                break;
            }
        }
    }

    /// The form that starts here. There is one: blanks were skipped.
    fn form(&mut self) -> Result<Value> {
        let span = self.span();
        match self.chars.peek().copied() {
            Some('(') => {
                self.next();
                self.list(span)
            }
            Some(')') => Err(Error::syntax("unexpected )").at(span)),
            Some('\'') => {
                self.next();
                self.skip_blanks();
                if self.chars.peek().is_none() {
                    return Err(Error::new(ErrorKind::Incomplete, "nothing after '").at(span));
                }
                let quoted = self.form()?;
                let rest = Value::cons(quoted, Value::Nil);
                Ok(Value::Pair(Rc::new(Pair {
                    car: Value::symbol("quote"),
                    cdr: rest,
                    span: Some(span),
                })))
            }
            Some('"') => {
                self.next();
                self.string(span)
            }
            _ => self.atom(span),
        }
    }

    /// The rest of a list whose `(` at `open` was just read.
    fn list(&mut self, open: Span) -> Result<Value> {
        self.nesting += 1;
        if self.nesting > MAX_NESTING {
            return Err(Error::syntax("lists nested too deeply").at(open));
        }
        let mut items = Vec::new();
        loop {
            self.skip_blanks();
            match self.chars.peek() {
                None => {
                    return Err(
                        Error::new(ErrorKind::Incomplete, "this ( is never closed").at(open)
                    );
                }
                Some(')') => {
                    self.next();
                    break;
                }
                Some(_) => items.push(self.form()?),
            }
        }
        self.nesting -= 1;
        // Only the pair the list starts at knows where it was written.
        let mut list = Value::Nil;
        let last = items.len().saturating_sub(1);
        for (index, item) in items.into_iter().rev().enumerate() {
            list = Value::Pair(Rc::new(Pair {
                car: item,
                cdr: list,
                span: (index == last).then_some(open),
            }));
        }
        Ok(list)
    }

    /// The rest of a string whose `"` at `open` was just read.
    fn string(&mut self, open: Span) -> Result<Value> {
        let mut text = String::new();
        loop {
            let at = self.span();
            match self.next() {
                None => {
                    return Err(
                        Error::new(ErrorKind::Incomplete, "this string is never closed").at(open),
                    );
                }
                Some('"') => return Ok(Value::Str(text.into())),
                Some('\\') => text.push(match self.next() {
                    Some('n') => '\n',
                    Some('t') => '\t',
                    Some('r') => '\r',
                    Some('\\') => '\\',
                    Some('"') => '"',
                    Some(c) => {
                        return Err(
                            Error::syntax(format!("\\{c} means nothing in a string")).at(at)
                        );
                    }
                    None => {
                        return Err(Error::new(
                            ErrorKind::Incomplete,
                            "this string is never closed",
                        )
                        .at(open));
                    }
                }),
                Some(c) => text.push(c),
            }
        }
    }

    /// A number, a name, a keyword, or one of `nil`, `true`, `false`.
    fn atom(&mut self, span: Span) -> Result<Value> {
        let mut text = String::new();
        while let Some(c) = self.chars.peek().copied() {
            if is_delimiter(c) {
                break;
            }
            text.push(c);
            self.next();
        }

        // What starts like a number has to be one: `1x` is not a name.
        let mut chars = text.chars();
        let first = chars.next().unwrap_or(' ');
        let second = chars.next();
        let numeric = first.is_ascii_digit()
            || (matches!(first, '+' | '-')
                && second.is_some_and(|c| c.is_ascii_digit() || c == '.'))
            || (first == '.' && second.is_some_and(|c| c.is_ascii_digit()));
        if numeric {
            if let Ok(value) = text.parse::<i64>() {
                return Ok(Value::Int(value));
            }
            return match text.parse::<f64>() {
                Ok(value) => Ok(Value::Float(value)),
                Err(_) => Err(Error::syntax(format!("{text} is not a number")).at(span)),
            };
        }

        Ok(match text.as_str() {
            "nil" => Value::Nil,
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => match text.strip_prefix(':') {
                Some(name) if !name.is_empty() => Value::Keyword(Symbol::new(name)),
                _ => Value::Symbol(Symbol::new(&text)),
            },
        })
    }
}
