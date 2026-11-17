//! Structural search and replace: code found by its shape, as IntelliJ
//! IDEA finds it, and not by its text.
//!
//! A pattern is code of the language with `$name$` where anything may be:
//! `$a$.unwrap()` is every call of `unwrap`, whatever it is called on.
//! `$name*$` is any number of things side by side, none too, such as the
//! arguments of a call, and `$_$` is anything, not kept. A name used twice
//! stands for the same text both times. Spaces, line breaks, comments and
//! the commas between things do not count. A `$name$` stands for a whole
//! part of the code, never for a piece of a word or of a string.
//!
//! A replacement is text with the pattern's `$name$`s in it, each standing
//! for what was found there. Its lines after the first are indented as the
//! line the match starts on.
//!
//! Programs reach this through `structural-search` and
//! `structural-replace` (see `host.rs`).

use crate::search;
use crate::syntax::{self, LangInfo};
use crate::workspace::Workspace;
use gpui::{Context, Window};
use lsp_types as lsp;
use regex::Regex;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use tree_sitter::{Node, Parser, Tree};

/// A `$name$` or a `$name*$`, in a pattern or a replacement.
fn variables() -> &'static Regex {
    static VARIABLES: OnceLock<Regex> = OnceLock::new();
    VARIABLES.get_or_init(|| Regex::new(r"\$([A-Za-z_][A-Za-z0-9_]*)(\*?)\$").unwrap())
}

/// Code around a pattern that makes it something the grammar reads, as
/// an expression is only in a function in Rust: tried in order, `{}`
/// being the pattern.
fn wrappers(lang: &str) -> &'static [&'static str] {
    match lang {
        "rust" => &[
            "{}",
            "fn __med() {\n{}\n}",
            "fn __med() {\n{};\n}",
            "type __Med = {};",
            "impl __Med {\n{}\n}",
        ],
        "go" => &[
            "package __med\n{}",
            "package __med\nfunc __med() {\n{}\n}",
            "package __med\nvar __med = {}",
        ],
        "c" | "cpp" => &["{}", "void __med() {\n{}\n}", "void __med() {\n{};\n}"],
        "java" | "csharp" => &[
            "{}",
            "class __Med {\n{}\n}",
            "class __Med {\nvoid __med() {\n{}\n}\n}",
            "class __Med {\nvoid __med() {\n{};\n}\n}",
        ],
        "javascript" | "typescript" | "tsx" => {
            &["{}", "class __Med {\n{}\n}", "function __med() {\n{}\n}"]
        }
        "php" => &["<?php\n{}", "<?php\n{};", "<?php\nclass __Med {\n{}\n}"],
        "kotlin" => &["{}", "class __Med {\n{}\n}", "fun __med() {\n{}\n}"],
        "swift" => &["{}", "class __Med {\n{}\n}", "func __med() {\n{}\n}"],
        "scala" => &["{}", "object __Med {\n{}\n}"],
        _ => &["{}", "{};"],
    }
}

/// A variable of a pattern, by the name it was written with.
struct Variable {
    name: String,
    many: bool,
}

/// The pattern with each variable made a name the grammar reads as one,
/// and which name is which variable.
fn stand_ins(source: &str) -> (String, HashMap<String, Variable>) {
    let mut variables = HashMap::new();
    let text = variables_in(source, |name, many| {
        let stand_in = format!("__med{}", variables.len());
        let name = name.to_string();
        variables.insert(stand_in.clone(), Variable { name, many });
        stand_in
    });
    (text, variables)
}

/// `text` with each variable what `each` makes of its name and whether it
/// is a `$name*$`.
fn variables_in(text: &str, mut each: impl FnMut(&str, bool) -> String) -> String {
    variables()
        .replace_all(text, |found: &regex::Captures| {
            each(&found[1], !found[2].is_empty())
        })
        .into_owned()
}

