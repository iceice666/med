//! Languages and tree-sitter highlighting.
//!
//! Grammars and highlight queries come from gpui-component's language
//! registry (plus a few of our own); parsing and querying run directly
//! against the editor's rope.

use gpui::component::highlighter::LanguageRegistry;
use med_core::{Buffer, Edit};
use ropey::Rope;
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use tree_sitter::{InputEdit, Node, Parser, Point, Query, QueryCursor, StreamingIterator, Tree};

pub struct LangInfo {
    /// Name in the grammar registry, also shown in the status bar.
    pub name: &'static str,
    /// LSP `languageId`.
    pub lsp_id: &'static str,
    pub extensions: &'static [&'static str],
    pub filenames: &'static [&'static str],
    pub line_comment: Option<&'static str>,
    pub tab_width: usize,
    pub expand_tab: bool,
}

const fn lang(
    name: &'static str,
    lsp_id: &'static str,
    extensions: &'static [&'static str],
    line_comment: Option<&'static str>,
    tab_width: usize,
) -> LangInfo {
    LangInfo {
        name,
        lsp_id,
        extensions,
        filenames: &[],
        line_comment,
        tab_width,
        expand_tab: true,
    }
}

static LANGS: &[LangInfo] = &[
    lang("rust", "rust", &["rs"], Some("//"), 4),
    LangInfo {
        filenames: &["Cargo.lock", "uv.lock"],
        ..lang("toml", "toml", &["toml"], Some("#"), 2)
    },
    lang("json", "json", &["json", "jsonc", "json5"], None, 2),
    lang("markdown", "markdown", &["md", "markdown", "mdx"], None, 2),
    lang("python", "python", &["py", "pyi"], Some("#"), 4),
    lang(
        "javascript",
        "javascript",
        &["js", "mjs", "cjs", "jsx"],
        Some("//"),
        2,
    ),
    lang(
        "typescript",
        "typescript",
        &["ts", "mts", "cts"],
        Some("//"),
        2,
    ),
    lang("tsx", "typescriptreact", &["tsx"], Some("//"), 2),
    LangInfo {
        expand_tab: false,
        ..lang("go", "go", &["go"], Some("//"), 4)
    },
    lang("c", "c", &["c", "h"], Some("//"), 4),
    lang(
        "cpp",
        "cpp",
        &["cpp", "cc", "cxx", "hpp", "hh", "hxx", "mm"],
        Some("//"),
        4,
    ),
    LangInfo {
        filenames: &[".bashrc", ".zshrc", ".profile", ".envrc"],
        ..lang("bash", "shellscript", &["sh", "bash", "zsh"], Some("#"), 2)
    },
    lang("html", "html", &["html", "htm"], None, 2),
    lang("css", "css", &["css", "scss"], None, 2),
    lang("yaml", "yaml", &["yaml", "yml"], Some("#"), 2),
    lang("lua", "lua", &["lua"], Some("--"), 2),
    lang("mlisp", "mlisp", &["mlisp"], Some(";"), 2),
    lang("zig", "zig", &["zig"], Some("//"), 4),
    LangInfo {
        filenames: &["flake.lock"],
        ..lang("nix", "nix", &["nix"], Some("#"), 2)
    },
    lang("swift", "swift", &["swift"], Some("//"), 4),
    lang("java", "java", &["java"], Some("//"), 4),
    lang("kotlin", "kotlin", &["kt", "kts"], Some("//"), 4),
    lang("ruby", "ruby", &["rb"], Some("#"), 2),
    lang("php", "php", &["php"], Some("//"), 4),
    lang("sql", "sql", &["sql"], Some("--"), 2),
    lang("csharp", "csharp", &["cs"], Some("//"), 4),
    lang("scala", "scala", &["scala", "sc"], Some("//"), 2),
    lang("elixir", "elixir", &["ex", "exs"], Some("#"), 2),
    lang("proto", "proto", &["proto"], Some("//"), 2),
    lang("graphql", "graphql", &["graphql", "gql"], Some("#"), 2),
    lang("svelte", "svelte", &["svelte"], None, 2),
    lang("astro", "astro", &["astro"], None, 2),
    lang("diff", "diff", &["diff", "patch"], None, 4),
    LangInfo {
        filenames: &["CMakeLists.txt"],
        ..lang("cmake", "cmake", &["cmake"], Some("#"), 2)
    },
    LangInfo {
        filenames: &["Makefile", "makefile", "GNUmakefile"],
        expand_tab: false,
        ..lang("make", "makefile", &["mk"], Some("#"), 4)
    },
];

