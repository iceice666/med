//! Snippets, as language servers write completions that have places to
//! fill in: `add(${1:a}, ${2:b})$0`. What is made of one is its text and
//! the places in it, in the order they are to be gone through, each with
//! every range that is to have the same text.
//!
//! A tabstop (`$1`, `${1}`) is nothing, a placeholder (`${1:a}`) its text,
//! a choice (`${1|a,b|}`) its first, and a variable (`$TM_FILENAME`,
//! `${TM_FILENAME:default}`) its default or nothing. `\` takes the `$`, `}`
//! or `\` after it as it is.

use std::ops::Range;

/// A snippet's text, and its places, in chars: the ones to fill in, in
/// order, then the one the cursor ends at (`$0`, else the end). The first
/// range of a place is the one typed in; the rest follow it.
#[derive(Debug, PartialEq, Eq)]
pub struct Expanded {
    pub text: String,
    pub stops: Vec<Vec<Range<usize>>>,
}

impl Expanded {
    /// Where the cursor goes first: the first place to fill in, else the
    /// end.
    pub fn cursor(&self) -> usize {
        self.stops
            .first()
            .and_then(|stop| stop.first())
            .map_or(0, |range| range.start)
    }

    /// Whether there is anywhere to go from where the cursor goes first.
    pub fn has_places(&self) -> bool {
        self.stops.len() > 1
    }
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
    let mut numbers: Vec<u32> = parser
        .stops
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| *n > 0)
        .collect();
    numbers.sort_unstable();
    numbers.dedup();
    let group = |number: u32| -> Vec<Range<usize>> {
        let mut ranges: Vec<Range<usize>> = parser
            .stops
            .iter()
            .filter(|(n, _)| *n == number)
            .map(|(_, range)| range.clone())
            .collect();
        ranges.sort_by_key(|range| range.start);
        // The one typed in: the first with a placeholder, else the first.
        if let Some(at) = ranges.iter().position(|range| !range.is_empty()) {
            let typed = ranges.remove(at);
            ranges.insert(0, typed);
        }
        ranges
    };
    let mut stops: Vec<Vec<Range<usize>>> = numbers.into_iter().map(group).collect();
    let last = group(0);
    stops.push(match last.is_empty() {
        true => std::iter::once(parser.len..parser.len).collect(),
        false => last,
    });
    Expanded {
        text: parser.text,
        stops,
    }
}

/// A snippet as it goes in on a line indented by `indent`: each line
/// after the first indented as much, and each tab one `unit` of indent.
pub fn reindent(expanded: Expanded, indent: &str, unit: &str) -> Expanded {
    let mut text = String::with_capacity(expanded.text.len());
    // Where each place of the text is now.
    let mut moved = Vec::with_capacity(expanded.text.len() + 1);
    let mut len = 0;
    for c in expanded.text.chars() {
        moved.push(len);
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
    moved.push(len);
    let at = |offset: usize| moved.get(offset).copied().unwrap_or(len);
    let stops = expanded
        .stops
        .into_iter()
        .map(|stop| {
            stop.into_iter()
                .map(|range| at(range.start)..at(range.end))
                .collect()
        })
        .collect();
    Expanded { text, stops }
}

struct Parser {
    chars: Vec<char>,
    at: usize,
    text: String,
    /// The length of `text`, in chars.
    len: usize,
    /// Each tabstop, placeholder and choice: its number and its text.
    stops: Vec<(u32, Range<usize>)>,
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
                self.stops.push((n, self.len..self.len));
            }
            // A variable: nothing is known of any.
            Some(c) if c.is_ascii_alphabetic() || c == '_' => self.skip_name(),
            Some('{') => {
                self.at += 1;
                let ok = match self.peek() {
                    Some(c) if c.is_ascii_digit() => {
                        let n = self.number();
                        let from = self.len;
                        let ok = self.after_stop();
                        // After what is in it, which may have places of
                        // its own.
                        self.stops.push((n, from..self.len));
                        ok
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
// A place's ranges are a list, even of one.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    fn expanded(snippet: &str) -> (String, usize) {
        let expanded = expand(snippet);
        let cursor = expanded.cursor();
        (expanded.text, cursor)
    }

    fn places(snippet: &str) -> Vec<Vec<Range<usize>>> {
        expand(snippet).stops
    }

    /// Each place to fill in, by its number, then where the cursor ends;
    /// a place written more than once has each of its ranges, the one
    /// with a placeholder first.
    #[test]
    fn the_places_go_by_their_numbers() {
        assert_eq!(
            places("add(${1:a}, ${2:b})$0"),
            [vec![4..5], vec![7..8], vec![9..9]]
        );
        // No `$0`: the end.
        assert_eq!(places("f($2, $1)"), [vec![4..4], vec![2..2], vec![5..5]]);
        assert_eq!(places("plain"), [vec![5..5]]);
        assert_eq!(
            places("${1:foo(${2:x})} $0"),
            [vec![0..6], vec![4..5], vec![7..7]]
        );
        assert_eq!(
            places("$1 = ${1:name}; $1"),
            [vec![3..7, 0..0, 9..9], vec![9..9]]
        );
        assert!(!expand("x$0").has_places());
        assert!(expand("x$1").has_places());
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
            let expanded = reindent(expand(snippet), "    ", "  ");
            let cursor = expanded.cursor();
            (expanded.text, cursor)
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
        // Every place moves with the text.
        assert_eq!(
            reindent(expand("{\n\t${1:a}\n}$0"), "  ", "\t").stops,
            [vec![5..6], vec![10..10]]
        );
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