/// What a pattern is made of, node by node.
#[derive(Debug)]
enum Shape {
    /// Any one node, or with `many` any run of them side by side.
    Variable { name: String, many: bool },
    /// A node with no parts: of this kind and with this text.
    Leaf { kind: u16, text: String },
    /// A node of this kind, its parts each of the shape given. `gaps` is
    /// what of its text is in no part, a string's words in some grammars.
    Branch {
        kind: u16,
        gaps: Vec<String>,
        parts: Vec<Shape>,
    },
}

/// The parts of a node that count: not comments, not what the parser
/// made up to mend a mistake, not the commas between things.
fn parts(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|part| !part.is_extra() && !part.is_missing() && part.kind() != ",")
        .collect()
}

/// The text of `node` that is in none of its children, a piece for each
/// gap between them, without the space around it.
fn gaps<'a>(node: Node, text: &'a str) -> Vec<&'a str> {
    let mut cursor = node.walk();
    let mut at = node.start_byte();
    let mut gaps = Vec::new();
    for child in node.children(&mut cursor) {
        gaps.push(text.get(at..child.start_byte()).unwrap_or_default().trim());
        at = at.max(child.end_byte());
    }
    gaps.push(text.get(at..node.end_byte()).unwrap_or_default().trim());
    gaps.retain(|gap| !gap.is_empty());
    gaps
}

fn shape(node: Node, text: &str, variables: &HashMap<String, Variable>) -> Shape {
    let own = &text[node.byte_range()];
    if let Some(variable) = variables.get(own) {
        return Shape::Variable {
            name: variable.name.clone(),
            many: variable.many,
        };
    }
    let parts = parts(node);
    if parts.is_empty() {
        return Shape::Leaf {
            kind: node.kind_id(),
            text: own.to_string(),
        };
    }
    Shape::Branch {
        kind: node.kind_id(),
        gaps: gaps(node, text).into_iter().map(str::to_string).collect(),
        parts: parts
            .into_iter()
            .map(|part| shape(part, text, variables))
            .collect(),
    }
}

impl Shape {
    /// Text every match has in it somewhere, to pass over the files
    /// without it unparsed.
    fn literals(&self, out: &mut Vec<String>) {
        match self {
            Shape::Variable { .. } => {}
            Shape::Leaf { text, .. } => out.push(text.clone()),
            Shape::Branch { gaps, parts, .. } => {
                out.extend(gaps.iter().cloned());
                for part in parts {
                    part.literals(out);
                }
            }
        }
    }
}

/// The nodes of a parsed pattern that are the pattern: the one node that
/// is all of it, or else the parts of one node that are all of it and
/// nothing more, as two statements are.
fn pattern_nodes(root: Node, range: Range<usize>) -> Option<Vec<Node>> {
    let node = root.descendant_for_byte_range(range.start, range.end)?;
    // The root is the whole file, never a pattern: what it holds is.
    if node.byte_range() == range && node.parent().is_some() {
        return Some(vec![node]);
    }
    let parts = parts(node);
    let inside = |part: &Node| range.start <= part.start_byte() && part.end_byte() <= range.end;
    let across = |part: &Node| part.start_byte() < range.end && part.end_byte() > range.start;
    if parts.iter().any(|part| across(part) && !inside(part)) {
        return None;
    }
    let within: Vec<Node> = parts.into_iter().filter(inside).collect();
    let tiles =
        within.first()?.start_byte() == range.start && within.last()?.end_byte() == range.end;
    tiles.then_some(within)
}

/// A pattern, ready to be looked for.
pub struct Pattern {
    pub lang: &'static LangInfo,
    language: tree_sitter::Language,
    /// One shape for a pattern that is one node, more for a run of them.
    shapes: Vec<Shape>,
    literals: Vec<String>,
    /// The names of its variables, `_` left out.
    names: Vec<String>,
}

