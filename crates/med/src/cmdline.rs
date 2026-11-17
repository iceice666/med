//! `Tab` on the `:` command line: the names of commands, and the argument
//! of the ones [`EX_ARGS`] says take a file or a command.
//!
//! Matches go in place, one per `Tab`, `S-Tab` going back; past the last
//! the line is as it was typed. A line changed since the last `Tab` is
//! completed afresh. While `Tab` goes through them, the matches are listed
//! above the status bar.

use crate::commands::Commands;
use crate::defaults::{EX_ARGS, ExArg};
use std::path::{Path, PathBuf};

/// The matches `Tab` is going through.
pub(crate) struct Completion {
    /// The line as typed.
    typed: String,
    /// Where on it the word being completed starts.
    start: usize,
    matches: Vec<String>,
    /// The match shown, `None` for the line as typed.
    selected: Option<usize>,
    /// The line as `Tab` last left it.
    shown: String,
}

impl Completion {
    /// Complete `line` afresh, or go on from `previous` if the line is as
    /// it left it. The line to show, if there is anything to complete.
    pub(crate) fn step(
        previous: Option<Completion>,
        line: &str,
        forward: bool,
        root: Option<&Path>,
        commands: &Commands,
    ) -> Option<(String, Option<Completion>)> {
        let mut this = match previous {
            Some(previous) if previous.shown == line => previous,
            _ => {
                let (start, matches) = matches(line, root, commands)?;
                Completion {
                    typed: line.to_string(),
                    start,
                    matches,
                    selected: None,
                    shown: String::new(),
                }
            }
        };
        let count = this.matches.len();
        this.selected = match (this.selected, forward) {
            (None, true) => Some(0),
            (None, false) => Some(count - 1),
            (Some(i), true) => (i + 1 < count).then_some(i + 1),
            (Some(i), false) => i.checked_sub(1),
        };
        let shown = match this.selected {
            Some(i) => format!("{}{}", &this.typed[..this.start], this.matches[i]),
            None => this.typed.clone(),
        };
        // One match is all there is to it: the next `Tab` starts again
        // from there (into the directory just completed, say).
        if count == 1 {
            return Some((shown, None));
        }
        this.shown = shown.clone();
        Some((shown, Some(this)))
    }

    /// The matches, the one shown, and where on the line the word they
    /// go in starts, while `line` is as `Tab` left it.
    pub(crate) fn menu(&self, line: &str) -> Option<(&[String], Option<usize>, usize)> {
        (self.shown == line).then_some((self.matches.as_slice(), self.selected, self.start))
    }
}

/// Where the word being completed starts on `line`, and what it could be.
/// `None` when there is nothing to complete there.
fn matches(line: &str, root: Option<&Path>, commands: &Commands) -> Option<(usize, Vec<String>)> {
    let start = skip_range(line);
    let rest = &line[start..];
    let found = match rest.split_once(char::is_whitespace) {
        None => {
            if rest.contains('!') {
                return None;
            }
            (start, command_names(rest, commands))
        }
        Some((name, arg)) => {
            let command = commands.resolve_ex(name.strip_suffix('!').unwrap_or(name))?;
            let kind = EX_ARGS
                .iter()
                .find(|(of, _)| *of == command.as_ref())
                .map(|(_, kind)| *kind)?;
            // The command gets all that follows as its argument, spaces
            // and all.
            let word = line.len() - arg.trim_start().len();
            let given = &line[word..];
            let found = match kind {
                ExArg::File => paths(given, root),
                ExArg::Command => help_topics(given, commands),
            };
            (word, found)
        }
    };
    (!found.1.is_empty()).then_some(found)
}

/// Past the line range a command may start with (`10,20`, `'<,'>`, `%`).
fn skip_range(line: &str) -> usize {
    let mut chars = line.char_indices().peekable();
    while let Some(&(at, c)) = chars.peek() {
        match c {
            '\'' => {
                chars.next();
                chars.next();
            }
            c if c.is_ascii_digit() || c.is_whitespace() || ".$%,;+-".contains(c) => {
                chars.next();
            }
            _ => return at,
        }
    }
    line.len()
}

