//! The environment a project's programs run in: language servers and what
//! programs `spawn`.
//!
//! It is built in two layers. An app started from Finder, the Dock or a
//! desktop launcher gets the system's environment, not the one the user's
//! shell sets up, so the first layer is what a login shell started in the
//! project prints (only when med itself was not started from a terminal).
//! On top of that goes what direnv says the project's `.envrc` changes,
//! which is also how devbox (`devbox generate direnv`), nix flakes
//! (`use flake`) and mise reach the editor.

use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

/// How long a login shell gets to print its environment.
const SHELL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long direnv gets: evaluating a nix flake the first time can take
/// minutes.
const DIRENV_TIMEOUT: Duration = Duration::from_secs(300);

/// Printed around the environment a shell prints, so what its startup
/// files print is told apart from it. The shell is given only the word in
/// it: the whole mark must not turn up in the environment it prints.
const MARK_WORD: &str = "med-env-mark";
const MARK: &str = "\u{1}med-env-mark\u{1}";

/// A whole environment: every variable a program is started with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectEnv {
    vars: BTreeMap<OsString, OsString>,
}

impl ProjectEnv {
    /// The environment med itself was started with.
    pub fn inherited() -> Self {
        Self {
            vars: std::env::vars_os().collect(),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_vars<'a>(vars: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self {
            vars: vars
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    /// Every variable, as text: what is not UTF-8 is left out.
    pub fn vars(&self) -> impl Iterator<Item = (&str, &str)> {
        self.vars
            .iter()
            .filter_map(|(name, value)| Some((name.to_str()?, value.to_str()?)))
    }

    pub fn get(&self, name: &str) -> Option<&OsStr> {
        self.vars.get(OsStr::new(name)).map(OsString::as_os_str)
    }

    /// Where `program` is on this environment's `PATH`; a path with a
    /// slash in it is taken as it is.
    pub fn find_program(&self, program: &str) -> Option<PathBuf> {
        if program.contains('/') {
            let path = PathBuf::from(program);
            return is_executable(&path).then_some(path);
        }
        std::env::split_paths(self.get("PATH")?)
            .map(|dir| dir.join(program))
            .find(|path| is_executable(path))
    }

    /// A command that runs `program` with exactly this environment.
    pub fn command(&self, program: &str) -> Command {
        let mut command = Command::new(
            self.find_program(program)
                .map_or_else(|| OsString::from(program), PathBuf::into_os_string),
        );
        command.env_clear().envs(&self.vars);
        command
    }

    /// Apply what `direnv export json` printed: a value of null unsets.
    fn apply(&mut self, changes: BTreeMap<String, Option<String>>) {
        for (name, value) in changes {
            match value {
                Some(value) => self.vars.insert(name.into(), value.into()),
                None => self.vars.remove(OsStr::new(&name)),
            };
        }
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// A project's environment, and what went wrong working it out.
pub struct Loaded {
    pub env: ProjectEnv,
    pub problems: Vec<String>,
}

/// Work out the environment of the project at `root`. Blocks, for as long
/// as the user's shell and direnv take.
pub fn load(root: &Path) -> Loaded {
    let mut env = ProjectEnv::inherited();
    let mut problems = Vec::new();
    // Started from a terminal, med has the shell's environment already.
    // launchd can hand an app an empty TERM.
    if env.get("TERM").is_none_or(OsStr::is_empty) {
        match shell_env(root, &env) {
            Ok(Some(shell)) => env = shell,
            Ok(None) => {}
            Err(problem) => problems.push(problem),
        }
    }
    // What direnv's hook did in that shell (or in the one med was started
    // from) is undone first, where no .envrc is: otherwise direnv, seeing
    // the project's .envrc dealt with, says nothing, not even that it is
    // blocked.
    if env.get("DIRENV_DIR").is_some() {
        match direnv(Path::new("/"), &env) {
            Ok(changes) => env.apply(changes),
            Err(problem) => problems.push(problem),
        }
    }
    match direnv(root, &env) {
        Ok(changes) => env.apply(changes),
        Err(problem) => problems.push(problem),
    }
    Loaded { env, problems }
}

// ---- the login shell ---------------------------------------------------

/// What a login shell started in `root` has in its environment; `None`
/// for a shell this does not know how to ask.
fn shell_env(root: &Path, base: &ProjectEnv) -> Result<Option<ProjectEnv>, String> {
    let shell = base
        .get("SHELL")
        .filter(|shell| !shell.is_empty())
        .map_or_else(|| PathBuf::from("/bin/sh"), PathBuf::from);
    let name = shell
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut script = String::from("cd \"$MED_ENV_DIR\";");
    match name.trim_start_matches('-') {
        // Hooks such as direnv's, asdf's and mise's run at the prompt.
        "fish" => script.push_str("emit fish_prompt;"),
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "yash" => {}
        _ => return Ok(None),
    }
    script.push_str(
        "printf '\\001%s\\001' \"$MED_ENV_MARK\"; env -0; printf '\\001%s\\001' \"$MED_ENV_MARK\"",
    );
    let mut command = Command::new(&shell);
    command
        .args(["-l", "-i", "-c", &script])
        .env_clear()
        .envs(&base.vars)
        .env("MED_ENV_DIR", root)
        .env("MED_ENV_MARK", MARK_WORD)
        .current_dir(root);
    let output = run(command, SHELL_TIMEOUT).map_err(|e| format!("{}: {e}", shell.display()))?;
    let mut env = parse_shell_output(&output.stdout)
        .ok_or_else(|| format!("{} printed no environment", shell.display()))?;
    env.vars.remove(OsStr::new("MED_ENV_DIR"));
    env.vars.remove(OsStr::new("MED_ENV_MARK"));
    Ok(Some(env))
}

/// The variables `env -0` printed between the marks; what startup files
/// printed around them is left out.
fn parse_shell_output(output: &[u8]) -> Option<ProjectEnv> {
    let mark = MARK.as_bytes();
    let start = find(output, mark)? + mark.len();
    let end = start + find(&output[start..], mark)?;
    let vars = output[start..end]
        .split(|&byte| byte == 0)
        .filter_map(|entry| {
            let eq = entry.iter().position(|&byte| byte == b'=')?;
            // `=C:` style names have nothing before the `=`.
            (eq > 0).then(|| (os_string(&entry[..eq]), os_string(&entry[eq + 1..])))
        })
        .collect();
    Some(ProjectEnv { vars })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(unix)]
fn os_string(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes.to_vec())
}

#[cfg(not(unix))]
fn os_string(bytes: &[u8]) -> OsString {
    String::from_utf8_lossy(bytes).into_owned().into()
}

// ---- direnv -------------------------------------------------------------

/// What direnv changes in `base` for `root`; nothing when direnv is not
/// installed or no `.envrc` applies.
fn direnv(root: &Path, base: &ProjectEnv) -> Result<BTreeMap<String, Option<String>>, String> {
    let Some(direnv) = base.find_program("direnv") else {
        return Ok(BTreeMap::new());
    };
    let mut command = base.command(direnv.to_str().unwrap_or("direnv"));
    command
        .args(["export", "json"])
        .env("TERM", "dumb")
        .current_dir(root);
    let output = run(command, DIRENV_TIMEOUT).map_err(|e| format!("direnv: {e}"))?;
    // A blocked .envrc still prints the DIRENV_ variables: none of it counts.
    if !output.success {
        return Err(direnv_error(&output.stderr));
    }
    parse_direnv_output(&output.stdout)
}

fn parse_direnv_output(stdout: &[u8]) -> Result<BTreeMap<String, Option<String>>, String> {
    let text = String::from_utf8_lossy(stdout);
    // Nothing to change prints nothing at all.
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else {
        return Err("direnv printed something other than JSON".into());
    };
    Ok(map
        .into_iter()
        .map(|(name, value)| match value {
            Value::String(value) => (name, Some(value)),
            _ => (name, None),
        })
        .collect())
}

/// The line of direnv's stderr that says what went wrong, its colours
/// taken out.
fn direnv_error(stderr: &[u8]) -> String {
    let text = strip_ansi(&String::from_utf8_lossy(stderr));
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .rev()
        .find(|line| line.contains("error"))
        .or(lines.last())
        .map_or_else(|| "direnv failed".to_string(), |line| line.to_string())
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI: ESC [ parameters, then a letter.
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---- running ------------------------------------------------------------

struct Output {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Run `command` to the end, or kill it after `timeout`.
fn run(mut command: Command, timeout: Duration) -> Result<Output, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let (tx, rx) = mpsc::channel();
    for (index, pipe) in [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .enumerate()
    {
        let tx = tx.clone();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            let _ = tx.send((index, bytes));
        });
    }
    let deadline = Instant::now() + timeout;
    let mut read = [Vec::new(), Vec::new()];
    let timed_out = || format!("gave up after {} seconds", timeout.as_secs());
    for _ in 0..2 {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((index, bytes)) => read[index] = bytes,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(timed_out());
            }
        }
    }
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(timed_out());
            }
            Err(error) => return Err(error.to_string()),
        }
    };
    let [stdout, stderr] = read;
    Ok(Output {
        success: status.success(),
        stdout,
        stderr,
    })
}

