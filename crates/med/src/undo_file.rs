//! Undo history kept with a file between runs, vim's `undofile`.
//!
//! When the `undo-file` option is on, each save writes the tab's history
//! to `<state>/undo/`, in a file named by the path it is the history of,
//! and opening that file again takes it up. It goes with the text it was
//! written with, by its length and a hash of it: a file changed since,
//! elsewhere, keeps none. Nothing about it is ever an error: a history
//! that cannot be written or read is none.

use med_core::{Buffer, History};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Steps of history written, the oldest let go first: vim's `undolevels`.
pub const LEVELS: usize = 1000;

const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct UndoFile {
    format: u32,
    /// The text the history ends at, as the buffer has it (`\n` line
    /// endings, a final one): its length in bytes, and its hash.
    len: usize,
    hash: u64,
    history: History,
}

/// FNV-1a: the same from one build to another, which std's hasher is not
/// said to be. It tells texts apart; it does not keep secrets.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Where the history of `file` is kept in `dir`: by a hash of its full
/// path, which is never too long a name, and its own name, to read.
pub fn path_for(dir: &Path, file: &Path) -> PathBuf {
    let file = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned());
    let hash = fnv(file.as_os_str().as_encoded_bytes());
    dir.join(format!("{hash:016x}-{}.json", name.unwrap_or_default()))
}

/// Keep `buf`'s history as the history of `file`, which holds its text.
pub fn write(dir: &Path, file: &Path, buf: &Buffer) {
    let text = buf.text();
    let kept = UndoFile {
        format: FORMAT,
        len: text.len(),
        hash: fnv(text.as_bytes()),
        history: buf.history(LEVELS),
    };
    let Ok(json) = serde_json::to_vec(&kept) else {
        return;
    };
    let path = path_for(dir, file);
    // Whole or not at all: a save cut short leaves the last one.
    let partial = path.with_extension("json.partial");
    let written = std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(&partial, json))
        .and_then(|()| std::fs::rename(&partial, &path));
    if written.is_err() {
        std::fs::remove_file(&partial).ok();
    }
}

/// Take up the history kept for `file` into `buf`, just made from its
/// text: whether there was one, of this text, that fits it.
pub fn read(dir: &Path, file: &Path, buf: &mut Buffer) -> bool {
    let Ok(json) = std::fs::read(path_for(dir, file)) else {
        return false;
    };
    let Ok(kept) = serde_json::from_slice::<UndoFile>(&json) else {
        return false;
    };
    let text = buf.text();
    kept.format == FORMAT
        && kept.len == text.len()
        && kept.hash == fnv(text.as_bytes())
        && buf.restore_history(kept.history)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_is_the_published_one() {
        // The FNV-1a 64 test vectors.
        assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn a_deep_path_still_has_a_short_name() {
        let deep: PathBuf = std::iter::repeat_n("directory-with-a-long-name", 40).collect();
        let path = path_for(Path::new("/state/undo"), &deep.join("main.rs"));
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.len() < 40 && name.ends_with("-main.rs.json"), "{name}");
    }
}
