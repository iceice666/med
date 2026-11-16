//! The projects med has opened, the last first: what `(recent-projects)`
//! lists for a program to open again.
//!
//! One file, [`crate::Settings::projects_file`], a project's directory to
//! a line. A project is put first each time a window opens on it. Apart
//! from the sessions, which a project with nothing open has none of.

use std::io;
use std::path::{Path, PathBuf};

/// No more projects than this are remembered: the oldest go.
const MAX: usize = 100;

/// The projects in `file`, the last opened first. None if it is not there.
pub(crate) fn read(file: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Put `root` first in `file`.
pub(crate) fn remember(file: &Path, root: &Path) -> io::Result<()> {
    // A directory whose name has a line break in it cannot be one line.
    if root.as_os_str().as_encoded_bytes().contains(&b'\n') {
        return Ok(());
    }
    let mut projects = read(file);
    projects.retain(|project| project != root);
    projects.insert(0, root.to_path_buf());
    projects.truncate(MAX);
    write(file, &projects)
}

/// Written whole or not at all: to a file beside it, then moved.
fn write(file: &Path, projects: &[PathBuf]) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = String::new();
    for project in projects {
        text.push_str(&project.to_string_lossy());
        text.push('\n');
    }
    let partial = file.with_extension(format!("{}", std::process::id()));
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, file).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_puts_a_project_first_once() {
        let dir = std::env::temp_dir().join(format!("med-projects-{}", std::process::id()));
        let file = dir.join("projects");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(read(&file).is_empty());
        remember(&file, Path::new("/a")).unwrap();
        remember(&file, Path::new("/b")).unwrap();
        remember(&file, Path::new("/a")).unwrap();
        assert_eq!(read(&file), [PathBuf::from("/a"), PathBuf::from("/b")]);
        remember(&file, Path::new("/odd\nname")).unwrap();
        assert_eq!(read(&file).len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
