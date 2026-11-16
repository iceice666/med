//! Which language servers serve which languages, how each is started and
//! what it is told; and the languages med knows only by name, for their
//! servers.
//!
//! ```lisp
//! (lsp-server "rust-analyzer"                 ; a server med has: what is given changes
//!   :settings '(:rust-analyzer (:checkOnSave (:command "clippy"))))
//! (lsp-server "pylsp" :enabled false)         ; never started
//! (lsp-server "eslint"                        ; one of the user's own
//!   :command "vscode-eslint-language-server" :args '("--stdio")
//!   :languages '("javascript" "typescript" "tsx")
//!   :settings "{\"validate\": \"on\"}")      ; JSON, as a server's docs write it
//!
//! (deflanguage "dart" :extensions '("dart") :comment "//" :tab-width 2)
//! (lsp-server "dartls" :command "dart" :args '("language-server" "--protocol=lsp")
//!   :languages '("dart"))
//!
//! (lsp-servers)                               ; ((:name :command :args :languages :enabled)...)
//! ```
//!
//! Every server of a file's language that is found on the project's PATH
//! serves it, except that med's own servers for one language are
//! alternatives (pyright, or else pylsp): the first of those found. What
//! is asked about the file goes to the first server that can answer it;
//! diagnostics and code actions come from all of them.
//!
//! `:settings` is what the server reads with `workspace/configuration`,
//! and is sent with `workspace/didChangeConfiguration` once it has
//! started; `:init-options` goes with `initialize`. Both are written as
//! lists of keywords and values (`(:a 1 :b (:c true))` is
//! `{"a": 1, "b": {"c": true}}`), any other list being an array, or as a
//! JSON string. A server keeps what it was started with until the
//! servers are started again (`:workspace.reload_env`).

use crate::panel::{word, wrong};
use crate::syntax::{self, LangInfo};
use gpui::{App, Global};
use med_lisp::{Arity, Error, ErrorKind, Interp, Value};
use serde_json::{Map, Value as Json};
use std::path::Path;
use std::sync::Arc;

/// How one language server is started, for which languages, and what it is
/// told.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerConfig {
    /// What it is known by: in the status bar, and to `lsp-server`.
    pub name: Arc<str>,
    pub command: String,
    pub args: Vec<String>,
    /// The languages it serves, by the names `buffer-language` gives.
    pub languages: Vec<String>,
    /// Servers of one group are alternatives: the first one found serves
    /// the file. A server the user adds is a group by itself.
    pub group: Arc<str>,
    pub enabled: bool,
    /// What `workspace/configuration` is answered from.
    pub settings: Json,
    /// `initializationOptions`.
    pub init_options: Option<Json>,
}

impl ServerConfig {
    fn new(name: &str, command: &str, args: &[&str], languages: &[&str], group: &str) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            languages: languages.iter().map(|l| l.to_string()).collect(),
            group: group.into(),
            enabled: true,
            settings: Json::Null,
            init_options: None,
        }
    }
}

/// The servers med knows of itself, in order of preference.
fn builtin_servers() -> Vec<ServerConfig> {
    let server = |program: &str, args: &[&str], languages: &[&str]| {
        ServerConfig::new(program, program, args, languages, program)
    };
    // Named so that no server a program adds is in the group.
    let alternative = |program: &str, args: &[&str], language: &str| {
        let group = format!("builtin:{language}");
        ServerConfig::new(program, program, args, &[language], &group)
    };
    vec![
        server("rust-analyzer", &[], &["rust"]),
        server("clangd", &[], &["c", "cpp"]),
        alternative("basedpyright-langserver", &["--stdio"], "python"),
        alternative("pyright-langserver", &["--stdio"], "python"),
        alternative("pylsp", &[], "python"),
        server(
            "typescript-language-server",
            &["--stdio"],
            &["javascript", "typescript", "tsx"],
        ),
        server("gopls", &[], &["go"]),
        alternative("nixd", &[], "nix"),
        alternative("nil", &[], "nix"),
        server("lua-language-server", &[], &["lua"]),
        server("zls", &[], &["zig"]),
        server("sourcekit-lsp", &[], &["swift"]),
        server("taplo", &["lsp", "stdio"], &["toml"]),
        server("vscode-json-language-server", &["--stdio"], &["json"]),
        server("vscode-html-language-server", &["--stdio"], &["html"]),
        server("vscode-css-language-server", &["--stdio"], &["css"]),
        server("yaml-language-server", &["--stdio"], &["yaml"]),
        server("bash-language-server", &["start"], &["bash"]),
    ]
}