/// One of med's own languages, by name.
pub fn lang_named(name: &str) -> Option<&'static LangInfo> {
    LANGS.iter().find(|l| l.name == name)
}

pub fn lang_for_path(path: &Path) -> Option<&'static LangInfo> {
    let file_name = path.file_name()?.to_str()?;
    if let Some(lang) = LANGS.iter().find(|l| l.filenames.contains(&file_name)) {
        return Some(lang);
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    LANGS.iter().find(|l| l.extensions.contains(&ext.as_str()))
}

struct Grammar {
    language: tree_sitter::Language,
    query: Query,
}

fn load_grammar(name: &str) -> Option<Grammar> {
    let (language, highlights) = if name == "nix" {
        (
            tree_sitter::Language::from(tree_sitter_nix::LANGUAGE),
            tree_sitter_nix::HIGHLIGHTS_QUERY.to_string(),
        )
    } else if name == "mlisp" {
        // med-lisp reads as Scheme; the query is ours, for its own forms.
        (
            tree_sitter::Language::from(tree_sitter_scheme::LANGUAGE),
            include_str!("mlisp-highlights.scm").to_string(),
        )
    } else {
        let config = LanguageRegistry::singleton().language(name)?;
        (config.language?, config.highlights.to_string())
    };
    let query = Query::new(&language, &highlights).ok()?;
    Some(Grammar { language, query })
}

/// Compiled grammars are shared between every buffer of a language.
fn grammar(name: &'static str) -> Option<Arc<Grammar>> {
    type Cache = Mutex<HashMap<&'static str, Option<Arc<Grammar>>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
    cache
        .entry(name)
        .or_insert_with(|| load_grammar(name).map(Arc::new))
        .clone()
}

struct RopeProvider<'a>(&'a Rope);

struct ChunkBytes<'a>(ropey::iter::Chunks<'a>);

impl<'a> Iterator for ChunkBytes<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(str::as_bytes)
    }
}

impl<'a> tree_sitter::TextProvider<&'a [u8]> for RopeProvider<'a> {
    type I = ChunkBytes<'a>;
    fn text(&mut self, node: tree_sitter::Node) -> Self::I {
        let len = self.0.len_bytes();
        let range = node.start_byte().min(len)..node.end_byte().min(len);
        ChunkBytes(self.0.byte_slice(range).chunks())
    }
}

fn parse(parser: &mut Parser, rope: &Rope, old: Option<&Tree>) -> Option<Tree> {
    let len = rope.len_bytes();
    parser.parse_with_options(
        &mut |byte: usize, _: Point| -> &[u8] {
            if byte >= len {
                return &[];
            }
            let (chunk, chunk_start, _, _) = rope.chunk_at_byte(byte);
            &chunk.as_bytes()[byte - chunk_start..]
        },
        old,
        None,
    )
}

/// One highlighted span: a byte range and the query capture that styles it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    pub range: Range<usize>,
    pub index: u32,
}

/// A buffer's syntax tree, kept in sync edit by edit.
pub struct Syntax {
    grammar: Arc<Grammar>,
    parser: Parser,
    tree: Option<Tree>,
    /// The buffer version the tree is of.
    version: u64,
    /// The tree was parsed anew since the last [`Syntax::update`], so the
    /// edits that one is given are no longer edits to this tree.
    reparsed: bool,
}

/// What a text object or a motion can ask the tree for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Function,
    Type,
    Argument,
    Comment,
}

impl Kind {
    fn of(letter: char) -> Option<Self> {
        Some(match letter {
            'f' => Kind::Function,
            't' => Kind::Type,
            'a' => Kind::Argument,
            'c' => Kind::Comment,
            _ => return None,
        })
    }
}