/// One place a pattern was found: its bytes, and what each variable
/// stands for there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub range: Range<usize>,
    pub captures: Vec<(String, Range<usize>)>,
    /// A comment in the match is in no variable: a replacement would
    /// lose it, so a replace leaves this match as it is.
    pub loses_comments: bool,
}

/// What is kept of each variable while a match is tried.
type Captures<'p> = Vec<(&'p str, Range<usize>)>;

impl Pattern {
    pub fn new(source: &str, lang: &'static LangInfo) -> Result<Self, String> {
        let language = syntax::language(lang)
            .ok_or_else(|| format!("there is no grammar for {}", lang.name))?;
        let (text, variables) = stand_ins(source.trim());
        if text.is_empty() {
            return Err("the pattern is empty".into());
        }
        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .map_err(|error| error.to_string())?;
        for wrapper in wrappers(lang.name) {
            let (before, after) = wrapper.split_once("{}").unwrap();
            let wrapped = format!("{before}{text}{after}");
            let Some(tree) = parser.parse(&wrapped, None) else {
                continue;
            };
            if tree.root_node().has_error() {
                continue;
            }
            let range = before.len()..before.len() + text.len();
            let Some(nodes) = pattern_nodes(tree.root_node(), range) else {
                continue;
            };
            let shapes: Vec<Shape> = nodes
                .into_iter()
                .map(|node| shape(node, &wrapped, &variables))
                .collect();
            let mut literals = Vec::new();
            for shape in &shapes {
                shape.literals(&mut literals);
            }
            literals.sort();
            literals.dedup();
            let mut names: Vec<String> = variables
                .into_values()
                .map(|variable| variable.name)
                .filter(|name| name != "_")
                .collect();
            names.sort();
            names.dedup();
            return Ok(Self {
                lang,
                language,
                shapes,
                literals,
                names,
            });
        }
        Err(format!("the pattern cannot be read as {}", lang.name))
    }

    /// Whether `text` may have a match: it has every word the pattern has.
    fn could_be_in(&self, text: &str) -> bool {
        self.literals
            .iter()
            .all(|literal| text.contains(literal.as_str()))
    }

    /// Every place in `tree`, of `text`, that has the pattern's shape,
    /// from the top. A match may be in another.
    pub fn find(&self, tree: &Tree, text: &str) -> Vec<Found> {
        let mut found = Vec::new();
        let mut nodes = vec![tree.root_node()];
        while let Some(node) = nodes.pop() {
            match self.shapes.as_slice() {
                [shape] => {
                    let mut captures = Vec::new();
                    if node.is_named() && matches(shape, node, text, &mut captures) {
                        found.push(found_at(node.byte_range(), captures));
                    }
                }
                shapes => found.extend(runs(shapes, node, text)),
            }
            let mut cursor = node.walk();
            let children: Vec<Node> = node.children(&mut cursor).collect();
            nodes.extend(children.into_iter().rev());
        }
        found.sort_by_key(|found| (found.range.start, std::cmp::Reverse(found.range.end)));
        found.dedup_by(|a, b| a.range == b.range);
        for one in &mut found {
            let mut comments = Vec::new();
            extras_in(tree.root_node(), &one.range, &mut comments);
            one.loses_comments = comments.iter().any(|comment| {
                !one.captures.iter().any(|(_, captured)| {
                    captured.start <= comment.start && comment.end <= captured.end
                })
            });
        }
        found
    }

    /// Every match in a text of the pattern's language.
    pub fn find_in(&self, text: &str) -> Vec<Found> {
        if !self.could_be_in(text) {
            return Vec::new();
        }
        let mut parser = Parser::new();
        if parser.set_language(&self.language).is_err() {
            return Vec::new();
        }
        parser
            .parse(text, None)
            .map_or_else(Vec::new, |tree| self.find(&tree, text))
    }
}

fn found_at(range: Range<usize>, captures: Captures) -> Found {
    Found {
        range,
        captures: captures
            .into_iter()
            .map(|(name, range)| (name.to_string(), range))
            .collect(),
        loses_comments: false,
    }
}

