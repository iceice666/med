//! Snippets, as language servers write completions that have places to
//! fill in: `add(${1:a}, ${2:b})$0`. Only the text is made of them, and
//! where the cursor goes: there is no going from one place to the next.
//!
//! A tabstop (`$1`, `${1}`) is nothing, a placeholder (`${1:a}`) its text,
//! a choice (`${1|a,b|}`) its first, and a variable (`$TM_FILENAME`,
//! `${TM_FILENAME:default}`) its default or nothing. `\` takes the `$`, `}`
//! or `\` after it as it is.

/// A snippet's text, and where in it the cursor goes, in chars: the first
/// place to fill in, else the end of the snippet (`$0`), else the end.
#[derive(Debug, PartialEq, Eq)]
pub struct Expanded {
    pub text: String,
    pub cursor: usize,
}

pub fn expand(snippet: &str) -> Expanded {
    let mut parser = Parser {
        chars: snippet.chars().collect(),
        at: 0,
        text: String::new(),
        len: 0,
        stops: Vec::new(),
    };
    parser.any(false);
    let first = parser
        .stops
        .iter()
        .filter(|(n, _)| *n > 0)
        .min_by_key(|(n, _)| *n);
    let last = parser.stops.iter().find(|(n, _)| *n == 0);
    let cursor = first.or(last).map_or(parser.len, |&(_, at)| at);
    Expanded {
        text: parser.text,
        cursor,
    }
}

/// A snippet as it goes in on a line indented by `indent`: each line
/// after the first indented as much, and each tab one `unit` of indent.
pub fn reindent(expanded: Expanded, indent: &str, unit: &str) -> Expanded {
    let mut text = String::with_capacity(expanded.text.len());
    let mut len = 0;
    let mut cursor = None;
    for (at, c) in expanded.text.chars().enumerate() {
        if at == expanded.cursor {
            cursor = Some(len);
        }
        match c {
            '\t' => {
                text.push_str(unit);
                len += unit.chars().count();
            }
            '\n' => {
                text.push('\n');
                text.push_str(indent);
                len += 1 + indent.chars().count();
            }
            _ => {
                text.push(c);
                len += 1;
            }
        }
    }
    Expanded {
        text,
        cursor: cursor.unwrap_or(len),
    }
}

struct Parser {
    chars: Vec<char>,
    at: usize,
    text: String,
    /// The length of `text`, in chars.
    len: usize,
    /// Each tabstop and placeholder: its number and where it starts, the
    /// first of a number first.
    stops: Vec<(u32, usize)>,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn push(&mut self, c: char) {
        self.text.push(c);
        self.len += 1;
    }

    /// Text up to the end, or `nested` up to the `}` that ends what it
    /// is in, which is left to be read.
    fn any(&mut self, nested: bool) {
        while let Some(c) = self.peek() {
            match c {
                '}' if nested => return,
                '\\' => {
                    match self.chars.get(self.at + 1) {
                        Some(&next @ ('$' | '}' | '\\')) => {
                            self.push(next);
                            self.at += 2;
                        }
                        _ => {
                            self.push('\\');
                            self.at += 1;
                        }
                    };
                }
                '$' => self.dollar(),
                _ => {
                    self.push(c);
                    self.at += 1;
                }
            }
        }
    }

    fn number(&mut self) -> u32 {
        let mut n: u32 = 0;
        while let Some(digit) = self.peek().and_then(|c| c.to_digit(10)) {
            n = n.saturating_mul(10).saturating_add(digit);
            self.at += 1;
        }
        n
    }

