//! Going to text on screen by typing some of it: what `motion.flash` looks
//! for, and the letters it puts on what it finds.
//!
//! Every character typed is added to the text looked for, and every place
//! on screen that has that text gets a letter, its label. Typing a label
//! goes to its place. A letter that comes right after the text somewhere
//! is never a label, so a key is one or the other: more of the text, or
//! the place to go to.
//!
//! The same labels go on the syntax nodes around the cursor, for
//! `select.flash` to select one by, and the places a find on screen could
//! go to next are marked the same way, without labels.

use crate::vim::Viewport;
use med_core::Buffer;
use std::collections::HashMap;

/// The labels, the easiest to type first. Their capitals come after them.
const LABELS: &str = "asdfghjklqwertyuiopzxcvbnm";

/// A place that is marked: one that has the text looked for, a syntax
/// node, or a character a find can go to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashMatch {
    /// `start..end` in chars.
    pub start: usize,
    pub end: usize,
    /// The key that goes there. `None` once the labels have run out, and
    /// for what a find marks.
    pub label: Option<char>,
    /// `enter` goes there: the first place after the cursor, or the
    /// smallest node.
    pub target: bool,
}

/// What there is to mark on screen, for a host to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashView<'a> {
    /// The places, in the order of the text; nodes from the smallest out.
    pub matches: &'a [FlashMatch],
    /// `start..end` in chars: the text to dim but for the places.
    pub dim: (usize, usize),
    /// The places are syntax nodes: a label goes before and after each,
    /// between the characters rather than over one, and the text of a
    /// node is not marked.
    pub nodes: bool,
}

/// What the keys after the command are for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Text to look for, then the label of a place to go to.
    #[default]
    Text,
    /// The same, for an operator that is to work there: it goes there
    /// and waits for its motion, and the cursor comes back afterwards.
    Remote,
    /// The label of a syntax node to select.
    Nodes,
}

/// A search in progress.
#[derive(Default)]
pub(crate) struct Flash {
    pub kind: Kind,
    /// The text looked for, as typed so far.
    pub pattern: String,
    /// Where it is on screen, in the order of the text.
    pub matches: Vec<FlashMatch>,
    /// The labels places were given, by where they start: a place keeps
    /// its label while more of the text is typed.
    used: HashMap<usize, char>,
    /// How many keys of the command were typed before the text.
    pub typed: usize,
}

impl Flash {
    /// A search that begins after `typed` keys of a command.
    pub fn new(kind: Kind, typed: usize) -> Self {
        Self {
            kind,
            typed,
            ..Self::default()
        }
    }

    /// The syntax nodes `nodes`, from the smallest out, each with a label
    /// while there are labels.
    pub fn nodes(nodes: &[(usize, usize)], typed: usize) -> Self {
        let labels = LABELS.chars().chain(capitals()).map(Some);
        let labels = labels.chain(std::iter::repeat(None));
        let matches = nodes.iter().zip(labels).enumerate();
        Self {
            kind: Kind::Nodes,
            typed,
            matches: matches
                .map(|(i, (&(start, end), label))| FlashMatch {
                    start,
                    end,
                    label,
                    target: i == 0,
                })
                .collect(),
            ..Self::default()
        }
    }

    /// The place with the label `c`.
    pub fn labelled(&self, c: char) -> Option<FlashMatch> {
        let found = self.matches.iter().find(|m| m.label == Some(c));
        found.copied()
    }

    /// The place `enter` goes to.
    pub fn target(&self) -> Option<FlashMatch> {
        self.matches.iter().find(|m| m.target).copied()
    }

    /// Look for the text in the lines on screen and label what is found,
    /// the nearest to the cursor first. Capitals in the text are looked
    /// for as they are; without any, so are small letters and capitals
    /// alike. The place the cursor is at is not one to go to.
    pub fn update(&mut self, buf: &Buffer, viewport: Viewport, cursor: usize) {
        self.matches.clear();
        let pattern: Vec<char> = self.pattern.chars().collect();
        if pattern.is_empty() {
            return;
        }
        let exact = pattern.iter().any(|c| c.is_uppercase());
        let same = |a: char, b: char| a == b || (!exact && a.to_lowercase().eq(b.to_lowercase()));

        let last = buf.last_line();
        let top = viewport.top.min(last);
        let bottom = (top + viewport.height.saturating_sub(1)).min(last);
        // What comes right after the text, wherever it was found.
        let mut follows: Vec<char> = Vec::new();
        for row in top..=bottom {
            let line_start = buf.line_start(row);
            let line: Vec<char> = buf.line_string(row).chars().collect();
            let mut col = 0;
            while col + pattern.len() <= line.len() {
                let end = col + pattern.len();
                let found = line[col..end]
                    .iter()
                    .zip(&pattern)
                    .all(|(a, b)| same(*a, *b));
                if !found {
                    col += 1;
                    continue;
                }
                if line_start + col != cursor {
                    follows.extend(line.get(end));
                    self.matches.push(FlashMatch {
                        start: line_start + col,
                        end: line_start + end,
                        label: None,
                        target: false,
                    });
                }
                col = end;
            }
        }

        let after = self.matches.iter().position(|m| m.start > cursor);
        if let Some(target) = self.matches.get_mut(after.unwrap_or(0)) {
            target.target = true;
        }

        let mut free: Vec<char> = LABELS
            .chars()
            .chain(capitals())
            .filter(|label| !follows.iter().any(|c| same(*c, *label)))
            .collect();

        let here = buf.offset_to_point(cursor);
        let mut nearest: Vec<usize> = (0..self.matches.len()).collect();
        nearest.sort_by_key(|&i| {
            let at = buf.offset_to_point(self.matches[i].start);
            (at.row.abs_diff(here.row), at.col.abs_diff(here.col), i)
        });
        // First the labels places already had, then the rest.
        for &i in &nearest {
            let had = self.used.get(&self.matches[i].start);
            if let Some(at) = had.and_then(|had| free.iter().position(|c| c == had)) {
                self.matches[i].label = Some(free.remove(at));
            }
        }
        let mut free = free.into_iter();
        for &i in &nearest {
            if self.matches[i].label.is_some() {
                continue;
            }
            let Some(label) = free.next() else {
                break;
            };
            self.matches[i].label = Some(label);
            // A capital is only lent: it goes back when the labels are
            // given out again, and a small letter may come free by then.
            if label.is_lowercase() {
                self.used.insert(self.matches[i].start, label);
            }
        }
    }
}

