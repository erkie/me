//! Syntax highlighting with tree-sitter.
//!
//! * Grammars are linked in, but a language's tree-sitter `Language` and
//!   highlight query are only built the first time a file of that language
//!   opens, so startup pays nothing for languages you don't use.
//! * Parsing is incremental and can run on a background thread with a
//!   deadline ([`parse`]).
//! * Highlighting only queries the byte range on screen ([`Syntax::highlight`]).

use std::collections::HashMap;
use std::ops::{ControlFlow, Range};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use editor_text::{ByteEdit, Rope, RopeSlice};
use streaming_iterator::StreamingIterator;
use tree_sitter::{
    InputEdit, Node, ParseOptions, ParseState, Parser, Point, Query, QueryCursor, TextProvider,
    Tree,
};

mod languages;

pub use languages::LANGUAGES;

/// Files larger than this are not parsed (like Sublime, very large files open
/// as plain text).
pub const DEFAULT_MAX_FILE_SIZE: usize = 8 * 1024 * 1024;

/// The highlight categories themes assign colors to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Highlight {
    Comment,
    Keyword,
    String,
    StringEscape,
    Number,
    Constant,
    Function,
    Type,
    Property,
    Attribute,
    Tag,
    Operator,
    Punctuation,
    Label,
    Title,
    Emphasis,
    Strong,
    Link,
    Literal,
    DiffPlus,
    DiffMinus,
}

impl Highlight {
    /// Maps a tree-sitter capture name (`keyword.control`, `string.special`,
    /// `markup.heading`...) to a category. `None` means default text color.
    pub fn from_capture_name(name: &str) -> Option<Self> {
        use Highlight::*;
        let first = name.split('.').next().unwrap_or(name);
        Some(match name {
            n if n.starts_with("string.escape") || n == "escape" => StringEscape,
            n if n.starts_with("string.special.url") || n.starts_with("text.uri") => Link,
            n if n.starts_with("variable.builtin") => Keyword,
            n if n.starts_with("text.title") || n.starts_with("markup.heading") => Title,
            n if n.starts_with("text.emphasis") || n.starts_with("markup.italic") => Emphasis,
            n if n.starts_with("text.strong") || n.starts_with("markup.bold") => Strong,
            n if n.starts_with("text.reference") || n.starts_with("markup.link") => Link,
            n if n.starts_with("text.literal") || n.starts_with("markup.raw") => Literal,
            n if n.starts_with("diff.plus") || n.starts_with("diff.addition") => DiffPlus,
            n if n.starts_with("diff.minus") || n.starts_with("diff.deletion") => DiffMinus,
            _ => match first {
                "comment" => Comment,
                "keyword" | "conditional" | "repeat" | "include" | "exception" | "storageclass"
                | "preproc" | "define" => Keyword,
                "string" | "character" => String,
                "number" | "float" => Number,
                "boolean" | "constant" => Constant,
                "function" | "method" | "constructor" => Function,
                "type" | "class" | "interface" | "enum" | "struct" | "module" | "namespace" => Type,
                "property" | "field" => Property,
                "attribute" => Attribute,
                "tag" => Tag,
                "operator" => Operator,
                "punctuation" | "delimiter" => Punctuation,
                "label" => Label,
                "title" => Title,
                "emphasis" => Emphasis,
                "strong" => Strong,
                "link" | "uri" => Link,
                "addition" => DiffPlus,
                "deletion" => DiffMinus,
                _ => return None,
            },
        })
    }
}

/// A supported language: how to detect it and how to build it.
pub struct LanguageDef {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    pub filenames: &'static [&'static str],
    pub line_comment: Option<&'static str>,
    grammar: fn() -> tree_sitter::Language,
    /// Highlight queries, concatenated in order. Earlier patterns win when
    /// two match the same node.
    highlights: &'static [&'static str],
}

/// A built language, ready to parse and highlight.
pub struct Language {
    pub def: &'static LanguageDef,
    grammar: tree_sitter::Language,
    query: Query,
    capture_highlights: Vec<Option<Highlight>>,
}

impl std::fmt::Debug for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Language")
            .field("name", &self.def.name)
            .finish()
    }
}

impl LanguageDef {
    /// Builds (once) and returns the language. `None` if its query fails to
    /// compile, in which case files open without highlighting.
    pub fn load(&'static self) -> Option<Arc<Language>> {
        static CACHE: OnceLock<Mutex<HashMap<&'static str, Option<Arc<Language>>>>> =
            OnceLock::new();
        let cache = CACHE.get_or_init(Default::default);
        if let Some(loaded) = cache.lock().unwrap().get(self.name) {
            return loaded.clone();
        }
        let loaded = self.build().map(Arc::new);
        cache.lock().unwrap().insert(self.name, loaded.clone());
        loaded
    }

