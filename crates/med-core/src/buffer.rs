use ropey::{Rope, RopeSlice};
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// A (row, column) pair. What `col` counts depends on where the point came
/// from: chars for cursor positions, bytes for tree-sitter, UTF-16 code units
/// for LSP. The producing method always says which.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub row: usize,
    pub col: usize,
}

impl Point {
    pub fn new(row: usize, col: usize) -> Self {
        Self { row, col }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

/// One applied change, described in every coordinate system a consumer might
/// need (tree-sitter wants bytes + byte points, LSP wants UTF-16 points).
/// "old" coordinates are relative to the text before the change.
#[derive(Clone, Debug)]
pub struct Edit {
    pub start_char: usize,
    pub old_end_char: usize,
    pub new_end_char: usize,
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    /// Column in bytes.
    pub start_point: Point,
    pub old_end_point: Point,
    pub new_end_point: Point,
    /// Column in UTF-16 code units.
    pub start_utf16: Point,
    pub old_end_utf16: Point,
    pub new_text: String,
}

#[derive(Clone)]
struct Change {
    at: usize,
    removed: String,
    inserted: String,
}

#[derive(Clone)]
struct Transaction {
    id: u64,
    changes: Vec<Change>,
    cursor_before: usize,
}

/// A buffer's undo history as data, to keep beyond the buffer: what
/// [`Buffer::history`] gives and [`Buffer::restore_history`] takes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    /// Oldest first; the last is what `u` undoes.
    pub undo: Vec<HistoryStep>,
    /// The last is what a redo does again.
    pub redo: Vec<HistoryStep>,
}

/// One undoable step: its changes in the order they were made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryStep {
    pub changes: Vec<HistoryChange>,
    pub cursor_before: usize,
}

/// `inserted` in place of `removed`, at a char offset of the text as it
/// was just before.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryChange {
    pub at: usize,
    pub removed: String,
    pub inserted: String,
}

impl From<&Transaction> for HistoryStep {
    fn from(tx: &Transaction) -> Self {
        let changes = tx.changes.iter().map(|c| HistoryChange {
            at: c.at,
            removed: c.removed.clone(),
            inserted: c.inserted.clone(),
        });
        Self {
            changes: changes.collect(),
            cursor_before: tx.cursor_before,
        }
    }
}

/// Make each change of `step` again on `rope`, or take each back, if it
/// fits what is there; whether it did.
fn replay(rope: &mut Rope, step: &HistoryStep, undo: bool) -> bool {
    let apply = |rope: &mut Rope, change: &HistoryChange| {
        let (gone, put) = if undo {
            (&change.inserted, &change.removed)
        } else {
            (&change.removed, &change.inserted)
        };
        let end = change.at + gone.chars().count();
        if end > rope.len_chars() || rope.slice(change.at..end) != gone.as_str() {
            return false;
        }
        rope.remove(change.at..end);
        rope.insert(change.at, put);
        true
    };
    if undo {
        step.changes.iter().rev().all(|c| apply(rope, c))
    } else {
        step.changes.iter().all(|c| apply(rope, c))
    }
}