/// Node names the grammars give functions and methods, closures included.
const FUNCTIONS: &[&str] = &[
    "function_item",
    "function_definition",
    "function_declaration",
    "function_expression",
    "function",
    "generator_function",
    "generator_function_declaration",
    "arrow_function",
    "method_definition",
    "method_declaration",
    "method",
    "singleton_method",
    "constructor_declaration",
    "local_function_statement",
    "closure_expression",
    "lambda",
    "lambda_expression",
    "func_literal",
    "anonymous_function",
];

/// Node names for what defines a type or holds its methods.
const TYPES: &[&str] = &[
    "struct_item",
    "enum_item",
    "union_item",
    "trait_item",
    "impl_item",
    "class_definition",
    "class_declaration",
    "class",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "struct_declaration",
    "protocol_declaration",
    "object_declaration",
    "object_definition",
    "trait_definition",
    "type_declaration",
    "struct_specifier",
    "class_specifier",
    "enum_specifier",
    "union_specifier",
    "module",
];

fn is_function(node: Node) -> bool {
    node.is_named() && FUNCTIONS.contains(&node.kind())
}

fn is_type(node: Node) -> bool {
    node.is_named() && TYPES.contains(&node.kind())
}

fn is_comment(node: Node) -> bool {
    node.kind().contains("comment")
}

/// A list of arguments or of parameters: what is in it are the arguments.
fn is_argument_list(node: Node) -> bool {
    let kind = node.kind();
    ["arguments", "parameters", "argument_list", "parameter_list"]
        .iter()
        .any(|list| kind.ends_with(list))
}

/// The part of a function or a type that is its body.
fn body_of(node: Node) -> Option<Node> {
    if let Some(body) = node.child_by_field_name("body") {
        return Some(body);
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    children.into_iter().rev().find(|child| {
        let kind = child.kind();
        kind.contains("block") || kind.contains("body") || kind.ends_with("_list")
    })
}

impl Syntax {
    pub fn new(lang: &'static LangInfo, buf: &Buffer) -> Option<Self> {
        let grammar = grammar(lang.name)?;
        let mut parser = Parser::new();
        parser.set_language(&grammar.language).ok()?;
        let tree = parse(&mut parser, buf.rope(), None);
        Some(Self {
            grammar,
            parser,
            tree,
            version: buf.version(),
            reparsed: false,
        })
    }

    /// Have the tree be of the text as it is now. Between two
    /// [`Syntax::update`]s the buffer may have been edited (a macro, a
    /// repeated change): what asks about the text then gets a fresh parse.
    fn ensure_current(&mut self, buf: &Buffer) {
        if self.version != buf.version() {
            self.tree = parse(&mut self.parser, buf.rope(), None);
            self.version = buf.version();
            self.reparsed = true;
        }
    }

    /// Reparse after `edits` (as returned by [`Buffer::take_edits`]).
    pub fn update(&mut self, buf: &Buffer, edits: &[Edit]) {
        if std::mem::take(&mut self.reparsed) {
            self.ensure_current(buf);
            self.reparsed = false;
            return;
        }
        self.version = buf.version();
        if let Some(tree) = &mut self.tree {
            for edit in edits {
                let point = |p: med_core::Point| Point::new(p.row, p.col);
                tree.edit(&InputEdit {
                    start_byte: edit.start_byte,
                    old_end_byte: edit.old_end_byte,
                    new_end_byte: edit.new_end_byte,
                    start_position: point(edit.start_point),
                    old_end_position: point(edit.old_end_point),
                    new_end_position: point(edit.new_end_point),
                });
            }
        }
        self.tree = parse(&mut self.parser, buf.rope(), self.tree.as_ref());
    }

    /// Names of the query's captures, indexed by [`Capture::index`].
    pub fn capture_names(&self) -> &[&str] {
        self.grammar.query.capture_names()
    }

    /// Captures intersecting `range` (bytes), in paint order: a later capture
    /// overrides an earlier one where they overlap.
    pub fn captures(&self, buf: &Buffer, range: Range<usize>) -> Vec<Capture> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let query = &self.grammar.query;
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        let mut found: Vec<(Range<usize>, usize, u32)> = Vec::new();
        let mut matches = cursor.matches(query, tree.root_node(), RopeProvider(buf.rope()));
        while let Some(m) = matches.next() {
            for capture in m.captures {
                let r = capture.node.byte_range();
                if r.start < range.end && r.end > range.start && !r.is_empty() {
                    found.push((r, m.pattern_index, capture.index));
                }
            }
        }
        // Outer nodes first so inner ones paint over them; for the very same
        // node the earliest pattern in the query wins, so it goes last.
        found.sort_by(|a, b| {
            a.0.start
                .cmp(&b.0.start)
                .then(b.0.end.cmp(&a.0.end))
                .then(b.1.cmp(&a.1))
        });
        found
            .into_iter()
            .map(|(range, _, index)| Capture { range, index })
            .collect()
    }
}

