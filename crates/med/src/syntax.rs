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
use tree_sitter::{InputEdit, Parser, Point, Query, QueryCursor, StreamingIterator, Tree};

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
        })
    }

    /// Reparse after `edits` (as returned by [`Buffer::take_edits`]).
    pub fn update(&mut self, buf: &Buffer, edits: &[Edit]) {
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
}