    fn build(&'static self) -> Option<Language> {
        let grammar = (self.grammar)();
        let source = self.highlights.join("\n");
        let query = match Query::new(&grammar, &source) {
            Ok(query) => query,
            Err(err) => {
                eprintln!(
                    "editor-syntax: highlight query for {} failed: {err}",
                    self.name
                );
                return None;
            }
        };
        let capture_highlights = query
            .capture_names()
            .iter()
            .map(|name| Highlight::from_capture_name(name))
            .collect();
        Some(Language {
            def: self,
            grammar,
            query,
            capture_highlights,
        })
    }
}

/// Picks a language from a file name.
pub fn language_for_path(path: &Path) -> Option<&'static LanguageDef> {
    let file_name = path.file_name()?.to_str()?;
    if let Some(def) = LANGUAGES.iter().find(|l| l.filenames.contains(&file_name)) {
        return Some(def);
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|l| l.extensions.contains(&ext.as_str()))
}

pub fn language_by_name(name: &str) -> Option<&'static LanguageDef> {
    let name = name.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|l| l.name.eq_ignore_ascii_case(&name) || l.extensions.contains(&name.as_str()))
}

/// A highlighted byte range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub range: Range<usize>,
    pub highlight: Highlight,
}

/// A document's syntax tree.
pub struct Syntax {
    language: Arc<Language>,
    tree: Option<Tree>,
}

impl Syntax {
    pub fn new(language: Arc<Language>) -> Self {
        Self {
            language,
            tree: None,
        }
    }

    pub fn language(&self) -> &Arc<Language> {
        &self.language
    }

    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    pub fn set_tree(&mut self, tree: Tree) {
        self.tree = Some(tree);
    }

    /// Shifts the existing tree to match edits made to the document. Cheap;
    /// highlighting stays roughly right until the next parse finishes.
    pub fn edit(&mut self, edits: &[ByteEdit]) {
        if let Some(tree) = &mut self.tree {
            for edit in edits {
                tree.edit(&to_input_edit(edit));
            }
        }
    }

    /// Parses on the calling thread. Returns false if it ran out of time.
    pub fn parse(&mut self, rope: &Rope, timeout: Duration) -> bool {
        match parse(&self.language, rope, self.tree.as_ref(), timeout) {
            Some(tree) => {
                self.tree = Some(tree);
                true
            }
            None => false,
        }
    }

    /// Highlight spans covering `range` (bytes), clipped to it, sorted and
    /// non-overlapping. Inner nodes win over outer ones; for the same node,
    /// the earlier query pattern wins.
    pub fn highlight(&self, rope: &Rope, range: Range<usize>) -> Vec<HighlightSpan> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let range = range.start.min(rope.len_bytes())..range.end.min(rope.len_bytes());
        if range.is_empty() {
            return Vec::new();
        }
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        cursor.set_match_limit(256);
        let query = &self.language.query;
        let mut captures = cursor.captures(query, tree.root_node(), RopeText(rope.slice(..)));

        // (start, end, node length, pattern, highlight)
        let mut paints: Vec<(usize, usize, usize, usize, Highlight)> = Vec::new();
        while let Some((m, index)) = captures.next() {
            let capture = m.captures[*index];
            let Some(highlight) = self.language.capture_highlights[capture.index as usize] else {
                continue;
            };
            let node = capture.node;
            let start = node.start_byte().max(range.start);
            let end = node.end_byte().min(range.end);
            if start < end {
                paints.push((
                    start,
                    end,
                    node.byte_range().len(),
                    m.pattern_index,
                    highlight,
                ));
            }
        }
        paints.sort_by(|a, b| b.2.cmp(&a.2).then(b.3.cmp(&a.3)));

        let mut cells: Vec<Option<Highlight>> = vec![None; range.len()];
        for (start, end, _, _, highlight) in paints {
            cells[start - range.start..end - range.start].fill(Some(highlight));
        }

        let mut spans: Vec<HighlightSpan> = Vec::new();
        for (i, cell) in cells.into_iter().enumerate() {
            let Some(highlight) = cell else { continue };
            let at = range.start + i;
            match spans.last_mut() {
                Some(last) if last.range.end == at && last.highlight == highlight => {
                    last.range.end = at + 1
                }
                _ => spans.push(HighlightSpan {
                    range: at..at + 1,
                    highlight,
                }),
            }
        }
        spans
    }
}

/// Parses `rope`, reusing `old_tree` (already edited to match) when given.
/// Returns `None` if parsing takes longer than `timeout`. Safe to call on a
/// background thread with a cloned rope and tree.
pub fn parse(
    language: &Language,
    rope: &Rope,
    old_tree: Option<&Tree>,
    timeout: Duration,
) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&language.grammar).ok()?;
    let len = rope.len_bytes();
    let mut read = |byte: usize, _: Point| -> &[u8] {
        if byte >= len {
            return &[];
        }
        let (chunk, chunk_byte, _, _) = rope.chunk_at_byte(byte);
        &chunk.as_bytes()[byte - chunk_byte..]
    };
    let deadline = Instant::now() + timeout;
    let mut progress = |_: &ParseState| {
        if Instant::now() > deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut progress);
    parser.parse_with_options(&mut read, old_tree, Some(options))
}