// ---- what the vim engine asks ---------------------------------------------

impl Syntax {
    /// The smallest node of `kind` around the byte `at`. For an argument
    /// that is the one the byte is in, or else the nearest in its list.
    fn node_at(&self, at: usize, kind: Kind) -> Option<Node<'_>> {
        let root = self.tree.as_ref()?.root_node();
        let mut node = root.descendant_for_byte_range(at, at)?;
        loop {
            let found = match kind {
                Kind::Function => is_function(node),
                Kind::Type => is_type(node),
                Kind::Comment => is_comment(node),
                Kind::Argument => node.is_named() && node.parent().is_some_and(is_argument_list),
            };
            if found {
                return Some(node);
            }
            // On the brackets or a comma of the list itself.
            if kind == Kind::Argument && is_argument_list(node) {
                let mut cursor = node.walk();
                let arguments: Vec<Node> = node.named_children(&mut cursor).collect();
                let after = arguments.iter().find(|arg| arg.start_byte() >= at);
                if let Some(argument) = after.or(arguments.last()) {
                    return Some(*argument);
                }
            }
            node = node.parent()?;
        }
    }

    /// The bytes of the text object `kind` that `node` is.
    fn object_range(&self, node: Node, kind: Kind, around: bool) -> Range<usize> {
        match kind {
            Kind::Function | Kind::Type if !around => match body_of(node) {
                Some(body) => {
                    let range = body.byte_range();
                    // Between the braces, when the body has them.
                    let delimited =
                        body.child(0).is_some_and(|open| !open.is_named()) && range.len() >= 2;
                    if delimited {
                        range.start + 1..range.end - 1
                    } else {
                        range
                    }
                }
                None => node.byte_range(),
            },
            Kind::Function | Kind::Type => node.byte_range(),
            Kind::Argument if around => {
                // With the comma that sets it apart: the one after it, or
                // for the last argument the one before.
                match (node.next_named_sibling(), node.prev_named_sibling()) {
                    (Some(next), _) => node.start_byte()..next.start_byte(),
                    (None, Some(prev)) => prev.end_byte()..node.end_byte(),
                    (None, None) => node.byte_range(),
                }
            }
            Kind::Argument => node.byte_range(),
            Kind::Comment => {
                // Line comments on lines that follow each other are one.
                let (mut first, mut last) = (node, node);
                while let Some(prev) = first.prev_sibling()
                    && is_comment(prev)
                    && prev.end_position().row + 1 >= first.start_position().row
                {
                    first = prev;
                }
                while let Some(next) = last.next_sibling()
                    && is_comment(next)
                    && next.start_position().row <= last.end_position().row + 1
                {
                    last = next;
                }
                let end = last.end_byte();
                first.start_byte()..end
            }
        }
    }

    /// Where every node of `kind` starts, in bytes.
    fn starts(&self, kind: Kind) -> Vec<usize> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let mut starts = Vec::new();
        let mut cursor = tree.walk();
        loop {
            let node = cursor.node();
            let found = match kind {
                Kind::Function => is_function(node),
                Kind::Type => is_type(node),
                Kind::Comment => is_comment(node),
                Kind::Argument => node.is_named() && node.parent().is_some_and(is_argument_list),
            };
            if found {
                starts.push(node.start_byte());
            }
            if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    return starts;
                }
            }
        }
    }
}