/// The comments, and whatever else the grammar lets be anywhere, that are
/// in `range`.
fn extras_in(node: Node, range: &Range<usize>, out: &mut Vec<Range<usize>>) {
    if node.end_byte() <= range.start || node.start_byte() >= range.end {
        return;
    }
    if node.is_extra() && range.start <= node.start_byte() && node.end_byte() <= range.end {
        out.push(node.byte_range());
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extras_in(child, range, out);
    }
}

/// The runs of `node`'s parts that a pattern of more than one node
/// matches, none in another.
fn runs(shapes: &[Shape], node: Node, text: &str) -> Vec<Found> {
    let parts = parts(node);
    let mut found = Vec::new();
    let mut start = 0;
    while start < parts.len() {
        let at = parts[start].start_byte();
        // The shortest run from here that matches.
        let hit = (start + 1..=parts.len()).find_map(|end| {
            let mut captures = Vec::new();
            sequence(shapes, &parts[start..end], at, text, &mut captures).then_some((end, captures))
        });
        match hit {
            Some((end, captures)) => {
                found.push(found_at(at..parts[end - 1].end_byte(), captures));
                start = end;
            }
            None => start += 1,
        }
    }
    found
}

/// Keep what `name` stands for, or check it is the same text as before.
fn bind<'p>(name: &'p str, range: Range<usize>, text: &str, captures: &mut Captures<'p>) -> bool {
    if name == "_" {
        return true;
    }
    match captures.iter().find(|(known, _)| *known == name) {
        Some((_, before)) => text[before.clone()] == text[range],
        None => {
            captures.push((name, range));
            true
        }
    }
}

fn matches<'p>(shape: &'p Shape, node: Node, text: &str, captures: &mut Captures<'p>) -> bool {
    match shape {
        Shape::Variable { name, .. } => bind(name, node.byte_range(), text, captures),
        Shape::Leaf { kind, text: own } => {
            node.kind_id() == *kind && text[node.byte_range()] == *own
        }
        Shape::Branch {
            kind,
            gaps: own,
            parts: shapes,
        } => {
            if node.kind_id() != *kind || !gaps(node, text).iter().eq(own.iter()) {
                return false;
            }
            sequence(shapes, &parts(node), node.start_byte(), text, captures)
        }
    }
}

/// Whether `nodes`, side by side from byte `at`, are `shapes` one by one,
/// a `$name*$` taking as many as it must. What is kept is as it was when
/// they are not.
fn sequence<'p>(
    shapes: &'p [Shape],
    nodes: &[Node],
    at: usize,
    text: &str,
    captures: &mut Captures<'p>,
) -> bool {
    let Some((first, rest)) = shapes.split_first() else {
        return nodes.is_empty();
    };
    let kept = captures.len();
    if let Shape::Variable { name, many: true } = first {
        for take in 0..=nodes.len() {
            let range = match take {
                0 => at..at,
                take => nodes[0].start_byte()..nodes[take - 1].end_byte(),
            };
            let next = range.end;
            if bind(name, range, text, captures)
                && sequence(rest, &nodes[take..], next, text, captures)
            {
                return true;
            }
            captures.truncate(kept);
        }
        return false;
    }
    let Some((node, more)) = nodes.split_first() else {
        return false;
    };
    if matches(first, *node, text, captures)
        && sequence(rest, more, node.end_byte(), text, captures)
    {
        return true;
    }
    captures.truncate(kept);
    false
}

/// The matches none of the others are in: the ones a replace changes.
pub fn outermost(found: Vec<Found>) -> Vec<Found> {
    let mut end = 0;
    let mut kept = Vec::new();
    for one in found {
        if one.range.start >= end {
            end = one.range.end;
            kept.push(one);
        }
    }
    kept
}

/// A replacement, ready to be filled in.
pub struct Replacement(Vec<Piece>);