/// The servers and the languages programs have set up.
#[derive(Clone)]
pub struct LspConfig {
    servers: Vec<ServerConfig>,
    /// Languages a program named, looked at before med's own.
    languages: Vec<&'static LangInfo>,
}

impl Global for LspConfig {}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            servers: builtin_servers(),
            languages: Vec::new(),
        }
    }
}

impl LspConfig {
    /// The servers that may serve `language`, in order.
    pub fn servers_for(&self, language: &str) -> Vec<ServerConfig> {
        self.servers
            .iter()
            .filter(|s| s.enabled && s.languages.iter().any(|l| l == language))
            .cloned()
            .collect()
    }

    /// The language of a file: one a program named, or one of med's.
    pub fn lang_for_path(&self, path: &Path) -> Option<&'static LangInfo> {
        let file_name = path.file_name()?.to_str()?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        self.languages
            .iter()
            .find(|l| l.filenames.contains(&file_name))
            .or_else(|| {
                let ext = ext.as_deref()?;
                self.languages.iter().find(|l| l.extensions.contains(&ext))
            })
            .copied()
            .or_else(|| syntax::lang_for_path(path))
    }

    /// Change a server, or add one. A new one needs a command and
    /// languages.
    fn set_server(&mut self, name: &str, change: ServerChange) -> Result<(), String> {
        if let Some(server) = self.servers.iter_mut().find(|s| &*s.name == name) {
            change.apply(server);
            return Ok(());
        }
        let mut server = ServerConfig::new(name, "", &[], &[], name);
        change.apply(&mut server);
        if server.command.is_empty() {
            return Err(format!(
                "lsp-server: {name} is a new server, and needs a :command"
            ));
        }
        if server.languages.is_empty() {
            return Err(format!(
                "lsp-server: {name} is a new server, and needs :languages"
            ));
        }
        self.servers.push(server);
        Ok(())
    }

    fn set_language(&mut self, lang: LangInfo) {
        self.languages.retain(|l| l.name != lang.name);
        self.languages.insert(0, Box::leak(Box::new(lang)));
    }
}

/// The servers that may serve `language`, as the programs have set them.
pub fn servers_for(language: &str, cx: &App) -> Vec<ServerConfig> {
    match cx.try_global::<LspConfig>() {
        Some(config) => config.servers_for(language),
        None => LspConfig::default().servers_for(language),
    }
}

/// The language of a file, as the programs have set them up.
pub fn lang_for_path(path: &Path, cx: &App) -> Option<&'static LangInfo> {
    match cx.try_global::<LspConfig>() {
        Some(config) => config.lang_for_path(path),
        None => syntax::lang_for_path(path),
    }
}

/// Forget what programs set up, as before the init file is run again.
pub fn reset(cx: &mut App) {
    cx.set_global(LspConfig::default());
}

/// What a server asked for with `workspace/configuration`: the part of its
/// settings at `section` (`"python.analysis"`), or all of them.
pub fn section(settings: &Json, section: Option<&str>) -> Json {
    let Some(section) = section.filter(|s| !s.is_empty()) else {
        return settings.clone();
    };
    section
        .split('.')
        .try_fold(settings, |at, key| at.get(key))
        .cloned()
        .unwrap_or(Json::Null)
}

/// What `lsp-server` was told to change.
#[derive(Default)]
struct ServerChange {
    command: Option<String>,
    args: Option<Vec<String>>,
    languages: Option<Vec<String>>,
    enabled: Option<bool>,
    settings: Option<Json>,
    init_options: Option<Option<Json>>,
}