impl med_vim::SyntaxTree for Syntax {
    fn object(
        &mut self,
        buf: &Buffer,
        pos: usize,
        kind: char,
        around: bool,
    ) -> Option<(usize, usize)> {
        let kind = Kind::of(kind)?;
        self.ensure_current(buf);
        let at = buf.char_to_byte(pos);
        let node = self.node_at(at, kind)?;
        let range = self.object_range(node, kind, around);
        let (mut start, mut end) = (buf.byte_to_char(range.start), buf.byte_to_char(range.end));
        // A comment node may take its line break with it.
        if kind == Kind::Comment {
            while end > start && buf.char_at(end - 1) == '\n' {
                end -= 1;
            }
        }
        // The inside of a body is what is between its braces, without the
        // blanks that only set it off from them.
        if !around && matches!(kind, Kind::Function | Kind::Type) {
            while start < end && matches!(buf.char_at(start), ' ' | '\t') {
                start += 1;
            }
            let own_lines = buf.char_at(start) == '\n' && {
                let last = buf.line_start(buf.line_of(end));
                (last..end).all(|at| matches!(buf.char_at(at), ' ' | '\t'))
            };
            if own_lines {
                start += 1;
                end = buf
                    .line_start(buf.line_of(end))
                    .saturating_sub(1)
                    .max(start);
                // Past the indentation of its first line.
                while start < end && matches!(buf.char_at(start), ' ' | '\t') {
                    start += 1;
                }
            } else {
                while end > start && matches!(buf.char_at(end - 1), ' ' | '\t') {
                    end -= 1;
                }
            }
        }
        (end > start).then_some((start, end))
    }

    fn enclosing(&mut self, buf: &Buffer, start: usize, end: usize) -> Option<(usize, usize)> {
        self.ensure_current(buf);
        let range = buf.char_to_byte(start)..buf.char_to_byte(end);
        let root = self.tree.as_ref()?.root_node();
        let mut node = root.named_descendant_for_byte_range(range.start, range.end)?;
        // The node that is exactly what is selected adds nothing.
        while node.byte_range() == range
            || node.start_byte() > range.start
            || node.end_byte() < range.end
        {
            node = node.parent()?;
        }
        let found = node.byte_range();
        Some((buf.byte_to_char(found.start), buf.byte_to_char(found.end)))
    }