fn to_input_edit(edit: &ByteEdit) -> InputEdit {
    let point = |(row, column): (usize, usize)| Point { row, column };
    InputEdit {
        start_byte: edit.start_byte,
        old_end_byte: edit.old_end_byte,
        new_end_byte: edit.new_end_byte,
        start_position: point(edit.start_point),
        old_end_position: point(edit.old_end_point),
        new_end_position: point(edit.new_end_point),
    }
}

/// Feeds rope chunks to tree-sitter's query predicates without copying.
struct RopeText<'a>(RopeSlice<'a>);

impl<'a> TextProvider<&'a [u8]> for RopeText<'a> {
    type I = ChunkBytes<'a>;

    fn text(&mut self, node: Node) -> Self::I {
        let len = self.0.len_bytes();
        let range = node.start_byte().min(len)..node.end_byte().min(len);
        ChunkBytes(self.0.byte_slice(range).chunks())
    }
}

struct ChunkBytes<'a>(ropey::iter::Chunks<'a>);

impl<'a> Iterator for ChunkBytes<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        self.0.next().map(str::as_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_text::EditorState;

    fn highlights_of(lang: &str, source: &str) -> Vec<(String, Highlight)> {
        let def = language_by_name(lang).unwrap();
        let mut syntax = Syntax::new(def.load().unwrap());
        let rope = Rope::from_str(source);
        assert!(syntax.parse(&rope, Duration::from_secs(5)));
        syntax
            .highlight(&rope, 0..rope.len_bytes())
            .into_iter()
            .map(|s| (source[s.range].to_string(), s.highlight))
            .collect()
    }

    #[test]
    fn every_language_builds() {
        for def in LANGUAGES {
            assert!(def.load().is_some(), "{} failed to build", def.name);
        }
    }

    #[test]
    fn detects_languages() {
        let name = |p: &str| language_for_path(Path::new(p)).map(|l| l.name);
        assert_eq!(name("src/main.rs"), Some("Rust"));
        assert_eq!(name("README.md"), Some("Markdown"));
        assert_eq!(name("App.TSX"), Some("TSX"));
        assert_eq!(name("Makefile"), Some("Make"));
        assert_eq!(name("notes.txt"), None);
    }

    #[test]
    fn highlights_rust() {
        let spans = highlights_of("rust", "// hi\nfn main() { let s = \"x\"; }\n");
        assert!(spans.contains(&("// hi".into(), Highlight::Comment)));
        assert!(spans.contains(&("fn".into(), Highlight::Keyword)));
        assert!(spans.contains(&("main".into(), Highlight::Function)));
        assert!(spans.contains(&("\"x\"".into(), Highlight::String)));
    }

    #[test]
    fn highlights_typescript_with_inherited_javascript_query() {
        let spans = highlights_of("typescript", "const x: number = 1;\n");
        assert!(spans.contains(&("const".into(), Highlight::Keyword)));
        assert!(spans.contains(&("number".into(), Highlight::Type)));
        assert!(spans.contains(&("1".into(), Highlight::Number)));
    }

    #[test]
    fn viewport_highlighting_is_clipped() {
        let def = language_by_name("rust").unwrap();
        let mut syntax = Syntax::new(def.load().unwrap());
        let source = "fn a() {}\n".repeat(1000);
        let rope = Rope::from_str(&source);
        syntax.parse(&rope, Duration::from_secs(5));
        let spans = syntax.highlight(&rope, 5000..5030);
        assert!(!spans.is_empty());
        assert!(
            spans
                .iter()
                .all(|s| s.range.start >= 5000 && s.range.end <= 5030)
        );
    }

    #[test]
    fn incremental_reparse_tracks_edits() {
        let def = language_by_name("rust").unwrap();
        let mut syntax = Syntax::new(def.load().unwrap());
        let mut state = EditorState::from_str("fn main() {}\n");
        syntax.parse(state.doc.rope(), Duration::from_secs(5));

        state.set_cursor(0);
        state.insert("// ");
        syntax.edit(&state.doc.take_edits());
        syntax.parse(state.doc.rope(), Duration::from_secs(5));

        let rope = state.doc.rope();
        let spans = syntax.highlight(rope, 0..rope.len_bytes());
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].highlight, Highlight::Comment);
    }

    #[test]
    fn markdown_headings() {
        let spans = highlights_of("markdown", "# Title\n\nSome text.\n");
        assert!(
            spans
                .iter()
                .any(|(t, h)| t.contains("Title") && *h == Highlight::Title)
        );
    }
}