impl ServerChange {
    fn apply(self, server: &mut ServerConfig) {
        if let Some(command) = self.command {
            server.command = command;
        }
        if let Some(args) = self.args {
            server.args = args;
        }
        if let Some(languages) = self.languages {
            server.languages = languages;
        }
        if let Some(enabled) = self.enabled {
            server.enabled = enabled;
        }
        if let Some(settings) = self.settings {
            server.settings = settings;
        }
        if let Some(init_options) = self.init_options {
            server.init_options = init_options;
        }
    }
}

fn failed(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Raised, message)
}

/// A value as JSON: a list of keywords and values is an object, any other
/// list an array; nil is null, and a symbol or keyword its name.
pub fn to_json(value: &Value) -> Result<Json, Error> {
    Ok(match value {
        Value::Nil => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(n) => Json::from(*n),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(Json::Number)
            .ok_or_else(|| failed(format!("{f} is no number JSON can hold")))?,
        Value::Str(s) => Json::String(s.to_string()),
        Value::Symbol(s) | Value::Keyword(s) => Json::String(s.name().to_string()),
        Value::Pair(_) => {
            let items = value.to_vec()?;
            let is_object = items.len().is_multiple_of(2)
                && items
                    .iter()
                    .step_by(2)
                    .all(|k| matches!(k, Value::Keyword(_)));
            if is_object {
                let mut object = Map::new();
                for pair in items.chunks(2) {
                    let Value::Keyword(key) = &pair[0] else {
                        unreachable!("every other item is a keyword");
                    };
                    object.insert(key.name().to_string(), to_json(&pair[1])?);
                }
                Json::Object(object)
            } else {
                Json::Array(items.iter().map(to_json).collect::<Result<_, _>>()?)
            }
        }
        other => return Err(failed(format!("{other} cannot be sent to a server"))),
    })
}

/// Settings or init options: an object, as a list or as a JSON string.
fn json_object(option: &str, value: &Value) -> Result<Json, Error> {
    let json = match value {
        Value::Str(text) => serde_json::from_str(text)
            .map_err(|e| failed(format!("lsp-server: {option} is no JSON: {e}")))?,
        other => to_json(other)?,
    };
    match json {
        Json::Object(_) | Json::Null => Ok(json),
        other => Err(failed(format!(
            "lsp-server: {option} is an object, as (:name value ...) or \"{{...}}\", not {other}"
        ))),
    }
}

fn strings(option: &str, value: &Value) -> Result<Vec<String>, Error> {
    value
        .to_vec()?
        .iter()
        .map(|item| word(item).ok_or_else(|| wrong(option, "a list of strings", value)))
        .collect()
}

fn config(cx: &mut App) -> &mut LspConfig {
    if !cx.has_global::<LspConfig>() {
        cx.set_global(LspConfig::default());
    }
    cx.global_mut::<LspConfig>()
}

fn options<'a>(
    function: &str,
    args: &'a [Value],
) -> Result<impl Iterator<Item = (String, &'a Value)>, Error> {
    if !args.len().is_multiple_of(2) {
        return Err(failed(format!(
            "{function}: expected options and their values, got {}",
            args[args.len() - 1]
        )));
    }
    let mut out = Vec::new();
    for pair in args.chunks(2) {
        let Value::Keyword(key) = &pair[0] else {
            return Err(failed(format!(
                "{function}: expected an option and its value, got {}",
                pair[0]
            )));
        };
        out.push((key.name().to_string(), &pair[1]));
    }
    Ok(out.into_iter())
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn leak_all(list: Vec<String>) -> &'static [&'static str] {
    Box::leak(
        list.into_iter()
            .map(leak)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    )
}