// ---- sharing ------------------------------------------------------------

/// The environment of a workspace, for whoever needs it: empty while it is
/// being worked out, which a background thread can wait for.
#[derive(Default)]
pub struct EnvCell {
    env: Mutex<Option<Arc<ProjectEnv>>>,
    filled: Condvar,
}

impl EnvCell {
    pub fn ready(env: ProjectEnv) -> Arc<Self> {
        let cell = Self::default();
        *cell.env.lock().unwrap() = Some(Arc::new(env));
        Arc::new(cell)
    }

    pub fn get(&self) -> Option<Arc<ProjectEnv>> {
        self.env.lock().unwrap().clone()
    }

    /// The environment, once there is one. Not for the main thread.
    pub fn wait(&self) -> Arc<ProjectEnv> {
        let mut env = self.env.lock().unwrap();
        loop {
            if let Some(env) = &*env {
                return env.clone();
            }
            env = self.filled.wait(env).unwrap();
        }
    }

    pub fn set(&self, env: ProjectEnv) {
        *self.env.lock().unwrap() = Some(Arc::new(env));
        self.filled.notify_all();
    }

    /// Back to being worked out, as when it is loaded again.
    pub fn clear(&self) {
        *self.env.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &[(&str, &str)]) -> ProjectEnv {
        ProjectEnv {
            vars: vars.iter().map(|(k, v)| (k.into(), v.into())).collect(),
        }
    }