    fn skip_name(&mut self) {
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            self.at += 1;
        }
    }

    /// Read past the `}` that ends what is being read, if it is there.
    fn close(&mut self) {
        if self.peek() == Some('}') {
            self.at += 1;
        }
    }

    /// What a `$` starts, the `$` being at `self.at`.
    fn dollar(&mut self) {
        let start = (self.at, self.text.len(), self.len, self.stops.len());
        self.at += 1;
        match self.peek() {
            Some(c) if c.is_ascii_digit() => {
                let n = self.number();
                self.stops.push((n, self.len));
            }
            // A variable: nothing is known of any.
            Some(c) if c.is_ascii_alphabetic() || c == '_' => self.skip_name(),
            Some('{') => {
                self.at += 1;
                let ok = match self.peek() {
                    Some(c) if c.is_ascii_digit() => {
                        let n = self.number();
                        self.stops.push((n, self.len));
                        self.after_stop()
                    }
                    Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                        self.skip_name();
                        self.after_variable()
                    }
                    _ => false,
                };
                if !ok {
                    // Not a snippet's: the `$` is only a `$`.
                    let (at, bytes, len, stops) = start;
                    self.at = at + 1;
                    self.text.truncate(bytes);
                    self.len = len;
                    self.stops.truncate(stops);
                    self.push('$');
                }
            }
            _ => self.push('$'),
        }
    }

    /// What may follow `${1`: `}`, `:placeholder}` or `|choice,…|}`.
    fn after_stop(&mut self) -> bool {
        match self.peek() {
            Some('}') => {
                self.at += 1;
                true
            }
            Some(':') => {
                self.at += 1;
                self.any(true);
                self.close();
                true
            }
            Some('|') => {
                self.at += 1;
                self.choice();
                true
            }
            _ => false,
        }
    }

    /// What may follow `${NAME`: `}`, `:default}` or `/regex/format/}`.
    fn after_variable(&mut self) -> bool {
        match self.peek() {
            Some('}') => {
                self.at += 1;
                true
            }
            Some(':') => {
                self.at += 1;
                self.any(true);
                self.close();
                true
            }
            Some('/') => {
                // A transform of a variable nothing is known of: nothing.
                while let Some(c) = self.peek() {
                    self.at += 1;
                    match c {
                        '\\' => self.at += 1,
                        '}' => break,
                        _ => {}
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// The first of `a,b|}`, and the rest skipped.
    fn choice(&mut self) {
        let mut first = true;
        while let Some(c) = self.peek() {
            self.at += 1;
            match c {
                '\\' => {
                    if let Some(next) = self.peek() {
                        self.at += 1;
                        if first {
                            self.push(next);
                        }
                    }
                }
                ',' => first = false,
                '|' => {
                    self.close();
                    return;
                }
                _ if first => self.push(c),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expanded(snippet: &str) -> (String, usize) {
        let Expanded { text, cursor } = expand(snippet);
        (text, cursor)
    }

    #[test]
    fn plain_text_is_itself() {
        assert_eq!(expanded("println"), ("println".into(), 7));
        assert_eq!(expanded(""), ("".into(), 0));
    }

    /// The cursor goes to the first place to fill in, by its number, else
    /// to the end of the snippet.
    #[test]
    fn the_cursor_goes_to_the_first_place() {
        assert_eq!(expanded("foo($1)"), ("foo()".into(), 4));
        assert_eq!(expanded("foo(${1})$0"), ("foo()".into(), 4));
        assert_eq!(expanded("$2 x $1"), (" x ".into(), 3));
        assert_eq!(expanded("{\n\t$0\n}"), ("{\n\t\n}".into(), 3));
        assert_eq!(expanded("a$0b$1c"), ("abc".into(), 2));
    }

    #[test]
    fn placeholders_are_their_text() {
        assert_eq!(
            expanded("add(${1:int a}, ${2:int b})$0"),
            ("add(int a, int b)".into(), 4)
        );
        assert_eq!(expanded("${1:foo(${2:x})}"), ("foo(x)".into(), 0));
        assert_eq!(expanded("vec![${1}]"), ("vec![]".into(), 5));
        // Chars, not bytes.
        assert_eq!(expanded("é(${1:x})"), ("é(x)".into(), 2));
    }

    #[test]
    fn a_choice_is_its_first() {
        assert_eq!(expanded("${1|pub,pub(crate)|} fn"), ("pub fn".into(), 0));
        assert_eq!(expanded("${1|a\\,b,c|}"), ("a,b".into(), 0));
    }

    #[test]
    fn variables_are_their_default_or_nothing() {
        assert_eq!(expanded("$TM_SELECTED_TEXT!"), ("!".into(), 1));
        assert_eq!(expanded("${TM_FILENAME:main}.rs"), ("main.rs".into(), 7));
        assert_eq!(expanded("${TM_FILENAME/(.*)/$1/}x"), ("x".into(), 1));
    }

    /// The lines after the first are indented as the line it goes in on,
    /// and a tab is the file's unit of indent; the cursor keeps its place.
    #[test]
    fn a_snippet_takes_the_indent_of_its_line() {
        let indented = |snippet: &str| {
            let Expanded { text, cursor } = reindent(expand(snippet), "    ", "  ");
            (text, cursor)
        };
        assert_eq!(
            indented("if $1 {\n\t$0\n}"),
            ("if  {\n      \n    }".into(), 3)
        );
        assert_eq!(
            indented("match x {\n\t$0\n}"),
            ("match x {\n      \n    }".into(), 16)
        );
        assert_eq!(indented("foo($1)"), ("foo()".into(), 4));
        assert_eq!(indented("x\n"), ("x\n    ".into(), 6));
    }

    /// `\` takes what follows as it is; a `$` that starts nothing is a `$`.
    #[test]
    fn what_is_not_a_snippet_is_text() {
        assert_eq!(expanded("\\$1 \\} \\\\"), ("$1 } \\".into(), 6));
        assert_eq!(expanded("a\\b"), ("a\\b".into(), 3));
        assert_eq!(expanded("cost: $"), ("cost: $".into(), 7));
        assert_eq!(expanded("${ x"), ("${ x".into(), 4));
        assert_eq!(expanded("${1x}"), ("${1x}".into(), 5));
        assert_eq!(expanded("$$1"), ("$".into(), 1));
    }
}