pub fn define_functions(interp: &mut Interp) {
    // (lsp-server name :command c :args (...) :languages (...) :enabled b
    //   :settings s :init-options o): change one of the servers, or add one.
    interp.define_native("lsp-server", Arity::at_least(1), |call, args| {
        let name = word(&args[0])
            .filter(|n| !n.is_empty())
            .ok_or_else(|| wrong("lsp-server", "a name", &args[0]))?;
        let mut change = ServerChange::default();
        for (option, value) in options("lsp-server", &args[1..])? {
            match option.as_str() {
                "command" => {
                    change.command =
                        Some(word(value).ok_or_else(|| wrong("lsp-server", "a command", value))?)
                }
                "args" => change.args = Some(strings(":args", value)?),
                "languages" => change.languages = Some(strings(":languages", value)?),
                "enabled" => change.enabled = Some(value.is_truthy()),
                "settings" => change.settings = Some(json_object(":settings", value)?),
                "init-options" => {
                    change.init_options = Some(match json_object(":init-options", value)? {
                        Json::Null => None,
                        json => Some(json),
                    })
                }
                other => {
                    return Err(failed(format!(
                        "lsp-server: {other} is not an option \
                         (command args languages enabled settings init-options)"
                    )));
                }
            }
        }
        let cx = crate::script::app(call)?;
        config(cx).set_server(&name, change).map_err(failed)?;
        Ok(Value::Nil)
    });

    // (lsp-servers): every server, in order.
    interp.define_native("lsp-servers", 0, |call, _| {
        let cx = crate::script::app(call)?;
        let servers = config(cx).servers.iter().map(|s| {
            let strings =
                |list: &[String]| Value::list(list.iter().map(|s| Value::from(s.as_str())));
            crate::host::record([
                ("name", Value::from(&*s.name)),
                ("command", Value::from(s.command.as_str())),
                ("args", strings(&s.args)),
                ("languages", strings(&s.languages)),
                ("enabled", Value::Bool(s.enabled)),
            ])
        });
        Ok(Value::list(servers.collect::<Vec<_>>()))
    });

    // (deflanguage name :extensions (...) :filenames (...) :lsp-id id
    //   :comment c :tab-width n :expand-tab b): a language for files med
    // does not know, or more files for one it does.
    interp.define_native("deflanguage", Arity::at_least(1), |call, args| {
        let name = word(&args[0])
            .filter(|n| !n.is_empty())
            .ok_or_else(|| wrong("deflanguage", "a name", &args[0]))?;
        let known = syntax::lang_named(&name);
        let mut extensions = known.map_or_else(Vec::new, |l| {
            l.extensions.iter().map(|e| e.to_string()).collect()
        });
        let mut filenames = known.map_or_else(Vec::new, |l| {
            l.filenames.iter().map(|e| e.to_string()).collect()
        });
        let mut lsp_id = known.map_or_else(|| name.clone(), |l| l.lsp_id.to_string());
        let mut comment = known.and_then(|l| l.line_comment).map(str::to_string);
        let mut tab_width = known.map_or(4, |l| l.tab_width);
        let mut expand_tab = known.is_none_or(|l| l.expand_tab);
        for (option, value) in options("deflanguage", &args[1..])? {
            match option.as_str() {
                "extensions" => {
                    extensions = strings(":extensions", value)?
                        .into_iter()
                        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
                        .collect()
                }
                "filenames" => filenames = strings(":filenames", value)?,
                "lsp-id" => {
                    lsp_id = word(value).ok_or_else(|| wrong("deflanguage", "an id", value))?
                }
                "comment" => {
                    comment = match value {
                        Value::Nil => None,
                        v => Some(word(v).ok_or_else(|| wrong("deflanguage", "a comment", v))?),
                    }
                }
                "tab-width" => {
                    tab_width = usize::try_from(value.as_int()?)
                        .ok()
                        .filter(|w| (1..=16).contains(w))
                        .ok_or_else(|| wrong("deflanguage", "a tab width from 1 to 16", value))?
                }
                "expand-tab" => expand_tab = value.is_truthy(),
                other => {
                    return Err(failed(format!(
                        "deflanguage: {other} is not an option \
                         (extensions filenames lsp-id comment tab-width expand-tab)"
                    )));
                }
            }
        }
        let lang = LangInfo {
            name: leak(name),
            lsp_id: leak(lsp_id),
            extensions: leak_all(extensions),
            filenames: leak_all(filenames),
            line_comment: comment.map(leak),
            tab_width,
            expand_tab,
        };
        let cx = crate::script::app(call)?;
        config(cx).set_language(lang);
        Ok(Value::Nil)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read(source: &str) -> Value {
        let mut interp = Interp::new();
        interp.eval_str(source, &mut ()).unwrap()
    }

    #[test]
    fn lists_of_keywords_are_objects() {
        let value = read(
            "'(:rust-analyzer (:checkOnSave (:command \"clippy\") :features (\"a\" \"b\")) \
               :n 2 :f 1.5 :on true :off false :none nil :kind full)",
        );
        assert_eq!(
            to_json(&value).unwrap(),
            json!({
                "rust-analyzer": { "checkOnSave": { "command": "clippy" }, "features": ["a", "b"] },
                "n": 2, "f": 1.5, "on": true, "off": false, "none": null, "kind": "full",
            })
        );
        // Strings in the keys' places make an array, not an object.
        assert_eq!(
            to_json(&read("'(\"--stdio\" \"x\")")).unwrap(),
            json!(["--stdio", "x"])
        );
        assert_eq!(to_json(&read("'(:a)")).unwrap(), json!(["a"]));
        assert_eq!(
            json_object(":settings", &read("\"{\\\"validate\\\": \\\"on\\\"}\"")).unwrap(),
            json!({ "validate": "on" })
        );
        assert!(json_object(":settings", &read("'(1 2)")).is_err());
    }

    #[test]
    fn sections_of_the_settings() {
        let settings = json!({ "python": { "analysis": { "typeCheckingMode": "strict" } } });
        assert_eq!(
            section(&settings, Some("python.analysis")),
            json!({ "typeCheckingMode": "strict" })
        );
        assert_eq!(section(&settings, Some("python.nothing")), Json::Null);
        assert_eq!(section(&settings, None), settings);
        assert_eq!(section(&settings, Some("")), settings);
    }

    #[test]
    fn changing_a_server_keeps_the_rest() {
        let mut config = LspConfig::default();
        let change = ServerChange {
            settings: Some(json!({ "rust-analyzer": { "cargo": { "features": "all" } } })),
            ..Default::default()
        };
        config.set_server("rust-analyzer", change).unwrap();
        let rust = config.servers_for("rust");
        assert_eq!(rust.len(), 1);
        assert_eq!(rust[0].command, "rust-analyzer");
        assert_eq!(rust[0].languages, ["rust"]);
        assert_eq!(
            rust[0].settings["rust-analyzer"]["cargo"]["features"],
            "all"
        );

        let off = ServerChange {
            enabled: Some(false),
            ..Default::default()
        };
        config.set_server("pylsp", off).unwrap();
        let python: Vec<_> = config
            .servers_for("python")
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(
            python,
            [
                Arc::from("basedpyright-langserver"),
                Arc::from("pyright-langserver")
            ]
        );

        // A new one is a group by itself, and needs a command and languages.
        assert!(
            config
                .set_server("eslint", ServerChange::default())
                .is_err()
        );
        let eslint = ServerChange {
            command: Some("vscode-eslint-language-server".into()),
            languages: Some(vec!["typescript".into()]),
            ..Default::default()
        };
        config.set_server("eslint", eslint).unwrap();
        let ts: Vec<_> = config
            .servers_for("typescript")
            .into_iter()
            .map(|s| (s.name, s.group))
            .collect();
        assert_eq!(
            ts,
            [
                (
                    Arc::from("typescript-language-server"),
                    Arc::from("typescript-language-server")
                ),
                (Arc::from("eslint"), Arc::from("eslint")),
            ]
        );
    }

    #[test]
    fn languages_a_program_names_come_first() {
        let mut config = LspConfig::default();
        assert!(config.lang_for_path(Path::new("/p/main.dart")).is_none());
        config.set_language(LangInfo {
            name: "dart",
            lsp_id: "dart",
            extensions: &["dart"],
            filenames: &[],
            line_comment: Some("//"),
            tab_width: 2,
            expand_tab: true,
        });
        assert_eq!(
            config
                .lang_for_path(Path::new("/p/main.dart"))
                .map(|l| l.name),
            Some("dart")
        );
        // med's own are still there.
        assert_eq!(
            config
                .lang_for_path(Path::new("/p/main.rs"))
                .map(|l| l.name),
            Some("rust")
        );
    }
}