enum Piece {
    Text(String),
    Variable(String),
}

impl Replacement {
    pub fn new(source: &str, pattern: &Pattern) -> Result<Self, String> {
        let mut pieces = Vec::new();
        let mut at = 0;
        for found in variables().captures_iter(source) {
            let whole = found.get(0).unwrap();
            let name = &found[1];
            if !pattern.names.iter().any(|known| known == name) {
                return Err(format!("${name}$ is not one of the pattern's"));
            }
            pieces.push(Piece::Text(source[at..whole.start()].to_string()));
            pieces.push(Piece::Variable(name.to_string()));
            at = whole.end();
        }
        pieces.push(Piece::Text(source[at..].to_string()));
        pieces.retain(|piece| !matches!(piece, Piece::Text(text) if text.is_empty()));
        Ok(Self(pieces))
    }

    /// What `found`, in `text`, is to be.
    pub fn fill(&self, found: &Found, text: &str) -> String {
        let line = text[..found.range.start].rfind('\n').map_or(0, |at| at + 1);
        let indent: String = text[line..]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let mut out = String::new();
        for (index, piece) in self.0.iter().enumerate() {
            match piece {
                Piece::Variable(name) => {
                    let captured = found.captures.iter().find(|(known, _)| known == name);
                    out.push_str(captured.map_or("", |(_, range)| &text[range.clone()]));
                }
                Piece::Text(own) => {
                    let more = index + 1 < self.0.len();
                    let mut lines = own.split('\n');
                    out.push_str(lines.next().unwrap_or_default());
                    let rest: Vec<&str> = lines.collect();
                    for (n, line) in rest.iter().enumerate() {
                        out.push('\n');
                        // A blank line stays blank, unless what follows
                        // on it is a variable.
                        if !line.is_empty() || (n + 1 == rest.len() && more) {
                            out.push_str(&indent);
                        }
                        out.push_str(line);
                    }
                }
            }
        }
        out
    }
}

/// The matches in one file.
pub struct FileFound {
    pub path: PathBuf,
    /// The path from the project's root.
    pub file: String,
    /// The text that was searched.
    pub text: String,
    pub found: Vec<Found>,
}

/// The matches in the project's files of the pattern's language, up to
/// `limit` of them. A file open in a tab is searched as it is there.
pub fn search(
    root: &Path,
    pattern: &Pattern,
    open: &HashMap<PathBuf, String>,
    limit: usize,
    cancel: &AtomicBool,
) -> Vec<FileFound> {
    let mut parser = Parser::new();
    if parser.set_language(&pattern.language).is_err() {
        return Vec::new();
    }
    let mut files = Vec::new();
    let mut count = 0;
    for file in search::walk_files(root) {
        if cancel.load(Ordering::Relaxed) || count >= limit {
            break;
        }
        let path = root.join(&file);
        if syntax::lang_for_path(&path).map(|lang| lang.name) != Some(pattern.lang.name) {
            continue;
        }
        let text = match open.get(&path) {
            Some(text) => text.clone(),
            None => match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(_) => continue,
            },
        };
        if !pattern.could_be_in(&text) {
            continue;
        }
        let Some(tree) = parser.parse(&text, None) else {
            continue;
        };
        let mut found = pattern.find(&tree, &text);
        found.truncate(limit - count);
        count += found.len();
        if !found.is_empty() {
            files.push(FileFound {
                path,
                file,
                text,
                found,
            });
        }
    }
    files
}

/// The line of byte `at` of `text`, and the column there in chars and in
/// UTF-16 units, all from 0.
pub fn place(text: &str, at: usize) -> (usize, usize, usize) {
    let before = &text[..at];
    let line = before.matches('\n').count();
    let start = before.rfind('\n').map_or(0, |at| at + 1);
    let column = &before[start..];
    (line, column.chars().count(), column.encode_utf16().count())
}