/// The ex names, the engine's own and those given to [`Commands::ex`],
/// then the commands' full names, that start with `prefix`.
fn command_names(prefix: &str, commands: &Commands) -> Vec<String> {
    let mut short: Vec<String> = commands
        .all_ex_names()
        .chain(med_vim::EX_NAMES.iter().copied())
        .filter(|name| name.starts_with(prefix))
        .map(String::from)
        .collect();
    short.sort();
    short.dedup();
    let mut full: Vec<String> = commands
        .all()
        .into_iter()
        .map(|command| command.name.to_string())
        .filter(|name| name.starts_with(prefix) && !short.contains(name))
        .collect();
    full.sort();
    short.extend(full);
    short
}

/// What `:help` takes: any command's name, the engine's included.
fn help_topics(prefix: &str, commands: &Commands) -> Vec<String> {
    let mut topics = command_names(prefix, commands);
    let mut engine: Vec<String> = med_vim::command_names()
        .filter(|name| name.starts_with(prefix))
        .map(String::from)
        .filter(|name| !topics.contains(name))
        .collect();
    engine.sort();
    topics.extend(engine);
    topics
}

/// The files and directories `given` could go on to name, directories
/// with a `/` to go on into. Dot files only when `given` asks for them.
fn paths(given: &str, root: Option<&Path>) -> Vec<String> {
    let (dir, prefix) = match given.rfind('/') {
        Some(i) => given.split_at(i + 1),
        None => ("", given),
    };
    let base = if Path::new(dir).is_absolute() {
        PathBuf::from(dir)
    } else {
        match root {
            Some(root) => root.join(dir),
            None => return Vec::new(),
        }
    };
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut found: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !name.starts_with(prefix) || (name.starts_with('.') && !prefix.starts_with('.')) {
                return None;
            }
            // Through symlinks: a link to a directory is one to go into.
            let slash = if entry.path().is_dir() { "/" } else { "" };
            Some(format!("{dir}{name}{slash}"))
        })
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_the_line_range() {
        assert_eq!(skip_range("w"), 0);
        assert_eq!(skip_range("10,20d"), 5);
        assert_eq!(skip_range("'<,'>d"), 5);
        assert_eq!(skip_range("% y"), 2);
        assert_eq!(skip_range(".,$"), 3);
    }

    #[test]
    fn steps_through_the_matches_and_back_to_what_was_typed() {
        let commands = Commands::default();
        let cycle = |forward: bool, from: Option<Completion>, line: &str| {
            // Nothing in the registry, so the engine's names: `d`, `delete`.
            Completion::step(from, line, forward, None, &commands).unwrap()
        };
        let (line, state) = cycle(true, None, "de");
        assert_eq!(line, "delete");
        assert!(state.is_none(), "one match is all there is");

        let (line, state) = cycle(true, None, "1,2no");
        assert_eq!(line, "1,2noh");
        let (line, state) = cycle(true, state, &line);
        assert_eq!(line, "1,2nohl");
        let (line, state) = cycle(true, state, &line);
        assert_eq!(line, "1,2nohlsearch");
        let (line, state) = cycle(true, state, &line);
        assert_eq!(line, "1,2no");
        let (line, _) = cycle(false, state, &line);
        assert_eq!(line, "1,2nohlsearch");

        // Changed in between: the matches are those of the new line.
        let (_, state) = cycle(true, None, "no");
        let (line, _) = cycle(true, state, "ya");
        assert_eq!(line, "yank");
        let (line, _) = cycle(false, None, "no");
        assert_eq!(line, "nohlsearch");

        assert!(Completion::step(None, "zz", true, None, &commands).is_none());
        assert!(Completion::step(None, "d! x", true, None, &commands).is_none());
        // A wide space between the name and the argument.
        assert!(Completion::step(None, "h\u{3000}x", true, None, &commands).is_none());
    }
}
