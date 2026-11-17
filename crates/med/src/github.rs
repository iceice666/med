//! Files that `load` takes from GitHub, named `gh:owner/repo/path@ref`.
//!
//! A file is downloaded once, with `curl`, and kept in
//! [`crate::Settings::github_dir`]; it is run from there after that, with
//! no network, until it is asked for again with `:fetch`. With no `@ref`
//! the repository's default branch is used.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One file of one repository, at one ref.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitHubFile {
    owner: String,
    repo: String,
    /// `None` for the default branch.
    git_ref: Option<String>,
    /// From the top of the repository, `/` between the parts, with no `.`
    /// or `..` left.
    path: String,
}

impl GitHubFile {
    /// What follows `gh:`: `owner/repo/path`, then `@ref` if any.
    pub(crate) fn parse(spec: &str) -> Result<Self, String> {
        let bad = |why: &str| format!("gh:{spec}: {why}");
        let (rest, git_ref) = match spec.rsplit_once('@') {
            Some((rest, git_ref)) => (rest, Some(git_ref)),
            None => (spec, None),
        };
        let mut parts = rest.splitn(3, '/');
        let (Some(owner), Some(repo), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(bad(
                "expected gh:owner/repo/path, or gh:owner/repo/path@ref",
            ));
        };
        for name in [owner, repo] {
            let fits = |c: char| c.is_ascii_alphanumeric() || "-_.".contains(c);
            if name.is_empty() || name == "." || name == ".." || !name.chars().all(fits) {
                return Err(bad(&format!("{name:?} is not a GitHub name")));
            }
        }
        let git_ref = match git_ref {
            Some(git_ref) => {
                let parts = git_ref.split('/');
                if git_ref.is_empty()
                    || !fits_url(git_ref)
                    || parts
                        .into_iter()
                        .any(|part| matches!(part, "" | "." | ".."))
                {
                    return Err(bad(&format!("{git_ref:?} is not a ref")));
                }
                Some(git_ref.to_string())
            }
            None => None,
        };
        let path = normalize("", path).map_err(|why| bad(&why))?;
        Ok(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            git_ref,
            path,
        })
    }

    /// The file `given` names from beside this one: another file of the
    /// same repository, at the same ref.
    pub(crate) fn beside(&self, given: &str) -> Result<Self, String> {
        let dir = self.path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let path = normalize(dir, given).map_err(|why| format!("{self}: {given}: {why}"))?;
        Ok(Self {
            path,
            ..self.clone()
        })
    }

    fn url(&self) -> String {
        let git_ref = self.git_ref.as_deref().unwrap_or("HEAD");
        let Self {
            owner, repo, path, ..
        } = self;
        format!("https://raw.githubusercontent.com/{owner}/{repo}/{git_ref}/{path}")
    }

    /// Where the file is kept in `dir`: by owner, repository and ref, the
    /// ref in one part (`feature%2Fx`), `HEAD` for the default branch.
    pub(crate) fn cache_path(&self, dir: &Path) -> PathBuf {
        let git_ref = match &self.git_ref {
            Some(git_ref) => git_ref.replace('%', "%25").replace('/', "%2F"),
            None => "HEAD".to_string(),
        };
        let mut path = dir.join(&self.owner).join(&self.repo).join(git_ref);
        path.extend(self.path.split('/'));
        path
    }

    /// Download the file to `to`. A download that fails leaves what was
    /// there before.
    pub(crate) fn fetch(&self, to: &Path) -> Result<(), String> {
        let failed = |why: String| format!("{self}: {why}");
        let dir = to.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)
            .map_err(|error| failed(format!("{}: {error}", dir.display())))?;
        let name = to.file_name().unwrap_or_default().to_string_lossy();
        let part = dir.join(format!(".{name}.part"));
        let url = self.url();
        let output = Command::new("curl")
            // Fail on 404 rather than keep the error page; no `[]` globs.
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--globoff",
            ])
            .args([
                "--proto",
                "=https",
                "--connect-timeout",
                "10",
                "--max-time",
                "60",
            ])
            .arg("--output")
            .arg(&part)
            .arg(&url)
            .output()
            .map_err(|error| failed(format!("could not run curl: {error}")))?;
        if !output.status.success() {
            let _ = std::fs::remove_file(&part);
            let said = String::from_utf8_lossy(&output.stderr);
            return Err(failed(format!("could not download {url}: {}", said.trim())));
        }
        std::fs::rename(&part, to).map_err(|error| {
            let _ = std::fs::remove_file(&part);
            failed(format!("{}: {error}", to.display()))
        })
    }
}

