//! Parsing for the bits of the ex command line the engine runs itself:
//! line ranges, `:s`, and search patterns.

use regex::{Regex, RegexBuilder};

pub struct RangeCtx {
    pub cur: usize,
    pub last: usize,
    pub visual: Option<(usize, usize)>,
}

fn parse_addr<'a>(s: &'a str, cx: &RangeCtx) -> (Option<usize>, &'a str) {
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    let (mut base, mut rest) = if digits > 0 {
        let n: usize = s[..digits].parse().unwrap_or(usize::MAX);
        (Some(n.saturating_sub(1)), &s[digits..])
    } else if let Some(rest) = s.strip_prefix('.') {
        (Some(cx.cur), rest)
    } else if let Some(rest) = s.strip_prefix('$') {
        (Some(cx.last), rest)
    } else if let Some(rest) = s.strip_prefix("'<") {
        match cx.visual {
            Some((a, _)) => (Some(a), rest),
            None => return (None, s),
        }
    } else if let Some(rest) = s.strip_prefix("'>") {
        match cx.visual {
            Some((_, b)) => (Some(b), rest),
            None => return (None, s),
        }
    } else {
        (None, s)
    };
    loop {
        let sign = match rest.as_bytes().first() {
            Some(b'+') => 1i64,
            Some(b'-') => -1i64,
            _ => break,
        };
        let after = &rest[1..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let n: i64 = if digits > 0 {
            after[..digits].parse().unwrap_or(i64::MAX / 2)
        } else {
            1
        };
        let from = base.unwrap_or(cx.cur) as i64;
        base = Some((from + sign * n).max(0) as usize);
        rest = &after[digits..];
    }
    (base.map(|b| b.min(cx.last)), rest)
}

/// Parse a leading line range. Returns 0-based inclusive lines and the rest
/// of the command.
pub fn parse_range<'a>(input: &'a str, cx: &RangeCtx) -> (Option<(usize, usize)>, &'a str) {
    if let Some(rest) = input.strip_prefix('%') {
        return (Some((0, cx.last)), rest);
    }
    let (first, rest) = parse_addr(input, cx);
    let Some(a) = first else {
        return (None, input);
    };
    if let Some(after) = rest.strip_prefix(',') {
        let (second, rest) = parse_addr(after, cx);
        let b = second.unwrap_or(cx.cur);
        (Some((a.min(b), a.max(b))), rest)
    } else {
        (Some((a, a)), rest)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Substitute {
    pub pattern: String,
    /// Already translated to the `regex` crate's replacement syntax.
    pub replacement: String,
    pub global: bool,
    pub ignore_case: bool,
}

/// Parse `s/pat/rep/flags` (any punctuation delimiter). `None` if `cmd` is
/// not a substitute command.
pub fn parse_substitute(cmd: &str) -> Option<Substitute> {
    let rest = cmd.strip_prefix('s')?;
    let delim = rest.chars().next()?;
    if delim.is_alphanumeric() || delim.is_whitespace() || delim == '"' || delim == '|' {
        return None;
    }
    let mut parts: Vec<String> = vec![String::new()];
    let mut chars = rest[delim.len_utf8()..].chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(&n) if n == delim => {
                    parts.last_mut().unwrap().push(delim);
                    chars.next();
                }
                Some(&n) => {
                    let part = parts.last_mut().unwrap();
                    part.push('\\');
                    part.push(n);
                    chars.next();
                }
                None => parts.last_mut().unwrap().push('\\'),
            }
        } else if c == delim && parts.len() < 3 {
            parts.push(String::new());
        } else {
            parts.last_mut().unwrap().push(c);
        }
    }
    let pattern = parts[0].clone();
    let replacement = translate_replacement(parts.get(1).map_or("", String::as_str));
    let flags = parts.get(2).map_or("", String::as_str);
    Some(Substitute {
        pattern,
        replacement,
        global: flags.contains('g'),
        ignore_case: flags.contains('i'),
    })
}

/// Vim replacement syntax (`\1`, `&`) → `regex` crate syntax (`${1}`, `${0}`).
fn translate_replacement(rep: &str) -> String {
    let mut out = String::new();
    let mut chars = rep.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(d) if d.is_ascii_digit() => {
                    out.push_str("${");
                    out.push(d);
                    out.push('}');
                }
                Some('n') | Some('r') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            },
            '&' => out.push_str("${0}"),
            '$' => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out
}

/// Compile a search pattern with smartcase. Patterns use Rust regex syntax;
/// vim's `\<`/`\>` word anchors and `\c`/`\C` case switches are accepted, and
/// anything that fails to compile is searched for literally.
pub fn build_regex(pattern: &str, ignore_case: bool) -> Regex {
    let mut pat = pattern.replace("\\<", "\\b").replace("\\>", "\\b");
    let mut case_insensitive = ignore_case || !pat.chars().any(char::is_uppercase);
    if pat.contains("\\c") {
        pat = pat.replace("\\c", "");
        case_insensitive = true;
    }
    if pat.contains("\\C") {
        pat = pat.replace("\\C", "");
        case_insensitive = false;
    }
    let build = |p: &str| {
        RegexBuilder::new(p)
            .case_insensitive(case_insensitive)
            .multi_line(true)
            .build()
    };
    build(&pat).unwrap_or_else(|_| build(&regex::escape(&pat)).expect("escaped pattern compiles"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx() -> RangeCtx {
        RangeCtx {
            cur: 4,
            last: 9,
            visual: Some((2, 3)),
        }
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("%s/a/b/", &cx()), (Some((0, 9)), "s/a/b/"));
        assert_eq!(parse_range("3", &cx()), (Some((2, 2)), ""));
        assert_eq!(parse_range("2,4d", &cx()), (Some((1, 3)), "d"));
        assert_eq!(parse_range(".,$d", &cx()), (Some((4, 9)), "d"));
        assert_eq!(parse_range("'<,'>s/x/y/", &cx()), (Some((2, 3)), "s/x/y/"));
        assert_eq!(parse_range(".,+2d", &cx()), (Some((4, 6)), "d"));
        assert_eq!(parse_range("w", &cx()), (None, "w"));
        assert_eq!(parse_range("999", &cx()), (Some((9, 9)), ""));
    }

    #[test]
    fn substitute() {
        let s = parse_substitute("s/a\\/b/[&]\\1/gi").unwrap();
        assert_eq!(s.pattern, "a/b");
        assert_eq!(s.replacement, "[${0}]${1}");
        assert!(s.global && s.ignore_case);
        assert!(parse_substitute("set nu").is_none());
        assert!(parse_substitute("sp").is_none());
        let s = parse_substitute("s#x#y").unwrap();
        assert_eq!(
            (s.pattern.as_str(), s.replacement.as_str(), s.global),
            ("x", "y", false)
        );
    }

    #[test]
    fn smartcase() {
        assert!(build_regex("foo", false).is_match("FOO"));
        assert!(!build_regex("Foo", false).is_match("foo"));
        assert!(build_regex("a(b", false).is_match("xa(b"));
        assert!(build_regex("\\<id\\>", false).is_match("an id here"));
        assert!(!build_regex("\\<id\\>", false).is_match("idle"));
    }
}
