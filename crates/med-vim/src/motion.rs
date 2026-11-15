//! Cursor motions as pure functions over a [`Buffer`]. All positions are
//! char offsets.

use med_core::Buffer;

pub const WS: u8 = 0;
pub const WORD: u8 = 1;
pub const PUNCT: u8 = 2;

/// Vim's character classes. With `big` (WORD motions) everything that is not
/// whitespace is one class.
pub fn class(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        WS
    } else if big || c.is_alphanumeric() || c == '_' {
        WORD
    } else {
        PUNCT
    }
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// Offset of the first non-blank char of `line` (its end if it is all blank).
pub fn first_non_blank(buf: &Buffer, line: usize) -> usize {
    let start = buf.line_start(line);
    let end = buf.line_end(line);
    let mut pos = start;
    while pos < end && is_blank(buf.char_at(pos)) {
        pos += 1;
    }
    pos
}

pub fn leading_ws(buf: &Buffer, line: usize) -> String {
    buf.slice_string(buf.line_start(line)..first_non_blank(buf, line))
}

pub fn is_blank_line(buf: &Buffer, line: usize) -> bool {
    first_non_blank(buf, line) == buf.line_end(line)
}

/// `w` / `W`
pub fn next_word_start(buf: &Buffer, mut pos: usize, big: bool) -> usize {
    let max = buf.max_offset();
    if pos >= max {
        return max;
    }
    let start = class(buf.char_at(pos), big);
    if start != WS {
        while pos < max && class(buf.char_at(pos), big) == start {
            pos += 1;
        }
    }
    while pos < max {
        let c = buf.char_at(pos);
        if c == '\n' {
            pos += 1;
            // An empty line counts as a word.
            if buf.char_at(pos) == '\n' {
                break;
            }
        } else if c.is_whitespace() {
            pos += 1;
        } else {
            break;
        }
    }
    pos.min(max)
}

/// `e` / `E`
pub fn word_end(buf: &Buffer, mut pos: usize, big: bool) -> usize {
    let max = buf.max_offset();
    if pos >= max {
        return max;
    }
    pos += 1;
    while pos < max && class(buf.char_at(pos), big) == WS {
        pos += 1;
    }
    if pos >= max {
        return max;
    }
    let cls = class(buf.char_at(pos), big);
    while pos < max && class(buf.char_at(pos + 1), big) == cls {
        pos += 1;
    }
    pos
}

fn is_empty_line_at(buf: &Buffer, pos: usize) -> bool {
    buf.char_at(pos) == '\n' && (pos == 0 || buf.char_at(pos - 1) == '\n')
}

/// `b` / `B`
pub fn prev_word_start(buf: &Buffer, mut pos: usize, big: bool) -> usize {
    if pos == 0 {
        return 0;
    }
    pos -= 1;
    while pos > 0 && class(buf.char_at(pos), big) == WS {
        if is_empty_line_at(buf, pos) {
            return pos;
        }
        pos -= 1;
    }
    let cls = class(buf.char_at(pos), big);
    if cls == WS {
        return pos;
    }
    while pos > 0 && class(buf.char_at(pos - 1), big) == cls {
        pos -= 1;
    }
    pos
}

/// `ge` / `gE`
pub fn prev_word_end(buf: &Buffer, mut pos: usize, big: bool) -> usize {
    if pos == 0 {
        return 0;
    }
    let cls = class(buf.char_at(pos), big);
    if cls != WS {
        while pos > 0 && class(buf.char_at(pos), big) == cls {
            pos -= 1;
        }
        if class(buf.char_at(pos), big) == cls {
            return 0;
        }
    }
    while pos > 0 && class(buf.char_at(pos), big) == WS {
        if is_empty_line_at(buf, pos) {
            return pos;
        }
        pos -= 1;
    }
    pos
}

/// A pending or repeatable `f`/`F`/`t`/`T`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Find {
    pub c: char,
    pub forward: bool,
    pub till: bool,
}