fn capitals() -> impl Iterator<Item = char> {
    LABELS.chars().map(|c| c.to_ascii_uppercase())
}

/// Every `c` in `start..end`, for a find to mark where it can go next.
pub(crate) fn occurrences(buf: &Buffer, c: char, (start, end): (usize, usize)) -> Vec<FlashMatch> {
    let found = (start..end).filter(|&at| buf.char_at(at) == c);
    found
        .map(|at| FlashMatch {
            start: at,
            end: at + 1,
            label: None,
            target: false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flash(text: &str, pattern: &str, cursor: usize) -> Flash {
        let buf = Buffer::new(text);
        let mut flash = Flash::new(Kind::Text, 0);
        let viewport = Viewport { top: 0, height: 10 };
        for c in pattern.chars() {
            flash.pattern.push(c);
            flash.update(&buf, viewport, cursor);
        }
        flash
    }

    fn labels(flash: &Flash) -> Vec<(usize, Option<char>)> {
        flash.matches.iter().map(|m| (m.start, m.label)).collect()
    }

    #[test]
    fn labels_the_nearest_first() {
        // The cursor is on the last line: its `ab` is the nearest.
        let found = flash("ab\nxx ab\nab x\n", "ab", 12);
        assert_eq!(
            labels(&found),
            [(0, Some('d')), (6, Some('s')), (9, Some('a'))]
        );
        // `enter` goes to the first one after the cursor, or failing
        // that to the first there is.
        let target = |cursor| flash("ab ab ab\n", "ab", cursor).target();
        assert_eq!(target(1).map(|m| m.start), Some(3));
        assert_eq!(target(7).map(|m| m.start), Some(0));
    }

    #[test]
    fn a_label_is_never_what_could_be_typed_next() {
        // After `fo` comes an `a` in one place and an `s` in another.
        let found = flash("foa fos fox\n", "fo", 11);
        // None of the three is a label; `d` stays where `f` alone put it.
        let given: Vec<char> = found.matches.iter().filter_map(|m| m.label).collect();
        assert_eq!(given, ['d', 'g', 'f']);
        // Capitals and small letters are alike until one is typed.
        let found = flash("foA fo\n", "fo", 6);
        assert_eq!(labels(&found), [(0, Some('s')), (4, Some('d'))]);
        let found = flash("FoA Fo fo\n", "Fo", 9);
        assert_eq!(labels(&found), [(0, Some('s')), (4, Some('a'))]);
    }

    #[test]
    fn a_place_keeps_its_label() {
        let buf = Buffer::new("fox foo fog\n");
        let viewport = Viewport { top: 0, height: 10 };
        let mut found = Flash::new(Kind::Text, 0);
        found.pattern = "fo".into();
        found.update(&buf, viewport, 11);
        let fox = found.matches[0].label;
        assert!(fox.is_some());
        found.pattern.push('x');
        found.update(&buf, viewport, 11);
        assert_eq!(labels(&found), [(0, fox)]);
    }

    #[test]
    fn looks_only_at_what_is_on_screen() {
        let buf = Buffer::new("ab\nab\nab\nab\n");
        let mut found = Flash::new(Kind::Text, 0);
        found.pattern = "ab".into();
        found.update(&buf, Viewport { top: 1, height: 2 }, 0);
        assert_eq!(
            found.matches.iter().map(|m| m.start).collect::<Vec<_>>(),
            [3, 6]
        );
        // The place the cursor is at is left out.
        found.update(&buf, Viewport { top: 0, height: 4 }, 3);
        assert_eq!(
            found.matches.iter().map(|m| m.start).collect::<Vec<_>>(),
            [0, 6, 9]
        );
    }

    #[test]
    fn nodes_are_labelled_from_the_smallest_out() {
        let found = Flash::nodes(&[(4, 7), (0, 9), (0, 20)], 1);
        let labels: Vec<_> = found
            .matches
            .iter()
            .map(|m| (m.start, m.end, m.label))
            .collect();
        assert_eq!(
            labels,
            [(4, 7, Some('a')), (0, 9, Some('s')), (0, 20, Some('d'))]
        );
        assert_eq!(found.target().map(|m| (m.start, m.end)), Some((4, 7)));
        assert_eq!(found.labelled('s').map(|m| m.end), Some(9));
        // More nodes than labels: the outermost go without.
        let many: Vec<(usize, usize)> = (0..60).map(|i| (60 - i, 60 + i)).collect();
        let found = Flash::nodes(&many, 1);
        assert_eq!(found.matches[51].label, Some('M'));
        assert_eq!(found.matches[52].label, None);
    }

    #[test]
    fn capitals_once_the_small_letters_are_gone() {
        let text = "x ".repeat(30) + "\n";
        let found = flash(&text, "x", 59);
        assert_eq!(found.matches.len(), 30);
        let given: String = found.matches.iter().rev().filter_map(|m| m.label).collect();
        // Nearest first: the small letters, then their capitals.
        assert_eq!(given, "asdfghjklqwertyuiopzxcvbnmASDF");
    }
}