/// The edits a replace makes in one file: each range of the text it was
/// worked out from, and what is to be there.
pub struct FileEdits {
    pub path: PathBuf,
    pub text: String,
    pub edits: Vec<(Range<usize>, String)>,
    /// Matches left as they are, a comment in each being in no variable.
    pub left: usize,
}

/// A match a program chose: its file, and where it starts and ends as
/// (line, column in chars), from 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen {
    pub path: PathBuf,
    pub start: (usize, usize),
    pub end: (usize, usize),
}

/// Whether `found`, in `text`, is the match `chosen` is.
fn is_chosen(path: &Path, text: &str, found: &Found, chosen: &Chosen) -> bool {
    let at = |byte| {
        let (line, column, _) = place(text, byte);
        (line, column)
    };
    chosen.path == path
        && chosen.start == at(found.range.start)
        && chosen.end == at(found.range.end)
}

/// The edits that make the matches in `files` what `replacement` says:
/// every one, or the ones `chosen` when there is a choice. A match that
/// would lose a comment is left, and counted in [`FileEdits::left`].
pub fn edits(
    files: Vec<FileFound>,
    replacement: &Replacement,
    chosen: Option<&[Chosen]>,
) -> Vec<FileEdits> {
    files
        .into_iter()
        .filter_map(|file| {
            let found: Vec<Found> = file
                .found
                .into_iter()
                .filter(|found| {
                    chosen.is_none_or(|chosen| {
                        chosen
                            .iter()
                            .any(|chosen| is_chosen(&file.path, &file.text, found, chosen))
                    })
                })
                .collect();
            let (kept, lost): (Vec<Found>, Vec<Found>) = outermost(found)
                .into_iter()
                .partition(|found| !found.loses_comments);
            let edits: Vec<(Range<usize>, String)> = kept
                .iter()
                .map(|found| (found.range.clone(), replacement.fill(found, &file.text)))
                .collect();
            (!edits.is_empty() || !lost.is_empty()).then_some(FileEdits {
                path: file.path,
                text: file.text,
                edits,
                left: lost.len(),
            })
        })
        .collect()
}

/// What a replace did.
pub struct Replaced {
    pub changes: usize,
    pub files: usize,
    /// Matches left as they are: each in a file that changed while they
    /// were being found, or with a comment the replacement would lose.
    pub skipped: usize,
}