/// Find the `count`-th `find.c` on the cursor's line. `repeat` is set for
/// `;`/`,`, where a `t` must skip the char it is already sitting next to.
pub fn find_char(
    buf: &Buffer,
    cur: usize,
    find: Find,
    count: usize,
    repeat: bool,
) -> Option<usize> {
    let line = buf.line_of(cur);
    let (ls, le) = (buf.line_start(line), buf.line_end(line));
    let skip = if find.till && repeat { 2 } else { 1 };
    let mut remaining = count.max(1);
    if find.forward {
        let mut pos = cur + skip;
        while pos < le {
            if buf.char_at(pos) == find.c {
                remaining -= 1;
                if remaining == 0 {
                    return Some(if find.till { pos - 1 } else { pos });
                }
            }
            pos += 1;
        }
    } else {
        let mut pos = cur.checked_sub(skip)?;
        loop {
            if pos < ls {
                break;
            }
            if buf.char_at(pos) == find.c {
                remaining -= 1;
                if remaining == 0 {
                    return Some(if find.till { pos + 1 } else { pos });
                }
            }
            if pos == 0 {
                break;
            }
            pos -= 1;
        }
    }
    None
}

const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

/// `%`: jump to the bracket matching the first bracket at or after the cursor
/// on its line.
pub fn match_pair(buf: &Buffer, cur: usize) -> Option<usize> {
    let le = buf.line_end(buf.line_of(cur));
    let mut pos = cur;
    let (open, close, forward) = loop {
        if pos >= le {
            return None;
        }
        let c = buf.char_at(pos);
        if let Some(&(o, cl)) = PAIRS.iter().find(|(o, _)| *o == c) {
            break (o, cl, true);
        }
        if let Some(&(o, cl)) = PAIRS.iter().find(|(_, cl)| *cl == c) {
            break (o, cl, false);
        }
        pos += 1;
    };
    let max = buf.max_offset();
    let mut depth = 0usize;
    if forward {
        let mut i = pos + 1;
        while i <= max {
            let c = buf.char_at(i);
            if c == open {
                depth += 1;
            } else if c == close {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            i += 1;
        }
    } else {
        let mut i = pos;
        while i > 0 {
            i -= 1;
            let c = buf.char_at(i);
            if c == close {
                depth += 1;
            } else if c == open {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
        }
    }
    None
}

/// `}`
pub fn paragraph_forward(buf: &Buffer, cur: usize) -> usize {
    let last = buf.last_line();
    let mut line = buf.line_of(cur);
    while line < last && buf.line_len(line) == 0 {
        line += 1;
    }
    while line < last && buf.line_len(line) != 0 {
        line += 1;
    }
    if buf.line_len(line) != 0 {
        buf.line_end(line)
            .saturating_sub(1)
            .max(buf.line_start(line))
    } else {
        buf.line_start(line)
    }
}

/// `{`
pub fn paragraph_backward(buf: &Buffer, cur: usize) -> usize {
    let mut line = buf.line_of(cur);
    while line > 0 && buf.line_len(line) == 0 {
        line -= 1;
    }
    while line > 0 && buf.line_len(line) != 0 {
        line -= 1;
    }
    buf.line_start(line)
}

/// The identifier under (or, failing that, after) the cursor on its line.
pub fn word_under_cursor(buf: &Buffer, cur: usize) -> Option<(usize, usize)> {
    let line = buf.line_of(cur);
    let (ls, le) = (buf.line_start(line), buf.line_end(line));
    let mut pos = cur.min(le);
    while pos < le && class(buf.char_at(pos), false) != WORD {
        pos += 1;
    }
    if pos >= le {
        return None;
    }
    let mut start = pos;
    while start > ls && class(buf.char_at(start - 1), false) == WORD {
        start -= 1;
    }
    let mut end = pos + 1;
    while end < le && class(buf.char_at(end), false) == WORD {
        end += 1;
    }
    Some((start, end))
}
