//! What a project had open: its panes, how they were split, the files in
//! each and where the cursor was in them. Written when med quits, read
//! when it next opens the same root with no files named.
//!
//! One file per root, under [`crate::Settings::session_dir`], named after
//! the root's path (`/a/b` is `%a%b.json`). Terminals and the texts of
//! programs are not kept: only files that are on disk.

use crate::pane::Axis;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// The format [`Session`] is written in; a file of any other is not read.
const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Session {
    version: u32,
    pub layout: Saved,
}

/// The panes, as [`crate::pane::Layout`] has them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Saved {
    Pane(SavedPane),
    Split(SavedAxis, Vec<Saved>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SavedAxis {
    Row,
    Column,
}

impl From<Axis> for SavedAxis {
    fn from(axis: Axis) -> Self {
        match axis {
            Axis::Row => SavedAxis::Row,
            Axis::Column => SavedAxis::Column,
        }
    }
}

impl From<SavedAxis> for Axis {
    fn from(axis: SavedAxis) -> Self {
        match axis {
            SavedAxis::Row => Axis::Row,
            SavedAxis::Column => Axis::Column,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedPane {
    pub tabs: Vec<SavedTab>,
    /// The tab shown, as an index into `tabs`.
    pub active: usize,
    /// It had the keys.
    #[serde(default)]
    pub focused: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedTab {
    pub path: PathBuf,
    /// Where the cursor was, both from 0.
    pub line: usize,
    pub col: usize,
}

impl Session {
    pub(crate) fn new(layout: Saved) -> Self {
        Self {
            version: VERSION,
            layout,
        }
    }
}

/// The file the session of `root` is kept in.
pub(crate) fn file(dir: &Path, root: &Path) -> PathBuf {
    let mut name = String::new();
    for c in root.to_string_lossy().chars() {
        match c {
            // `%` first, so that a `%` of the path's own is told apart.
            '%' => name.push_str("%%"),
            '/' | '\\' => name.push('%'),
            ':' => name.push_str("%="),
            c => name.push(c),
        }
    }
    // Too long a name for the file system: as much of it as reads well,
    // and what tells it from every other root.
    if name.len() > MAX_NAME {
        let mut cut = MAX_NAME - 17;
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        name = format!(
            "{}~{:016x}",
            &name[..cut],
            fnv1a(root.as_os_str().as_encoded_bytes())
        );
    }
    dir.join(format!("{name}.json"))
}

/// The longest a session's name gets before `.json`, well under the 255
/// bytes most file systems allow.
const MAX_NAME: usize = 200;

/// FNV-1a: the same for a path from one build of med to the next, which
/// std's hasher is not promised to be.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The session kept for `root`. `Ok(None)` if there is none; an error if
/// there is one that cannot be read, or is of another version.
pub(crate) fn read(dir: &Path, root: &Path) -> Result<Option<Session>, String> {
    let path = file(dir, root);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let bad = |error: serde_json::Error| format!("{}: {error}", path.display());
    // The version first: another one's layout may be nothing like this one's.
    #[derive(Deserialize)]
    struct Header {
        version: u32,
    }
    let header: Header = serde_json::from_str(&text).map_err(bad)?;
    if header.version != VERSION {
        return Err(format!(
            "{}: a session of version {}, not {VERSION}",
            path.display(),
            header.version
        ));
    }
    serde_json::from_str(&text).map(Some).map_err(bad)
}

/// Keep `session` as `root`'s, or with `None` forget the one there was.
/// Written whole or not at all: to a file beside it, then moved.
pub(crate) fn write(dir: &Path, root: &Path, session: Option<&Session>) -> io::Result<()> {
    let path = file(dir, root);
    let Some(session) = session else {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    };
    std::fs::create_dir_all(dir)?;
    let text = serde_json::to_string_pretty(session).map_err(io::Error::other)?;
    let partial = path.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })
}

/// `layout` with only the tabs `keep` is true of, and so with no pane left
/// without a tab: a split left with one part is that part, and one left
/// split the same way as the split it is in is part of that. Each pane's
/// tab shown is the same tab if it is still there, and some pane has the
/// keys. `None` if no tab is left.
pub(crate) fn prune(layout: Saved, keep: &impl Fn(&SavedTab) -> bool) -> Option<Saved> {
    let mut pruned = prune_in(layout, keep)?;
    if !any_focused(&pruned) {
        first_pane(&mut pruned).focused = true;
    }
    Some(pruned)
}

fn prune_in(layout: Saved, keep: &impl Fn(&SavedTab) -> bool) -> Option<Saved> {
    match layout {
        Saved::Pane(pane) => {
            // Asked once a tab: `keep` may remember what it was asked.
            let kept: Vec<bool> = pane.tabs.iter().map(keep).collect();
            let shown = pane.active.min(pane.tabs.len().saturating_sub(1));
            let before = kept[..shown].iter().filter(|kept| **kept).count();
            let shown_kept = kept.get(shown).copied().unwrap_or(false);
            let tabs: Vec<SavedTab> = pane
                .tabs
                .into_iter()
                .zip(kept)
                .filter_map(|(tab, kept)| kept.then_some(tab))
                .collect();
            if tabs.is_empty() {
                return None;
            }
            // The tab shown went: the one before it, as when it is closed.
            let active = match shown_kept {
                true => before,
                false => before.saturating_sub(1),
            }
            .min(tabs.len() - 1);
            Some(Saved::Pane(SavedPane {
                tabs,
                active,
                focused: pane.focused,
            }))
        }
        Saved::Split(axis, parts) => {
            let mut kept = Vec::new();
            for part in parts.into_iter().filter_map(|part| prune_in(part, keep)) {
                match part {
                    Saved::Split(inner, inner_parts) if inner == axis => kept.extend(inner_parts),
                    part => kept.push(part),
                }
            }
            match kept.len() {
                0 => None,
                1 => kept.pop(),
                _ => Some(Saved::Split(axis, kept)),
            }
        }
    }
}

fn any_focused(layout: &Saved) -> bool {
    match layout {
        Saved::Pane(pane) => pane.focused,
        Saved::Split(_, parts) => parts.iter().any(any_focused),
    }
}

fn first_pane(layout: &mut Saved) -> &mut SavedPane {
    match layout {
        Saved::Pane(pane) => pane,
        Saved::Split(_, parts) => first_pane(&mut parts[0]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(name: &str) -> SavedTab {
        SavedTab {
            path: PathBuf::from(name),
            line: 0,
            col: 0,
        }
    }

    fn pane(names: &[&str], active: usize, focused: bool) -> Saved {
        Saved::Pane(SavedPane {
            tabs: names.iter().map(|name| tab(name)).collect(),
            active,
            focused,
        })
    }

    fn without<'a>(gone: &'a [&'a str]) -> impl Fn(&SavedTab) -> bool + 'a {
        move |tab| !gone.iter().any(|name| tab.path == Path::new(name))
    }

    #[test]
    fn names_a_file_for_each_root() {
        let dir = Path::new("/state");
        assert_eq!(
            file(dir, Path::new("/a/b")),
            PathBuf::from("/state/%a%b.json")
        );
        // Not the same file as `/a/b`'s, nor as `/a%b`'s.
        assert_eq!(
            file(dir, Path::new("/a%b")),
            PathBuf::from("/state/%a%%b.json")
        );
        assert_ne!(file(dir, Path::new("/a/b")), file(dir, Path::new("/a%b")));

        // A deep root: a name short enough to write, still its own.
        let deep = format!("/{}", "directory/".repeat(40));
        let other = format!("{deep}x");
        let name = |root: &str| {
            let path = file(dir, Path::new(root));
            path.file_name().unwrap().to_string_lossy().into_owned()
        };
        assert!(name(&deep).len() <= MAX_NAME + ".json".len());
        assert!(name(&deep).starts_with("%directory%directory%"));
        assert_ne!(name(&deep), name(&other));
        assert_eq!(name(&deep), name(&deep));
    }

    #[test]
    fn keeps_the_layout_when_every_tab_is_there() {
        let layout = Saved::Split(
            SavedAxis::Row,
            vec![pane(&["a", "b"], 1, false), pane(&["c"], 0, true)],
        );
        assert_eq!(prune(layout.clone(), &|_| true), Some(layout));
    }

    #[test]
    fn a_pane_with_no_tab_left_goes() {
        // a | (b / c): with b gone, a | c.
        let layout = Saved::Split(
            SavedAxis::Row,
            vec![
                pane(&["a"], 0, false),
                Saved::Split(
                    SavedAxis::Column,
                    vec![pane(&["b"], 0, true), pane(&["c"], 0, false)],
                ),
            ],
        );
        assert_eq!(
            prune(layout, &without(&["b"])),
            // The pane that had the keys went: the first one has them.
            Some(Saved::Split(
                SavedAxis::Row,
                vec![pane(&["a"], 0, true), pane(&["c"], 0, false)],
            ))
        );
    }

    #[test]
    fn a_split_left_the_same_way_as_its_own_joins_it() {
        // a | (b / (c | d)): with b gone, a | c | d, three as wide.
        let layout = Saved::Split(
            SavedAxis::Row,
            vec![
                pane(&["a"], 0, true),
                Saved::Split(
                    SavedAxis::Column,
                    vec![
                        pane(&["b"], 0, false),
                        Saved::Split(
                            SavedAxis::Row,
                            vec![pane(&["c"], 0, false), pane(&["d"], 0, false)],
                        ),
                    ],
                ),
            ],
        );
        assert_eq!(
            prune(layout, &without(&["b"])),
            Some(Saved::Split(
                SavedAxis::Row,
                vec![
                    pane(&["a"], 0, true),
                    pane(&["c"], 0, false),
                    pane(&["d"], 0, false),
                ],
            ))
        );
    }

    #[test]
    fn the_tab_shown_stays_shown() {
        let layout = pane(&["a", "b", "c", "d"], 2, true);
        assert_eq!(
            prune(layout.clone(), &without(&["a"])),
            Some(pane(&["b", "c", "d"], 1, true))
        );
        // Gone itself: the one before it.
        assert_eq!(
            prune(layout.clone(), &without(&["c"])),
            Some(pane(&["a", "b", "d"], 1, true))
        );
        assert_eq!(
            prune(layout.clone(), &without(&["a", "b", "c"])),
            Some(pane(&["d"], 0, true))
        );
        assert_eq!(prune(layout, &without(&["a", "b", "c", "d"])), None);
    }

    #[test]
    fn asks_of_each_tab_once() {
        // As restoring does, to give a file to the first pane that has it.
        let seen = std::cell::RefCell::new(std::collections::HashSet::new());
        let keep = |tab: &SavedTab| seen.borrow_mut().insert(tab.path.clone());
        let layout = Saved::Split(
            SavedAxis::Row,
            vec![pane(&["a", "b"], 1, false), pane(&["b", "c"], 0, true)],
        );
        assert_eq!(
            prune(layout, &keep),
            Some(Saved::Split(
                SavedAxis::Row,
                vec![pane(&["a", "b"], 1, false), pane(&["c"], 0, true)],
            ))
        );
    }

    #[test]
    fn reads_what_it_wrote() {
        let dir = std::env::temp_dir().join(format!("med-session-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = Path::new("/some/project");
        assert_eq!(read(&dir, root), Ok(None));

        let session = Session::new(Saved::Split(
            SavedAxis::Column,
            vec![pane(&["a", "b"], 1, false), pane(&["c"], 0, true)],
        ));
        write(&dir, root, Some(&session)).unwrap();
        assert_eq!(read(&dir, root), Ok(Some(session)));

        // Another version, or not JSON: an error to show, not a panic.
        std::fs::write(file(&dir, root), r#"{"version":99,"layout":null}"#).unwrap();
        assert!(read(&dir, root).unwrap_err().contains("version 99"));
        std::fs::write(file(&dir, root), "nope").unwrap();
        assert!(read(&dir, root).is_err());

        write(&dir, root, None).unwrap();
        assert_eq!(read(&dir, root), Ok(None));
        // Forgetting what is not there is nothing to report.
        write(&dir, root, None).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