/// Make the edits: in the tabs that have the files open, and on disk for
/// the rest. A file whose text is no longer what was searched is left as
/// it is.
pub(crate) fn apply(
    workspace: &mut Workspace,
    files: Vec<FileEdits>,
    window: &Window,
    cx: &mut Context<Workspace>,
) -> Replaced {
    let open = workspace.open_texts(cx);
    let mut skipped = 0;
    let mut documents = Vec::new();
    for file in files {
        skipped += file.left;
        let now = match open.get(&file.path) {
            Some(text) => Some(text.clone()),
            None => std::fs::read_to_string(&file.path).ok(),
        };
        if now.as_deref() != Some(file.text.as_str()) {
            skipped += file.edits.len();
            continue;
        }
        let Some(uri) = crate::lsp::path_to_uri(&file.path) else {
            skipped += file.edits.len();
            continue;
        };
        let position = |at: usize| {
            let (line, _, character) = place(&file.text, at);
            lsp::Position::new(line as u32, character as u32)
        };
        let edits = file
            .edits
            .into_iter()
            .map(|(range, new_text)| {
                lsp::OneOf::Left(lsp::TextEdit {
                    range: lsp::Range::new(position(range.start), position(range.end)),
                    new_text,
                })
            })
            .collect();
        documents.push(lsp::TextDocumentEdit {
            text_document: lsp::OptionalVersionedTextDocumentIdentifier { uri, version: None },
            edits,
        });
    }
    let edit = lsp::WorkspaceEdit {
        document_changes: Some(lsp::DocumentChanges::Edits(documents)),
        ..Default::default()
    };
    let (changes, files) = workspace.apply_workspace_edit(edit, window, cx);
    Replaced {
        changes,
        files,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(lang: &str, source: &str) -> Pattern {
        let lang = syntax::lang_named(lang).unwrap();
        Pattern::new(source, lang).unwrap_or_else(|problem| panic!("{source}: {problem}"))
    }

    /// The text of each match of `source` in `text`.
    fn found(lang: &str, source: &str, text: &str) -> Vec<String> {
        let pattern = pattern(lang, source);
        let found = pattern.find_in(text);
        found
            .iter()
            .map(|found| text[found.range.clone()].to_string())
            .collect()
    }

    /// `text` with each outermost match of `source` made `replacement`.
    fn replaced(lang: &str, source: &str, replacement: &str, text: &str) -> String {
        let pattern = pattern(lang, source);
        let replacement = Replacement::new(replacement, &pattern).unwrap();
        let mut out = text.to_string();
        let found = outermost(pattern.find_in(text));
        for found in found.iter().rev().filter(|found| !found.loses_comments) {
            out.replace_range(found.range.clone(), &replacement.fill(found, text));
        }
        out
    }

    const RUST: &str = "\
fn main() {
    let a = first().unwrap();
    let b = second(1, 2).unwrap(); // why not
    let c = third(first().unwrap());
    if a == a { go(a, b, c); }
    if a == b { go(); }
}
";

    #[test]
    fn finds_by_shape_and_not_by_text() {
        assert_eq!(
            found("rust", "$x$.unwrap()", RUST),
            [
                "first().unwrap()",
                "second(1, 2).unwrap()",
                "first().unwrap()",
            ]
        );
        // Spaces and line breaks do not count.
        assert_eq!(found("rust", "second( 1,\n 2 )", RUST), ["second(1, 2)"]);
        assert!(found("rust", "second(2, 1)", RUST).is_empty());
    }

    #[test]
    fn a_name_twice_is_the_same_text() {
        assert_eq!(found("rust", "$x$ == $x$", RUST), ["a == a"]);
        assert_eq!(found("rust", "$x$ == $y$", RUST), ["a == a", "a == b"]);
        // `$_$` is anything each time.
        assert_eq!(found("rust", "$_$ == $_$", RUST), ["a == a", "a == b"]);
    }

    #[test]
    fn a_starred_name_takes_any_number() {
        assert_eq!(found("rust", "go($args*$)", RUST), ["go(a, b, c)", "go()"]);
        assert_eq!(found("rust", "go($first$, $rest*$)", RUST), ["go(a, b, c)"]);
        assert_eq!(found("rust", "go($a$)", RUST), Vec::<String>::new());
    }

    #[test]
    fn statements_side_by_side() {
        assert_eq!(
            found("rust", "let $a$ = $x$; let $b$ = $y$;", RUST),
            ["let a = first().unwrap();\n    let b = second(1, 2).unwrap();",]
        );
    }

    #[test]
    fn other_languages_read_their_patterns() {
        let go = "package main\n\nfunc main() {\n\tfmt.Println(x)\n\tlog.Println(y)\n}\n";
        assert_eq!(found("go", "fmt.Println($a$)", go), ["fmt.Println(x)"]);
        let java = "class A {\n  void f() {\n    list.add(1);\n    set.add(2);\n  }\n}\n";
        assert_eq!(
            found("java", "$c$.add($v$)", java),
            ["list.add(1)", "set.add(2)"]
        );
        let python = "def f():\n    print(a)\n    print(a, b)\n";
        assert_eq!(found("python", "print($x$)", python), ["print(a)"]);
        let js = "const a = foo(1);\nfoo(2, 3);\n";
        assert_eq!(
            found("javascript", "foo($args*$)", js),
            ["foo(1)", "foo(2, 3)"]
        );
    }

    #[test]
    fn strings_are_told_apart() {
        let rust = "fn f() { say(\"hi\"); say(\"bye\"); }\n";
        assert_eq!(found("rust", "say(\"bye\")", rust), ["say(\"bye\")"]);
    }

    #[test]
    fn a_pattern_that_is_no_code_is_refused() {
        let lang = syntax::lang_named("rust").unwrap();
        assert!(Pattern::new("fn (((", lang).is_err());
        assert!(Pattern::new("  ", lang).is_err());
        let pattern = pattern("rust", "$x$.unwrap()");
        assert!(Replacement::new("$y$.expect(\"\")", &pattern).is_err());
    }

    #[test]
    fn replaces_with_what_was_found() {
        assert_eq!(
            replaced("rust", "$x$.unwrap()", "$x$?", RUST),
            RUST.replace("first().unwrap()", "first()?")
                .replace("second(1, 2).unwrap()", "second(1, 2)?")
        );
        // The comment the match ends next to stays.
        assert!(replaced("rust", "$x$.unwrap()", "$x$?", RUST).contains("?; // why not"));
        assert_eq!(
            replaced(
                "rust",
                "go($args*$)",
                "run(ctx, $args*$)",
                "fn f() { go(a, b); }"
            ),
            "fn f() { run(ctx, a, b); }"
        );
    }

    #[test]
    fn a_match_that_would_lose_a_comment_is_left() {
        let text = "fn f() { go(x, /* keep */ y); go(x, y); }";
        assert_eq!(
            replaced("rust", "go($a$, $b$)", "go($b$, $a$)", text),
            "fn f() { go(x, /* keep */ y); go(y, x); }"
        );
        // A comment in what a variable stands for goes with it.
        assert_eq!(
            replaced("rust", "go($args*$)", "run($args*$)", text),
            "fn f() { run(x, /* keep */ y); run(x, y); }"
        );
        let two = "fn f() {\n    let a = 1;\n    // note\n    let b = 2;\n}\n";
        assert_eq!(
            replaced(
                "rust",
                "let $a$ = $x$; let $b$ = $y$;",
                "let ($a$, $b$) = ($x$, $y$);",
                two
            ),
            two
        );
        let pattern = pattern("rust", "go($a$, $b$)");
        let found = pattern.find_in(text);
        assert_eq!(
            found
                .iter()
                .map(|found| found.loses_comments)
                .collect::<Vec<_>>(),
            [true, false]
        );
    }

    #[test]
    fn edits_only_what_was_chosen() {
        let pattern = pattern("rust", "go($a$)");
        let replacement = Replacement::new("run($a$)", &pattern).unwrap();
        let text = "fn f() {\n    go(1);\n    go(2);\n}\n".to_string();
        let path = PathBuf::from("/p/a.rs");
        let file = || FileFound {
            path: path.clone(),
            file: "a.rs".into(),
            text: text.clone(),
            found: pattern.find_in(&text),
        };
        let chosen = [Chosen {
            path: path.clone(),
            start: (2, 4),
            end: (2, 9),
        }];
        let made = edits(vec![file()], &replacement, Some(&chosen));
        assert_eq!(made[0].edits, [(24..29, "run(2)".to_string())]);
        assert_eq!(edits(vec![file()], &replacement, None)[0].edits.len(), 2);
        assert!(edits(vec![file()], &replacement, Some(&[])).is_empty());
    }

    #[test]
    fn a_replacement_over_lines_is_indented_as_the_match() {
        let text = "fn f() {\n    if ok {\n        go(a);\n    }\n}\n";
        assert_eq!(
            replaced("rust", "go($a$);", "prepare();\ngo($a$);", text),
            "fn f() {\n    if ok {\n        prepare();\n        go(a);\n    }\n}\n"
        );
    }

    #[test]
    fn places_count_chars_and_utf16() {
        let text = "a\n字😀x";
        let at = text.find('x').unwrap();
        assert_eq!(place(text, at), (1, 2, 3));
    }
}
