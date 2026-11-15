//! How the window is split: panes side by side and one above another,
//! split again as deep as need be. Each pane has tabs of its own; the
//! tree says only where the panes are.

/// A pane, by a number that stays its own while panes come and go.
pub(crate) type PaneId = usize;

/// Which way a split lays its parts out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    /// Side by side, as `:vsplit` makes them.
    Row,
    /// One above another, as `:split` makes them.
    Column,
}

/// Where to go from a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn axis(self) -> Axis {
        match self {
            Direction::Left | Direction::Right => Axis::Row,
            Direction::Up | Direction::Down => Axis::Column,
        }
    }

    /// Toward the end of a split: right, or down.
    fn forward(self) -> bool {
        matches!(self, Direction::Right | Direction::Down)
    }
}

/// The panes, as a tree. A split always has two parts or more, and none
/// of its parts is a split the same way: three panes side by side are one
/// split of three, so that each is as wide as the others.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    Pane(PaneId),
    Split(Axis, Vec<Layout>),
}

impl Layout {
    /// The panes, from the top left: left to right, top to bottom, as
    /// they are nested.
    pub(crate) fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Layout::Pane(id) => out.push(*id),
            Layout::Split(_, parts) => parts.iter().for_each(|part| part.collect(out)),
        }
    }

    /// Put `new` beside `at`, after it: right of it in a row, below it in
    /// a column. False if there is no pane `at`.
    pub(crate) fn split(&mut self, at: PaneId, axis: Axis, new: PaneId) -> bool {
        match self {
            Layout::Pane(id) if *id == at => {
                *self = Layout::Split(axis, vec![Layout::Pane(at), Layout::Pane(new)]);
                true
            }
            Layout::Pane(_) => false,
            Layout::Split(way, parts) => {
                let found = parts.iter().position(|part| *part == Layout::Pane(at));
                match found {
                    // Another part of the same split, as wide as the rest.
                    Some(index) if *way == axis => {
                        parts.insert(index + 1, Layout::Pane(new));
                        true
                    }
                    Some(index) => {
                        let both = vec![Layout::Pane(at), Layout::Pane(new)];
                        parts[index] = Layout::Split(axis, both);
                        true
                    }
                    None => parts.iter_mut().any(|part| part.split(at, axis, new)),
                }
            }
        }
    }

    /// Take the pane `id` away, the space it had going to the panes
    /// beside it. False if it is not there, or is the only one.
    pub(crate) fn remove(&mut self, id: PaneId) -> bool {
        let Layout::Split(way, parts) = self else {
            return false;
        };
        let removed = match parts.iter().position(|part| *part == Layout::Pane(id)) {
            Some(index) => {
                parts.remove(index);
                true
            }
            None => parts.iter_mut().any(|part| part.remove(id)),
        };
        if removed {
            // A part left alone, or a part now split the same way as this.
            let way = *way;
            let flat = std::mem::take(parts)
                .into_iter()
                .flat_map(|part| match part {
                    Layout::Split(inner, inner_parts) if inner == way => inner_parts,
                    part => vec![part],
                });
            *parts = flat.collect();
            if parts.len() == 1 {
                *self = parts.pop().expect("one part");
            }
        }
        removed
    }

    /// The pane next to `from` in `direction`, if there is one: in the
    /// nearest split that way, the part beside the one `from` is in, and
    /// in that part the pane on the side nearest `from`.
    pub(crate) fn neighbor(&self, from: PaneId, direction: Direction) -> Option<PaneId> {
        let mut path = Vec::new();
        if !self.path_to(from, &mut path) {
            return None;
        }
        // From the innermost split out.
        let mut node_path = path.as_slice();
        while let Some((&index, outer)) = node_path.split_last() {
            let split = self.at(outer);
            if let Layout::Split(way, parts) = split
                && *way == direction.axis()
            {
                let next = match direction.forward() {
                    true => index.checked_add(1).filter(|next| *next < parts.len()),
                    false => index.checked_sub(1),
                };
                if let Some(next) = next {
                    return Some(parts[next].edge(direction));
                }
            }
            node_path = outer;
        }
        None
    }

    /// The indices of the parts, from the top, down to the pane `id`.
    fn path_to(&self, id: PaneId, path: &mut Vec<usize>) -> bool {
        match self {
            Layout::Pane(here) => *here == id,
            Layout::Split(_, parts) => parts.iter().enumerate().any(|(index, part)| {
                path.push(index);
                let found = part.path_to(id, path);
                if !found {
                    path.pop();
                }
                found
            }),
        }
    }

    fn at(&self, path: &[usize]) -> &Layout {
        path.iter().fold(self, |node, index| match node {
            Layout::Split(_, parts) => &parts[*index],
            Layout::Pane(_) => node,
        })
    }

    /// The pane of this part that is met first coming in `direction`: its
    /// leftmost going right, its rightmost going left; across a split
    /// the other way, its first.
    fn edge(&self, direction: Direction) -> PaneId {
        match self {
            Layout::Pane(id) => *id,
            Layout::Split(way, parts) => {
                let part = match *way == direction.axis() && !direction.forward() {
                    true => parts.last(),
                    false => parts.first(),
                };
                part.expect("a split has parts").edge(direction)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Axis::{Column, Row};
    use super::Direction::{Down, Left, Right, Up};
    use super::*;

    fn pane(id: PaneId) -> Layout {
        Layout::Pane(id)
    }

    #[test]
    fn splitting_the_same_way_adds_a_part() {
        let mut layout = pane(0);
        assert!(layout.split(0, Row, 1));
        assert!(layout.split(0, Row, 2));
        assert_eq!(layout, Layout::Split(Row, vec![pane(0), pane(2), pane(1)]));
        assert_eq!(layout.panes(), [0, 2, 1]);
        assert!(!layout.split(9, Row, 3));
    }

    #[test]
    fn splitting_the_other_way_nests() {
        let mut layout = pane(0);
        layout.split(0, Row, 1);
        layout.split(1, Column, 2);
        assert_eq!(
            layout,
            Layout::Split(
                Row,
                vec![pane(0), Layout::Split(Column, vec![pane(1), pane(2)])]
            )
        );
        assert_eq!(layout.panes(), [0, 1, 2]);
    }

    #[test]
    fn removing_gives_the_space_back() {
        let mut layout = pane(0);
        layout.split(0, Row, 1);
        layout.split(1, Column, 2);
        assert!(layout.remove(2));
        assert_eq!(layout, Layout::Split(Row, vec![pane(0), pane(1)]));
        assert!(layout.remove(0));
        assert_eq!(layout, pane(1));
        // The last pane stays.
        assert!(!layout.remove(1));
        assert!(!layout.remove(7));
    }

    #[test]
    fn removing_flattens_what_is_left_the_same_way() {
        // 0 | (1 / (2 | 3)): without 1, the column is (2 | 3) alone, which
        // goes into the row beside 0.
        let mut layout = pane(0);
        layout.split(0, Row, 1);
        layout.split(1, Column, 2);
        layout.split(2, Row, 3);
        assert!(layout.remove(1));
        assert_eq!(layout, Layout::Split(Row, vec![pane(0), pane(2), pane(3)]));
    }

    #[test]
    fn neighbors_by_direction() {
        // 0 | (1 / 2)
        let mut layout = pane(0);
        layout.split(0, Row, 1);
        layout.split(1, Column, 2);
        assert_eq!(layout.neighbor(0, Right), Some(1));
        assert_eq!(layout.neighbor(0, Left), None);
        assert_eq!(layout.neighbor(0, Down), None);
        assert_eq!(layout.neighbor(1, Down), Some(2));
        assert_eq!(layout.neighbor(2, Up), Some(1));
        assert_eq!(layout.neighbor(2, Left), Some(0));
        assert_eq!(layout.neighbor(1, Right), None);
        assert_eq!(layout.neighbor(9, Right), None);
    }

    #[test]
    fn going_left_comes_in_at_the_right_edge() {
        // (0 | 1) / 2, then 2 split into 2 | 3: up from 3 is the column's
        // first part, the row 0 | 1, entered at its first pane.
        let mut layout = pane(0);
        layout.split(0, Column, 2);
        layout.split(0, Row, 1);
        layout.split(2, Row, 3);
        assert_eq!(layout.neighbor(3, Up), Some(0));
        assert_eq!(layout.neighbor(3, Left), Some(2));
        assert_eq!(layout.neighbor(1, Left), Some(0));
        // ((0 | 1) / 2) | 3: left from 3 goes into the column at its top,
        // and into the row there at its right.
        let mut layout = pane(0);
        layout.split(0, Row, 3);
        layout.split(0, Column, 2);
        layout.split(0, Row, 1);
        assert_eq!(layout.neighbor(3, Left), Some(1));
        assert_eq!(layout.neighbor(1, Right), Some(3));
        assert_eq!(layout.neighbor(2, Right), Some(3));
    }
}