    fn neighbor(&mut self, buf: &Buffer, pos: usize, kind: char, forward: bool) -> Option<usize> {
        let kind = Kind::of(kind)?;
        self.ensure_current(buf);
        let at = buf.char_to_byte(pos);
        let starts = self.starts(kind);
        let found = if forward {
            starts.into_iter().filter(|start| *start > at).min()
        } else {
            starts.into_iter().filter(|start| *start < at).max()
        };
        found.map(|byte| buf.byte_to_char(byte))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names_at(syntax: &Syntax, buf: &Buffer, needle: &str) -> Vec<String> {
        let text = buf.text();
        let start = text.find(needle).unwrap();
        let range = start..start + needle.len();
        syntax
            .captures(buf, 0..buf.len_bytes())
            .into_iter()
            .filter(|c| c.range == range)
            .map(|c| syntax.capture_names()[c.index as usize].to_string())
            .collect()
    }

    #[test]
    fn detects_languages() {
        assert_eq!(
            lang_for_path(Path::new("src/main.rs")).unwrap().name,
            "rust"
        );
        assert_eq!(lang_for_path(Path::new("Makefile")).unwrap().name, "make");
        assert_eq!(lang_for_path(Path::new("a/flake.nix")).unwrap().name, "nix");
        assert_eq!(lang_for_path(Path::new("x.HPP")).unwrap().name, "cpp");
        assert_eq!(
            lang_for_path(Path::new("init.mlisp")).unwrap().name,
            "mlisp"
        );
        assert!(lang_for_path(Path::new("notes.txt")).is_none());
    }

    #[test]
    fn every_language_has_a_working_grammar() {
        for lang in LANGS {
            assert!(
                Syntax::new(lang, &Buffer::new("x")).is_some(),
                "no grammar or bad highlight query for {}",
                lang.name
            );
        }
    }

    #[test]
    fn highlights_rust_and_follows_edits() {
        let lang = lang_for_path(Path::new("a.rs")).unwrap();
        let mut buf = Buffer::new("fn main() {\n    let x = \"hi\";\n}\n");
        let mut syntax = Syntax::new(lang, &buf).unwrap();
        assert_eq!(names_at(&syntax, &buf, "fn"), ["keyword"]);
        assert_eq!(names_at(&syntax, &buf, "main").last().unwrap(), "function");
        assert_eq!(names_at(&syntax, &buf, "\"hi\""), ["string"]);

        // Turn the string into a number; the tree must follow.
        let start = buf.text().find("\"hi\"").unwrap();
        buf.take_edits();
        buf.edit(start..start + 4, "42");
        let edits = buf.take_edits();
        syntax.update(&buf, &edits);
        let names = names_at(&syntax, &buf, "42");
        assert!(
            !names.is_empty() && !names.contains(&"string".to_string()),
            "{names:?}"
        );
        assert!(buf.text().contains("let x = 42;"));
    }

    /// What paints `needle`: the capture drawn last over it.
    fn painted(syntax: &Syntax, buf: &Buffer, needle: &str) -> String {
        names_at(syntax, buf, needle).pop().unwrap_or_default()
    }

    #[test]
    fn highlights_med_lisp() {
        let lang = lang_for_path(Path::new("init.mlisp")).unwrap();
        assert_eq!(lang.name, "mlisp");
        let buf = Buffer::new(
            "; c\n\
             (defn leader (chord cmd)\n\
             \x20 (bind 'normal (str \"<Space>\" chord) cmd :at 1))\n\
             (def lines nil)\n\
             (let ((count 0)) (when true (+ count 2)))\n",
        );
        let syntax = Syntax::new(lang, &buf).unwrap();
        for (needle, name) in [
            ("; c", "comment"),
            ("defn", "keyword"),
            ("leader", "function"),
            ("chord", "variable"),
            ("bind", "function"),
            ("'", "punctuation.special"),
            ("normal", "string.special.symbol"),
            ("\"<Space>\"", "string"),
            (":at", "string.special.symbol"),
            ("1", "number"),
            ("lines", "variable.special"),
            ("nil", "constant.builtin"),
            ("let", "keyword"),
            ("count", "variable"),
            ("when", "keyword"),
            ("true", "boolean"),
            ("+", "operator"),
            ("(", "punctuation.bracket"),
        ] {
            assert_eq!(painted(&syntax, &buf, needle), name, "{needle}");
        }
    }

    /// The Scheme grammar reads every example without an error.
    #[test]
    fn reads_the_examples_without_errors() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_scheme::LANGUAGE.into())
            .unwrap();
        let mut read = 0;
        for entry in std::fs::read_dir(examples).unwrap() {
            let path = entry.unwrap().path();
            if lang_for_path(&path).map(|l| l.name) != Some("mlisp") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let tree = parser.parse(&text, None).unwrap();
            assert!(!tree.root_node().has_error(), "{}", path.display());
            read += 1;
        }
        assert!(read > 0);
    }

    #[test]
    fn handles_multibyte_text() {
        let lang = lang_for_path(Path::new("a.rs")).unwrap();
        let mut buf = Buffer::new("// 註解\nlet s = \"字串\";\n");
        let mut syntax = Syntax::new(lang, &buf).unwrap();
        assert_eq!(names_at(&syntax, &buf, "// 註解"), ["comment"]);
        buf.take_edits();
        buf.insert(3, "好");
        let edits = buf.take_edits();
        syntax.update(&buf, &edits);
        assert_eq!(names_at(&syntax, &buf, "// 好註解"), ["comment"]);
        assert_eq!(names_at(&syntax, &buf, "\"字串\""), ["string"]);
    }

    const RUST: &str = "\
// one
// two
struct Point {
    x: i32,
}