/// A text buffer with undo history.
///
/// Invariant: the text always ends with `\n`. Every line the user can see is
/// therefore terminated, the cursor never has to sit on a phantom trailing
/// line, and `line_count()` is simply the number of `\n`s.
///
/// All offsets are char offsets unless a method says otherwise.
pub struct Buffer {
    rope: Rope,
    version: u64,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    open: Option<Transaction>,
    depth: usize,
    next_tx_id: u64,
    saved_tx: u64,
    edits: Vec<Edit>,
    pub line_ending: LineEnding,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new("")
    }
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        let line_ending = if text.contains("\r\n") {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        };
        let mut rope = if line_ending == LineEnding::CrLf {
            Rope::from_str(&text.replace("\r\n", "\n"))
        } else {
            Rope::from_str(text)
        };
        let len = rope.len_chars();
        if len == 0 || rope.char(len - 1) != '\n' {
            rope.insert_char(len, '\n');
        }
        Self {
            rope,
            version: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            open: None,
            depth: 0,
            next_tx_id: 1,
            saved_tx: 0,
            edits: Vec::new(),
            line_ending,
        }
    }

    // ---- reading -------------------------------------------------------

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Text as it should be written to disk.
    pub fn to_disk_string(&self) -> String {
        let text = self.rope.to_string();
        match self.line_ending {
            LineEnding::Lf => text,
            LineEnding::CrLf => text.replace('\n', "\r\n"),
        }
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// Bumped on every change, including undo/redo.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// The last offset a cursor may occupy (the final newline).
    pub fn max_offset(&self) -> usize {
        self.rope.len_chars() - 1
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines() - 1
    }

    pub fn last_line(&self) -> usize {
        self.line_count() - 1
    }

    pub fn line_start(&self, line: usize) -> usize {
        self.rope.line_to_char(line.min(self.last_line()))
    }

    /// Length of the line in chars, excluding the newline.
    pub fn line_len(&self, line: usize) -> usize {
        let line = line.min(self.last_line());
        self.rope.line(line).len_chars() - 1
    }

    /// Offset of the line's terminating newline.
    pub fn line_end(&self, line: usize) -> usize {
        self.line_start(line) + self.line_len(line)
    }

    pub fn line_of(&self, offset: usize) -> usize {
        self.rope.char_to_line(offset.min(self.max_offset()))
    }

    /// Line content without its newline.
    pub fn line(&self, line: usize) -> RopeSlice<'_> {
        let start = self.line_start(line);
        self.rope.slice(start..start + self.line_len(line))
    }

    pub fn line_string(&self, line: usize) -> String {
        self.line(line).to_string()
    }

    /// The char at `offset`; offsets past the end read as `\n`.
    pub fn char_at(&self, offset: usize) -> char {
        if offset < self.rope.len_chars() {
            self.rope.char(offset)
        } else {
            '\n'
        }
    }

    pub fn slice(&self, range: Range<usize>) -> RopeSlice<'_> {
        let end = range.end.min(self.rope.len_chars());
        self.rope.slice(range.start.min(end)..end)
    }

    pub fn slice_string(&self, range: Range<usize>) -> String {
        self.slice(range).to_string()
    }

    /// (line, column-in-chars) of an offset.
    pub fn offset_to_point(&self, offset: usize) -> Point {
        let offset = offset.min(self.max_offset());
        let row = self.rope.char_to_line(offset);
        Point::new(row, offset - self.rope.line_to_char(row))
    }

    /// Offset of (line, column-in-chars), clamped to the line.
    pub fn point_to_offset(&self, point: Point) -> usize {
        let row = point.row.min(self.last_line());
        self.line_start(row) + point.col.min(self.line_len(row))
    }

    pub fn char_to_byte(&self, offset: usize) -> usize {
        self.rope.char_to_byte(offset.min(self.rope.len_chars()))
    }

    pub fn byte_to_char(&self, byte: usize) -> usize {
        self.rope.byte_to_char(byte.min(self.rope.len_bytes()))
    }

    /// (line, column-in-bytes), as tree-sitter wants it.
    pub fn offset_to_byte_point(&self, offset: usize) -> Point {
        let offset = offset.min(self.rope.len_chars());
        let row = self.rope.char_to_line(offset);
        let col = self.rope.char_to_byte(offset) - self.rope.line_to_byte(row);
        Point::new(row, col)
    }

    /// (line, column-in-UTF-16-units), as LSP wants it.
    pub fn offset_to_utf16(&self, offset: usize) -> Point {
        let offset = offset.min(self.rope.len_chars());
        let row = self.rope.char_to_line(offset);
        let line_start = self.rope.line_to_char(row);
        let col = self.rope.char_to_utf16_cu(offset) - self.rope.char_to_utf16_cu(line_start);
        Point::new(row, col)
    }

    /// Inverse of [`offset_to_utf16`], clamped to the buffer and the line.
    pub fn utf16_to_offset(&self, point: Point) -> usize {
        if point.row > self.last_line() {
            return self.max_offset();
        }
        let line_start = self.rope.line_to_char(point.row);
        let line_end = self.line_end(point.row);
        let base = self.rope.char_to_utf16_cu(line_start);
        let max = self.rope.char_to_utf16_cu(line_end);
        let target = (base + point.col).min(max);
        self.rope.utf16_cu_to_char(target)
    }

    // ---- editing -------------------------------------------------------

    /// Replace `range` with `text`. Recorded for undo and reported through
    /// [`take_edits`].
    pub fn edit(&mut self, range: Range<usize>, text: &str) {
        let len = self.rope.len_chars();
        let start = range.start.min(len);
        let end = range.end.clamp(start, len);
        if start == end && text.is_empty() {
            return;
        }
        let implicit = self.depth == 0;
        if implicit {
            self.begin(start);
        }
        let text = if text.contains('\r') {
            std::borrow::Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
        } else {
            std::borrow::Cow::Borrowed(text)
        };
        let removed = self.raw_replace(start, end, &text);
        self.record(start, removed, text.into_owned());

        let len = self.rope.len_chars();
        if len == 0 || self.rope.char(len - 1) != '\n' {
            self.raw_replace(len, len, "\n");
            self.record(len, String::new(), "\n".to_string());
        }
        if implicit {
            self.end();
        }
    }

    pub fn insert(&mut self, at: usize, text: &str) {
        self.edit(at..at, text);
    }

    pub fn delete(&mut self, range: Range<usize>) {
        self.edit(range, "");
    }

    fn record(&mut self, at: usize, removed: String, inserted: String) {
        if let Some(tx) = &mut self.open {
            tx.changes.push(Change {
                at,
                removed,
                inserted,
            });
        }
    }

    /// Apply a replacement to the rope, log an [`Edit`], return removed text.
    fn raw_replace(&mut self, start: usize, end: usize, text: &str) -> String {
        let start_byte = self.rope.char_to_byte(start);
        let old_end_byte = self.rope.char_to_byte(end);
        let start_point = self.offset_to_byte_point(start);
        let old_end_point = self.offset_to_byte_point(end);
        let start_utf16 = self.offset_to_utf16(start);
        let old_end_utf16 = self.offset_to_utf16(end);

        let removed = self.rope.slice(start..end).to_string();
        if end > start {
            self.rope.remove(start..end);
        }
        if !text.is_empty() {
            self.rope.insert(start, text);
        }
        let new_end_char = start + text.chars().count();
        self.version += 1;
        self.edits.push(Edit {
            start_char: start,
            old_end_char: end,
            new_end_char,
            start_byte,
            old_end_byte,
            new_end_byte: start_byte + text.len(),
            start_point,
            old_end_point,
            new_end_point: self.offset_to_byte_point(new_end_char),
            start_utf16,
            old_end_utf16,
            new_text: text.to_string(),
        });
        removed
    }

    /// The edits [`take_edits`](Self::take_edits) would give, left where
    /// they are. Each is one step of [`version`](Self::version).
    pub fn edits(&self) -> &[Edit] {
        &self.edits
    }

    /// Edits applied since the last call, oldest first.
    pub fn take_edits(&mut self) -> Vec<Edit> {
        std::mem::take(&mut self.edits)
    }

    // ---- undo ----------------------------------------------------------

    /// Open an undo group. Groups nest; only the outermost one is recorded.
    /// `cursor` is where undo should put the cursor back.
    pub fn begin(&mut self, cursor: usize) {
        if self.depth == 0 {
            self.open = Some(Transaction {
                id: self.next_tx_id,
                changes: Vec::new(),
                cursor_before: cursor,
            });
            self.next_tx_id += 1;
        }
        self.depth += 1;
    }

    pub fn end(&mut self) {
        if self.depth == 0 {
            return;
        }
        self.depth -= 1;
        if self.depth == 0
            && let Some(tx) = self.open.take()
            && !tx.changes.is_empty()
        {
            self.undo.push(tx);
            self.redo.clear();
        }
    }

    pub fn in_transaction(&self) -> bool {
        self.depth > 0
    }

    /// Undo the last group; returns where the cursor should go.
    pub fn undo(&mut self) -> Option<usize> {
        if self.depth > 0 {
            return None;
        }
        let tx = self.undo.pop()?;
        for change in tx.changes.iter().rev() {
            let end = change.at + change.inserted.chars().count();
            self.raw_replace(change.at, end, &change.removed);
        }
        let cursor = tx.cursor_before.min(self.max_offset());
        self.redo.push(tx);
        Some(cursor)
    }

    /// Redo the last undone group; returns where the cursor should go.
    pub fn redo(&mut self) -> Option<usize> {
        if self.depth > 0 {
            return None;
        }
        let tx = self.redo.pop()?;
        for change in &tx.changes {
            let end = change.at + change.removed.chars().count();
            self.raw_replace(change.at, end, &change.inserted);
        }
        let cursor = tx
            .changes
            .first()
            .map_or(0, |c| c.at)
            .min(self.max_offset());
        self.undo.push(tx);
        Some(cursor)
    }

    fn current_tx(&self) -> u64 {
        self.undo.last().map_or(0, |tx| tx.id)
    }

    pub fn is_dirty(&self) -> bool {
        self.current_tx() != self.saved_tx
    }

    pub fn mark_saved(&mut self) {
        self.saved_tx = self.current_tx();
    }

    /// The undo history, as data: at most the last `limit` steps to undo,
    /// the step being made (an insert not yet left) the last of them, as
    /// the text has it already.
    pub fn history(&self, limit: usize) -> History {
        let open = self.open.as_ref().filter(|tx| !tx.changes.is_empty());
        let mut undo: Vec<HistoryStep> = self.undo.iter().chain(open).map(Into::into).collect();
        undo.drain(..undo.len().saturating_sub(limit));
        History {
            undo,
            redo: self.redo.iter().map(Into::into).collect(),
        }
    }

    /// Take up a history kept from before, of this very text, as though
    /// it had been made here and then saved. Refused, and the buffer left
    /// as it was, where the buffer has a history of its own already, or
    /// where a step does not fit the text it would undo or redo.
    pub fn restore_history(&mut self, history: History) -> bool {
        if self.depth > 0 || !self.undo.is_empty() || !self.redo.is_empty() {
            return false;
        }
        // Back to the start and on to the end again, on a copy.
        let mut rope = self.rope.clone();
        let fits = history
            .undo
            .iter()
            .rev()
            .all(|s| replay(&mut rope, s, true))
            && history.undo.iter().all(|s| replay(&mut rope, s, false))
            && history
                .redo
                .iter()
                .rev()
                .all(|s| replay(&mut rope, s, false))
            && history.redo.iter().all(|s| replay(&mut rope, s, true));
        if !fits || rope != self.rope {
            return false;
        }
        let mut id = 0;
        let mut transaction = |step: HistoryStep| {
            id += 1;
            Transaction {
                id,
                changes: (step.changes.into_iter())
                    .map(|c| Change {
                        at: c.at,
                        removed: c.removed,
                        inserted: c.inserted,
                    })
                    .collect(),
                cursor_before: step.cursor_before,
            }
        };
        self.undo = history.undo.into_iter().map(&mut transaction).collect();
        self.redo = history.redo.into_iter().map(&mut transaction).collect();
        self.next_tx_id = id + 1;
        self.saved_tx = self.current_tx();
        true
    }

    /// Replace the whole text (e.g. reload from disk) as one undoable step,
    /// touching only the part that actually differs.
    pub fn set_text(&mut self, text: &str) {
        let mut new: Vec<char> = text.replace("\r\n", "\n").chars().collect();
        if new.last() != Some(&'\n') {
            new.push('\n');
        }
        let old: Vec<char> = self.rope.chars().collect();
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let replacement: String = new[prefix..new.len() - suffix].iter().collect();
        self.edit(prefix..old.len() - suffix, &replacement);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_taken_up_again_by_the_same_text() {
        let mut b = Buffer::new("one\n");
        for (at, text) in [(3, " two"), (7, " three"), (0, "zero ")] {
            b.begin(at);
            b.insert(at, text);
            b.end();
        }
        b.undo();
        let history = b.history(100);
        assert_eq!((history.undo.len(), history.redo.len()), (2, 1));

        // The text as saved, with what came before it and after.
        let mut again = Buffer::new(&b.text());
        assert!(again.restore_history(history.clone()));
        assert!(!again.is_dirty());
        again.undo();
        assert_eq!(again.text(), "one two\n");
        assert!(again.is_dirty());
        again.undo();
        assert_eq!(again.text(), "one\n");
        assert_eq!(again.undo(), None);
        again.redo();
        again.redo();
        again.redo();
        assert_eq!(again.text(), "zero one two three\n");

        // Not another text, nor one with a history of its own.
        let mut other = Buffer::new("one two thre\n");
        assert!(!other.restore_history(history.clone()));
        assert_eq!(other.undo(), None);
        assert!(!again.restore_history(history));
    }

    #[test]
    fn history_has_the_step_being_made_and_no_more_than_asked() {
        let mut b = Buffer::new("\n");
        for n in 0..5 {
            b.begin(n);
            b.insert(n, "x");
            b.end();
        }
        // An insert not yet left: its text is there, so it is a step.
        b.begin(5);
        b.insert(5, "y");
        let history = b.history(3);
        assert_eq!(history.undo.len(), 3);
        assert_eq!(history.undo[2].changes[0].inserted, "y");

        let mut again = Buffer::new("xxxxxy\n");
        assert!(again.restore_history(history));
        for _ in 0..3 {
            again.undo();
        }
        assert_eq!(again.text(), "xxx\n");
        assert_eq!(again.undo(), None);
    }

    #[test]
    fn always_ends_with_newline() {
        assert_eq!(Buffer::new("").text(), "\n");
        assert_eq!(Buffer::new("a").text(), "a\n");
        assert_eq!(Buffer::new("a\n").text(), "a\n");
        let mut b = Buffer::new("a\nb\n");
        b.delete(0..b.len_chars());
        assert_eq!(b.text(), "\n");
        assert_eq!(b.line_count(), 1);
    }

    #[test]
    fn lines() {
        let b = Buffer::new("ab\n\ncde");
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line_len(0), 2);
        assert_eq!(b.line_len(1), 0);
        assert_eq!(b.line_len(2), 3);
        assert_eq!(b.line_start(2), 4);
        assert_eq!(b.line_end(2), 7);
        assert_eq!(b.line_string(2), "cde");
        assert_eq!(b.line_of(3), 1);
        assert_eq!(b.max_offset(), 7);
    }

    #[test]
    fn undo_redo_groups() {
        let mut b = Buffer::new("hello\n");
        b.begin(0);
        b.insert(5, " world");
        b.insert(0, ">> ");
        b.end();
        assert_eq!(b.text(), ">> hello world\n");
        assert!(b.is_dirty());
        assert_eq!(b.undo(), Some(0));
        assert_eq!(b.text(), "hello\n");
        assert!(!b.is_dirty());
        assert_eq!(b.redo(), Some(5));
        assert_eq!(b.text(), ">> hello world\n");
        b.mark_saved();
        assert!(!b.is_dirty());
        b.undo();
        assert!(b.is_dirty());
    }

    #[test]
    fn undo_restores_final_newline_fixup() {
        let mut b = Buffer::new("a\nb\n");
        b.delete(1..4);
        assert_eq!(b.text(), "a\n");
        b.undo();
        assert_eq!(b.text(), "a\nb\n");
    }

    #[test]
    fn edit_coordinates() {
        let mut b = Buffer::new("a好b\nxyz\n");
        b.edit(1..2, "😀\n");
        let e = b.take_edits().pop().unwrap();
        assert_eq!((e.start_byte, e.old_end_byte, e.new_end_byte), (1, 4, 6));
        assert_eq!(e.start_point, Point::new(0, 1));
        assert_eq!(e.old_end_point, Point::new(0, 4));
        assert_eq!(e.new_end_point, Point::new(1, 0));
        assert_eq!(e.start_utf16, Point::new(0, 1));
        assert_eq!(e.old_end_utf16, Point::new(0, 2));
        assert_eq!(b.text(), "a😀\nb\nxyz\n");
    }

    #[test]
    fn utf16_roundtrip() {
        let b = Buffer::new("a😀b\n好\n");
        assert_eq!(b.offset_to_utf16(2), Point::new(0, 3));
        assert_eq!(b.utf16_to_offset(Point::new(0, 3)), 2);
        assert_eq!(b.utf16_to_offset(Point::new(0, 99)), 3);
        assert_eq!(b.utf16_to_offset(Point::new(1, 1)), 5);
        assert_eq!(b.utf16_to_offset(Point::new(9, 0)), b.max_offset());
    }

    #[test]
    fn crlf_roundtrip() {
        let mut b = Buffer::new("a\r\nb\r\n");
        assert_eq!(b.text(), "a\nb\n");
        b.insert(1, "!");
        assert_eq!(b.to_disk_string(), "a!\r\nb\r\n");
    }

    #[test]
    fn set_text_minimal_diff() {
        let mut b = Buffer::new("one\ntwo\nthree\n");
        b.take_edits();
        b.set_text("one\nTWO\nthree\n");
        let edits = b.take_edits();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "TWO");
        assert_eq!(b.text(), "one\nTWO\nthree\n");
        b.undo();
        assert_eq!(b.text(), "one\ntwo\nthree\n");
    }
}
