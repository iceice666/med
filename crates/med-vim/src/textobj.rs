//! Text objects (`iw`, `a"`, `i{`, `ap`, …).

use crate::motion::{WS, class, is_blank_line};
use med_core::Buffer;

/// A text object's extent: `start..end` in chars. For `linewise` objects the
/// range covers whole lines including the last one's newline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjRange {
    pub start: usize,
    pub end: usize,
    pub linewise: bool,
}

impl ObjRange {
    fn chars(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            linewise: false,
        }
    }
}

pub fn text_object(buf: &Buffer, pos: usize, kind: char, around: bool) -> Option<ObjRange> {
    match kind {
        'w' => Some(word(buf, pos, around, false)),
        'W' => Some(word(buf, pos, around, true)),
        '"' | '\'' | '`' => quote(buf, pos, kind, around),
        '(' | ')' | 'b' => bracket(buf, pos, '(', ')', around),
        '[' | ']' => bracket(buf, pos, '[', ']', around),
        '{' | '}' | 'B' => bracket(buf, pos, '{', '}', around),
        '<' | '>' => bracket(buf, pos, '<', '>', around),
        'p' => Some(paragraph(buf, pos, around)),
        _ => None,
    }
}

/// Where the delimiters `kind` stands for are, around `pos`: the opening
/// one and the closing one. Brackets are matched across lines, anything
/// else pairs up along the line.
pub fn delimiters(buf: &Buffer, pos: usize, kind: char) -> Option<(usize, usize)> {
    let bracket = |open, close| {
        let around = bracket(buf, pos, open, close, true)?;
        Some((around.start, around.end - 1))
    };
    match kind {
        '(' | ')' | 'b' => bracket('(', ')'),
        '[' | ']' | 'r' => bracket('[', ']'),
        '{' | '}' | 'B' => bracket('{', '}'),
        '<' | '>' | 'a' => bracket('<', '>'),
        c => {
            let inner = quote(buf, pos, c, false)?;
            Some((inner.start - 1, inner.end))
        }
    }
}

fn word(buf: &Buffer, pos: usize, around: bool, big: bool) -> ObjRange {
    let line = buf.line_of(pos);
    let (ls, le) = (buf.line_start(line), buf.line_end(line));
    if ls == le {
        return ObjRange::chars(ls, ls);
    }
    let pos = pos.min(le - 1);
    let cls = class(buf.char_at(pos), big);
    let mut start = pos;
    while start > ls && class(buf.char_at(start - 1), big) == cls {
        start -= 1;
    }
    let mut end = pos + 1;
    while end < le && class(buf.char_at(end), big) == cls {
        end += 1;
    }
    if around {
        if cls == WS {
            if end < le {
                let next = class(buf.char_at(end), big);
                while end < le && class(buf.char_at(end), big) == next {
                    end += 1;
                }
            }
        } else {
            let word_end = end;
            while end < le && class(buf.char_at(end), big) == WS {
                end += 1;
            }
            if end == word_end {
                while start > ls && class(buf.char_at(start - 1), big) == WS {
                    start -= 1;
                }
            }
        }
    }
    ObjRange::chars(start, end)
}

fn quote(buf: &Buffer, pos: usize, q: char, around: bool) -> Option<ObjRange> {
    let line = buf.line_of(pos);
    let (ls, le) = (buf.line_start(line), buf.line_end(line));
    let mut quotes = Vec::new();
    let mut i = ls;
    while i < le {
        match buf.char_at(i) {
            '\\' => i += 1,
            c if c == q => quotes.push(i),
            _ => {}
        }
        i += 1;
    }
    let pairs = quotes.as_chunks::<2>().0.iter().map(|p| (p[0], p[1]));
    let (open, close) = pairs
        .clone()
        .find(|&(a, b)| a <= pos && pos <= b)
        .or_else(|| pairs.clone().find(|&(a, _)| a > pos))?;
    if !around {
        return Some(ObjRange::chars(open + 1, close));
    }
    let mut start = open;
    let mut end = close + 1;
    let after = end;
    while end < le && matches!(buf.char_at(end), ' ' | '\t') {
        end += 1;
    }
    if end == after {
        while start > ls && matches!(buf.char_at(start - 1), ' ' | '\t') {
            start -= 1;
        }
    }
    Some(ObjRange::chars(start, end))
}

fn bracket(buf: &Buffer, pos: usize, open: char, close: char, around: bool) -> Option<ObjRange> {
    let max = buf.max_offset();
    // Walk left to the enclosing opener.
    let mut depth = 0usize;
    let mut i = pos;
    let start = loop {
        let c = buf.char_at(i);
        if c == open {
            if depth == 0 {
                break i;
            }
            depth -= 1;
        } else if c == close && i != pos {
            depth += 1;
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    };
    // Walk right to its closer.
    depth = 0;
    let mut j = start + 1;
    let end = loop {
        if j > max {
            return None;
        }
        let c = buf.char_at(j);
        if c == open {
            depth += 1;
        } else if c == close {
            if depth == 0 {
                break j;
            }
            depth -= 1;
        }
        j += 1;
    };
    if around {
        return Some(ObjRange::chars(start, end + 1));
    }

    let mut inner_start = start + 1;
    let close_line = buf.line_of(end);
    if close_line > buf.line_of(start) && buf.char_at(inner_start) == '\n' {
        inner_start += 1;
        // `{` and `}` on their own lines: the object is the lines in between.
        let close_ls = buf.line_start(close_line);
        let only_indent = (close_ls..end).all(|k| matches!(buf.char_at(k), ' ' | '\t'));
        if only_indent {
            return Some(if close_ls > inner_start {
                ObjRange {
                    start: inner_start,
                    end: close_ls,
                    linewise: true,
                }
            } else {
                ObjRange::chars(inner_start - 1, inner_start - 1)
            });
        }
    }
    Some(ObjRange::chars(inner_start, end))
}

fn paragraph(buf: &Buffer, pos: usize, around: bool) -> ObjRange {
    let last = buf.last_line();
    let line = buf.line_of(pos);
    let blank = is_blank_line(buf, line);
    let mut first = line;
    while first > 0 && is_blank_line(buf, first - 1) == blank {
        first -= 1;
    }
    let mut end_line = line;
    while end_line < last && is_blank_line(buf, end_line + 1) == blank {
        end_line += 1;
    }
    if around {
        let before = end_line;
        while end_line < last && is_blank_line(buf, end_line + 1) != blank {
            end_line += 1;
        }
        // No trailing separator: take the one before instead.
        if end_line == before && !blank {
            while first > 0 && is_blank_line(buf, first - 1) {
                first -= 1;
            }
        }
    }
    ObjRange {
        start: buf.line_start(first),
        end: buf.line_end(end_line) + 1,
        linewise: true,
    }
}