    #[test]
    fn shell_output_between_the_marks() {
        let mut out = b"Welcome!\n".to_vec();
        out.extend_from_slice(MARK.as_bytes());
        out.extend_from_slice(
            b"PATH=/a:/b\0MED_ENV_MARK=med-env-mark\0EMPTY=\0MULTI=one\ntwo\0=C:=x\0",
        );
        out.extend_from_slice(MARK.as_bytes());
        out.extend_from_slice(b"bye\n");
        let parsed = parse_shell_output(&out).unwrap();
        assert_eq!(
            parsed,
            env(&[
                ("PATH", "/a:/b"),
                ("MED_ENV_MARK", MARK_WORD),
                ("EMPTY", ""),
                ("MULTI", "one\ntwo")
            ])
        );
        assert!(parse_shell_output(b"no marks here").is_none());
    }

    #[test]
    fn direnv_null_unsets() {
        let changes =
            parse_direnv_output(br#"{"PATH": "/nix/bin:/usr/bin", "GONE": null}"#).unwrap();
        let mut base = env(&[("PATH", "/usr/bin"), ("GONE", "1"), ("KEPT", "2")]);
        base.apply(changes);
        assert_eq!(base, env(&[("PATH", "/nix/bin:/usr/bin"), ("KEPT", "2")]));
        assert!(parse_direnv_output(b"").unwrap().is_empty());
        assert!(parse_direnv_output(b"not json").is_err());
    }

    #[test]
    fn direnv_error_line() {
        let stderr = b"\x1b[0mdirenv: loading ~/p/.envrc\n\x1b[31mdirenv: error /p/.envrc is blocked. Run `direnv allow` to approve its content\x1b[0m\n";
        assert_eq!(
            direnv_error(stderr),
            "direnv: error /p/.envrc is blocked. Run `direnv allow` to approve its content"
        );
        assert_eq!(direnv_error(b""), "direnv failed");
    }

    #[cfg(unix)]
    #[test]
    fn programs_are_found_on_the_env_path() {
        let dir = std::env::temp_dir().join(format!("med-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("med-env-probe");
        std::fs::write(&program, "#!/bin/sh\necho \"$MED_PROBE\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();

        let env = env(&[
            ("PATH", &format!("{}:/usr/bin:/bin", dir.display())),
            ("MED_PROBE", "from the project"),
        ]);
        assert_eq!(env.find_program("med-env-probe"), Some(program.clone()));
        assert_eq!(env.find_program("surely-not-a-program-anywhere"), None);
        let out = env.command("med-env-probe").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "from the project\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_program_is_killed() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 5"]);
        let started = Instant::now();
        assert!(run(command, Duration::from_millis(200)).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn waiting_for_the_cell() {
        let cell = Arc::new(EnvCell::default());
        assert!(cell.get().is_none());
        let waiter = {
            let cell = cell.clone();
            thread::spawn(move || cell.wait())
        };
        cell.set(env(&[("A", "1")]));
        assert_eq!(*waiter.join().unwrap(), env(&[("A", "1")]));
    }
}