impl fmt::Display for GitHubFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "gh:{}/{}/{}", self.owner, self.repo, self.path)?;
        match &self.git_ref {
            Some(git_ref) => write!(f, "@{git_ref}"),
            None => Ok(()),
        }
    }
}

/// What can go in a URL as it is.
fn fits_url(text: &str) -> bool {
    !text
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || "?#%\\".contains(c))
}

/// `path` from `dir`, both from the top of a repository, with `.` and
/// `..` worked out. Going above the top is an error.
fn normalize(dir: &str, path: &str) -> Result<String, String> {
    if !fits_url(path) {
        return Err(format!("{path:?} is not a path"));
    }
    let start = if path.starts_with('/') { "" } else { dir };
    let mut parts: Vec<&str> = Vec::new();
    for part in start.split('/').chain(path.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts
                    .pop()
                    .ok_or_else(|| format!("{path} is outside the repository"))?;
            }
            part => parts.push(part),
        }
    }
    if parts.is_empty() || path.ends_with('/') {
        return Err(format!("{path:?} is not a file"));
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(spec: &str) -> GitHubFile {
        GitHubFile::parse(spec).unwrap()
    }

    #[test]
    fn names_are_read() {
        let file = parse("me/conf/lib/tree.mlisp@v1.2");
        assert_eq!(file.to_string(), "gh:me/conf/lib/tree.mlisp@v1.2");
        assert_eq!(
            file.url(),
            "https://raw.githubusercontent.com/me/conf/v1.2/lib/tree.mlisp"
        );
        assert_eq!(
            file.cache_path(Path::new("/c")),
            Path::new("/c/me/conf/v1.2/lib/tree.mlisp")
        );

        // The default branch, and a ref with a slash in it.
        let file = parse("me/conf/./a/../init.mlisp");
        assert_eq!(file.to_string(), "gh:me/conf/init.mlisp");
        assert_eq!(
            file.url(),
            "https://raw.githubusercontent.com/me/conf/HEAD/init.mlisp"
        );
        assert_eq!(
            parse("me/conf/x.mlisp@feature/x").cache_path(Path::new("/c")),
            Path::new("/c/me/conf/feature%2Fx/x.mlisp")
        );
    }

    #[test]
    fn bad_names_are_not() {
        for spec in [
            "me/conf",
            "me//x.mlisp",
            "../conf/x.mlisp",
            "me/conf/",
            "me/conf/../../x.mlisp",
            "me/conf/a b.mlisp",
            "me/conf/x.mlisp?raw",
            "me/conf/x.mlisp@",
            "me/conf/x.mlisp@../../other",
            "me/conf/x.mlisp@a b",
        ] {
            assert!(GitHubFile::parse(spec).is_err(), "{spec}");
        }
    }

    #[test]
    fn files_beside_are_in_the_same_repository() {
        let file = parse("me/conf/lib/tree.mlisp@main");
        let beside = |given: &str| file.beside(given).map(|file| file.to_string());
        assert_eq!(
            beside("more.mlisp").unwrap(),
            "gh:me/conf/lib/more.mlisp@main"
        );
        assert_eq!(
            beside("../init.mlisp").unwrap(),
            "gh:me/conf/init.mlisp@main"
        );
        assert_eq!(beside("/top.mlisp").unwrap(), "gh:me/conf/top.mlisp@main");
        assert!(beside("../../x.mlisp").is_err());
    }
}
