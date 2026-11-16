//! Lines a closed fold hides. A fold keeps its first line on screen, and
//! the lines after it, to its last, are hidden; `j`, `k` and paging go
//! over them as over one line.

/// The lines hidden in closed folds: each run as its first and last line,
/// in order, apart and not touching.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hidden(Vec<(usize, usize)>);

impl Hidden {
    /// From the lines each closed fold hides, in any order, and nested or
    /// overlapping as they may be.
    pub fn new(mut runs: Vec<(usize, usize)>) -> Self {
        runs.retain(|&(first, last)| first <= last && first > 0);
        runs.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(runs.len());
        for (first, last) in runs {
            match merged.last_mut() {
                Some(prev) if first <= prev.1 + 1 => prev.1 = prev.1.max(last),
                _ => merged.push((first, last)),
            }
        }
        Self(merged)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn runs(&self) -> &[(usize, usize)] {
        &self.0
    }

    /// The run of hidden lines `line` is in.
    pub fn around(&self, line: usize) -> Option<(usize, usize)> {
        let at = self.0.partition_point(|&(_, last)| last < line);
        self.0.get(at).copied().filter(|&(first, _)| first <= line)
    }

    pub fn is_hidden(&self, line: usize) -> bool {
        self.around(line).is_some()
    }

    /// The line on screen for `line`: itself, or the first line of the
    /// fold it is hidden in.
    pub fn shown(&self, line: usize) -> usize {
        self.around(line).map_or(line, |(first, _)| first - 1)
    }

    /// The line on screen after `line`, up to `last`.
    pub fn next(&self, line: usize, last: usize) -> Option<usize> {
        let next = match self.around(line + 1) {
            Some((_, end)) => end + 1,
            None => line + 1,
        };
        (next <= last).then_some(next)
    }

    /// The line on screen before `line`.
    pub fn prev(&self, line: usize) -> Option<usize> {
        line.checked_sub(1).map(|line| self.shown(line))
    }

    /// `n` lines on screen down from `line`, or the last there is.
    pub fn down(&self, mut line: usize, n: usize, last: usize) -> usize {
        for _ in 0..n {
            match self.next(line, last) {
                Some(next) => line = next,
                None => break,
            }
        }
        line
    }

    /// `n` lines on screen up from `line`, or the first.
    pub fn up(&self, mut line: usize, n: usize) -> usize {
        for _ in 0..n {
            match self.prev(line) {
                Some(prev) => line = prev,
                None => break,
            }
        }
        line
    }

    /// How many lines hidden there are from `from` to `to`, both counted.
    pub fn hidden_in(&self, from: usize, to: usize) -> usize {
        if to < from {
            return 0;
        }
        let at = self.0.partition_point(|&(_, last)| last < from);
        self.0[at..]
            .iter()
            .take_while(|&&(first, _)| first <= to)
            .map(|&(first, last)| last.min(to) + 1 - first.max(from))
            .sum()
    }

    /// How many lines on screen `to` is below `from`, each a line on screen.
    pub fn between(&self, from: usize, to: usize) -> usize {
        if to <= from {
            return 0;
        }
        to - from - self.hidden_in(from + 1, to)
    }
}

#[cfg(test)]
mod tests {
    use super::Hidden;

    #[test]
    fn runs_are_merged_and_walked_over() {
        // Folds of 2..=4 (nested 3..=3) and 5..=6, and one of 9..=10.
        let hidden = Hidden::new(vec![(3, 3), (5, 6), (2, 4), (9, 10)]);
        assert_eq!(hidden.runs(), [(2, 6), (9, 10)]);
        assert!(!hidden.is_hidden(1) && hidden.is_hidden(2) && hidden.is_hidden(6));
        assert_eq!(hidden.shown(4), 1);
        assert_eq!(hidden.shown(7), 7);
        assert_eq!(hidden.next(1, 20), Some(7));
        assert_eq!(hidden.next(8, 20), Some(11));
        assert_eq!(hidden.next(8, 10), None);
        assert_eq!(hidden.prev(7), Some(1));
        assert_eq!(hidden.prev(0), None);
        assert_eq!(hidden.down(0, 3, 20), 8);
        assert_eq!(hidden.down(0, 30, 20), 20);
        assert_eq!(hidden.up(11, 2), 7);
        assert_eq!(hidden.up(11, 30), 0);
        assert_eq!(hidden.between(1, 11), 3);
        assert_eq!(hidden.between(0, 1), 1);
        assert_eq!(hidden.hidden_in(4, 9), 4);
        // Nothing hidden: every line is a line.
        let none = Hidden::default();
        assert_eq!(none.down(3, 2, 10), 5);
        assert_eq!(none.between(3, 5), 2);
    }
}