fn add(left: i32, right: i32) -> i32 {
    let sum = left + right;
    sum
}
";

    fn rust() -> (Buffer, Syntax) {
        let lang = lang_for_path(Path::new("a.rs")).unwrap();
        let buf = Buffer::new(RUST);
        let syntax = Syntax::new(lang, &buf).unwrap();
        (buf, syntax)
    }

    /// The text object at the first `needle`.
    fn object(needle: &str, kind: char, around: bool) -> Option<String> {
        use med_vim::SyntaxTree as _;
        let (buf, mut syntax) = rust();
        let pos = buf.byte_to_char(RUST.find(needle).unwrap());
        let (start, end) = syntax.object(&buf, pos, kind, around)?;
        Some(buf.slice_string(start..end))
    }

    #[test]
    fn finds_text_objects_in_the_tree() {
        let function = &RUST[RUST.find("fn add").unwrap()..RUST.len() - 1];
        assert_eq!(object("sum =", 'f', true).as_deref(), Some(function));
        assert_eq!(
            object("sum =", 'f', false).as_deref(),
            Some("let sum = left + right;\n    sum")
        );
        assert_eq!(
            object("x: i32", 't', true).as_deref(),
            Some("struct Point {\n    x: i32,\n}")
        );
        assert_eq!(object("x: i32", 't', false).as_deref(), Some("x: i32,"));
        assert_eq!(
            object("left: i32", 'a', false).as_deref(),
            Some("left: i32")
        );
        assert_eq!(
            object("left: i32", 'a', true).as_deref(),
            Some("left: i32, ")
        );
        assert_eq!(
            object("right: i32", 'a', true).as_deref(),
            Some(", right: i32")
        );
        // On the bracket: the argument that comes next.
        assert_eq!(object("(left", 'a', false).as_deref(), Some("left: i32"));
        assert_eq!(object("one", 'c', true).as_deref(), Some("// one\n// two"));
        // Not in one at all.
        assert_eq!(object("x: i32", 'f', true), None);
        assert_eq!(object("x: i32", 'q', true), None);
    }

    #[test]
    fn finds_neighbors_and_what_encloses() {
        use med_vim::SyntaxTree as _;
        let (buf, mut syntax) = rust();
        let at = |needle: &str| buf.byte_to_char(RUST.find(needle).unwrap());
        assert_eq!(syntax.neighbor(&buf, 0, 'f', true), Some(at("fn add")));
        assert_eq!(
            syntax.neighbor(&buf, at("sum ="), 'f', false),
            Some(at("fn add"))
        );
        assert_eq!(syntax.neighbor(&buf, at("fn add"), 'f', true), None);
        assert_eq!(
            syntax.neighbor(&buf, at("fn add"), 't', false),
            Some(at("struct"))
        );
        assert_eq!(
            syntax.neighbor(&buf, at("left:"), 'a', true),
            Some(at("right:"))
        );
        assert_eq!(
            syntax.neighbor(&buf, at("struct"), 'c', false),
            Some(at("// two"))
        );

        // From a name out: the expression, the statement, the block…
        let mut range = (at("left +"), at("left +") + 1);
        let mut seen = Vec::new();
        while let Some(next) = syntax.enclosing(&buf, range.0, range.1) {
            assert!(next.0 <= range.0 && next.1 >= range.1 && next != range);
            seen.push(buf.slice_string(next.0..next.1));
            range = next;
        }
        assert_eq!(seen[0], "left");
        assert_eq!(seen[1], "left + right");
        assert_eq!(seen[2], "let sum = left + right;");
        assert!(seen.last().unwrap().starts_with("// one"));
    }

    #[test]
    fn answers_for_the_text_as_it_is() {
        use med_vim::SyntaxTree as _;
        let (mut buf, mut syntax) = rust();
        // Edited, and the tree not told yet.
        buf.take_edits();
        buf.insert(0, "fn first() {}\n");
        assert_eq!(
            syntax.neighbor(&buf, 0, 'f', true),
            Some(buf.text().find("fn add").unwrap())
        );
        assert_eq!(syntax.object(&buf, 3, 'f', true), Some((0, 13)));
        // Told afterwards, it is none the worse.
        let edits = buf.take_edits();
        syntax.update(&buf, &edits);
        assert_eq!(syntax.object(&buf, 3, 'f', true), Some((0, 13)));
        buf.insert(0, "\n");
        let edits = buf.take_edits();
        syntax.update(&buf, &edits);
        assert_eq!(syntax.object(&buf, 4, 'f', true), Some((1, 14)));
    }
}
