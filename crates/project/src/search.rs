//! Project-wide search and replace, built on ripgrep's crates.

use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use editor_text::{Change, ChangeSet, Rope, SearchQuery};
use grep_matcher::Matcher as _;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::overrides::OverrideBuilder;

use crate::tree::ALWAYS_HIDDEN;

/// Longest line preview we keep; longer lines (minified files) are cut to a
/// window around the first match.
const MAX_PREVIEW: usize = 300;

#[derive(Clone, Debug, Default)]
pub struct SearchOptions {
    pub query: SearchQuery,
    /// Globs to include (e.g. `*.rs`, `src/**`). Empty means everything.
    pub include: Vec<String>,
    /// Globs to exclude.
    pub exclude: Vec<String>,
    /// Stop after this many matches in total (0 = unlimited).
    pub max_matches: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineMatch {
    /// 1-based.
    pub line_number: u64,
    /// The line, without its terminator, possibly cut down (see
    /// [`LineMatch::preview_offset`]).
    pub preview: String,
    /// Byte offset of `preview` within the full line.
    pub preview_offset: usize,
    /// Byte ranges of matches within `preview`.
    pub ranges: Vec<Range<usize>>,
    /// Byte ranges of matches within the full line (for replace).
    pub line_ranges: Vec<Range<usize>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMatches {
    pub path: PathBuf,
    /// Relative to the search root, `/`-separated.
    pub relative: String,
    pub lines: Vec<LineMatch>,
}

impl FileMatches {
    pub fn match_count(&self) -> usize {
        self.lines.iter().map(|l| l.line_ranges.len().max(1)).sum()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchStats {
    pub files_searched: usize,
    pub files_matched: usize,
    pub matches: usize,
    /// The search stopped early (limit reached or cancelled).
    pub truncated: bool,
}

fn build_matcher(query: &SearchQuery) -> Result<RegexMatcher, String> {
    if query.pattern.is_empty() {
        return Err("empty pattern".into());
    }
    let mut builder = RegexMatcherBuilder::new();
    builder
        .case_insensitive(!query.case_sensitive)
        .fixed_strings(!query.regex)
        .word(query.whole_word)
        .crlf(true)
        .line_terminator(Some(b'\n'));
    builder.build(&query.pattern).map_err(|e| e.to_string())
}

/// Searches every file under `root` (respecting ignore files) in parallel.
/// `on_file` is called from worker threads as files with matches are found,
/// in no particular order.
pub fn search(
    root: &Path,
    options: &SearchOptions,
    cancel: &AtomicBool,
    on_file: impl Fn(FileMatches) + Sync,
) -> Result<SearchStats, String> {
    let matcher = build_matcher(&options.query)?;

    let mut overrides = OverrideBuilder::new(root);
    for glob in &options.include {
        overrides.add(glob).map_err(|e| e.to_string())?;
    }
    for glob in &options.exclude {
        overrides.add(&format!("!{glob}")).map_err(|e| e.to_string())?;
    }
    let overrides = overrides.build().map_err(|e| e.to_string())?;

    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .overrides(overrides)
        .filter_entry(|e| {
            !e.file_name().to_str().is_some_and(|name| ALWAYS_HIDDEN.contains(&name))
        })
        .build_parallel();

    let files_searched = AtomicUsize::new(0);
    let files_matched = AtomicUsize::new(0);
    let total = AtomicUsize::new(0);
    let limit_hit = AtomicBool::new(false);
    let max = if options.max_matches == 0 { usize::MAX } else { options.max_matches };

    walker.run(|| {
        let matcher = matcher.clone();
        let mut searcher = SearcherBuilder::new()
            .binary_detection(BinaryDetection::quit(b'\x00'))
            .line_number(true)
            .build();
        let on_file = &on_file;
        let (files_searched, files_matched, total, limit_hit) =
            (&files_searched, &files_matched, &total, &limit_hit);
        Box::new(move |entry| {
            if cancel.load(Ordering::Relaxed) || limit_hit.load(Ordering::Relaxed) {
                return ignore::WalkState::Quit;
            }
            let Ok(entry) = entry else { return ignore::WalkState::Continue };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return ignore::WalkState::Continue;
            }
            files_searched.fetch_add(1, Ordering::Relaxed);
            let mut sink = CollectSink { matcher: &matcher, lines: Vec::new() };
            if searcher.search_path(&matcher, entry.path(), &mut sink).is_err() {
                return ignore::WalkState::Continue;
            }
            if sink.lines.is_empty() {
                return ignore::WalkState::Continue;
            }
            let found = FileMatches {
                path: entry.path().to_path_buf(),
                relative: relative(root, entry.path()),
                lines: sink.lines,
            };
            let count = found.match_count();
            files_matched.fetch_add(1, Ordering::Relaxed);
            if total.fetch_add(count, Ordering::Relaxed) + count >= max {
                limit_hit.store(true, Ordering::Relaxed);
            }
            on_file(found);
            ignore::WalkState::Continue
        })
    });

    Ok(SearchStats {
        files_searched: files_searched.into_inner(),
        files_matched: files_matched.into_inner(),
        matches: total.into_inner(),
        truncated: limit_hit.into_inner() || cancel.load(Ordering::Relaxed),
    })
}

fn relative(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        rel.into_owned()
    } else {
        rel.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

struct CollectSink<'a> {
    matcher: &'a RegexMatcher,
    lines: Vec<LineMatch>,
}

impl Sink for CollectSink<'_> {
    type Error = io::Error;

    fn matched(&mut self, _: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, io::Error> {
        let mut bytes = mat.bytes();
        while let Some((&last, rest)) = bytes.split_last() {
            if last == b'\n' || last == b'\r' {
                bytes = rest;
            } else {
                break;
            }
        }
        let mut line_ranges = Vec::new();
        let _ = self.matcher.find_iter(bytes, |m| {
            if m.start() < m.end() {
                line_ranges.push(m.start()..m.end());
            }
            true
        });
        let (preview, preview_offset, ranges) = make_preview(bytes, &line_ranges);
        self.lines.push(LineMatch {
            line_number: mat.line_number().unwrap_or(0),
            preview,
            preview_offset,
            ranges,
            line_ranges,
        });
        Ok(true)
    }
}

fn make_preview(line: &[u8], ranges: &[Range<usize>]) -> (String, usize, Vec<Range<usize>>) {
    if line.len() <= MAX_PREVIEW {
        return (String::from_utf8_lossy(line).into_owned(), 0, ranges.to_vec());
    }
    let first = ranges.first().map_or(0, |r| r.start);
    let mut start = first.saturating_sub(MAX_PREVIEW / 4);
    while start > 0 && (line[start] & 0xC0) == 0x80 {
        start -= 1;
    }
    let mut end = (start + MAX_PREVIEW).min(line.len());
    while end < line.len() && (line[end] & 0xC0) == 0x80 {
        end += 1;
    }
    let preview = String::from_utf8_lossy(&line[start..end]).into_owned();
    let ranges = ranges
        .iter()
        .filter(|r| r.start >= start && r.end <= end)
        .map(|r| r.start - start..r.end - start)
        .collect();
    (preview, start, ranges)
}

/// One match chosen for replacement: its line and byte range in that line.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MatchRef {
    pub line_number: u64,
    pub range: Range<usize>,
}

/// Replaces matches of `query` in the file at `path` and writes it back
/// atomically (temp file + rename, keeping permissions).
///
/// If `only` is given, only those matches are replaced; matches that no
/// longer exist (the file changed since the search) are skipped. Returns the
/// number of replacements made.
pub fn replace_in_file(
    path: &Path,
    query: &SearchQuery,
    replacement: &str,
    only: Option<&[MatchRef]>,
) -> io::Result<usize> {
    let text = std::fs::read_to_string(path)?;
    let Some(new_text) = replace_in_text(&text, query, replacement, only)? else {
        return Ok(0);
    };
    let (new_text, count) = new_text;
    write_atomically(path, new_text.as_bytes())?;
    Ok(count)
}

type Replaced = Option<(String, usize)>;

/// The text-only half of [`replace_in_file`]. Uses the same matcher as
/// in-editor find/replace, so open buffers and files on disk behave the same.
pub fn replace_in_text(
    text: &str,
    query: &SearchQuery,
    replacement: &str,
    only: Option<&[MatchRef]>,
) -> io::Result<Replaced> {
    let matcher = query.compile().map_err(io::Error::other)?;
    let mut rope = Rope::from_str(text);
    let mut reps = matcher.replacements(&rope, 0..rope.len_chars(), replacement, query.regex);
    if let Some(only) = only {
        let wanted: std::collections::HashSet<&MatchRef> = only.iter().collect();
        reps.retain(|(range, _)| {
            let line = rope.char_to_line(range.start);
            let line_start = rope.line_to_byte(line);
            let start = rope.char_to_byte(range.start) - line_start;
            let end = rope.char_to_byte(range.end) - line_start;
            wanted.contains(&MatchRef { line_number: line as u64 + 1, range: start..end })
        });
    }
    if reps.is_empty() {
        return Ok(None);
    }
    let count = reps.len();
    let changes =
        ChangeSet::new(reps.into_iter().map(|(r, t)| Change::new(r.start, r.end, t)).collect());
    changes.apply(&mut rope);
    Ok(Some((rope.to_string(), count)))
}

fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    let permissions = std::fs::metadata(path)?.permissions();
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(contents)?;
    tmp.as_file().sync_all()?;
    std::fs::set_permissions(tmp.path(), permissions)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Convenience for callers that just want everything, sorted by path.
pub fn search_collect(root: &Path, options: &SearchOptions) -> Result<Vec<FileMatches>, String> {
    let results = parking_lot::Mutex::new(Vec::new());
    let cancel = Arc::new(AtomicBool::new(false));
    search(root, options, &cancel, |m| results.lock().push(m))?;
    let mut results = results.into_inner();
    results.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("build")).unwrap();
        fs::write(root.join(".gitignore"), "build/\n").unwrap();
        fs::write(root.join("src/a.rs"), "fn foo() {}\nlet x = foo();\nfoo_bar();\n").unwrap();
        fs::write(root.join("src/b.md"), "# Foo\nNothing here\r\nfoo again\r\n").unwrap();
        fs::write(root.join("build/out.rs"), "foo foo foo\n").unwrap();
        fs::write(root.join("bin.dat"), b"foo\0\x01\x02").unwrap();
        dir
    }

    fn query(pattern: &str) -> SearchQuery {
        SearchQuery::literal(pattern)
    }

    #[test]
    fn finds_matches_respecting_ignore_and_binary() {
        let dir = project();
        let opts = SearchOptions { query: query("foo"), ..Default::default() };
        let results = search_collect(dir.path(), &opts).unwrap();
        let files: Vec<_> = results.iter().map(|f| f.relative.as_str()).collect();
        assert_eq!(files, vec!["src/a.rs", "src/b.md"]);
        let a = &results[0];
        assert_eq!(a.lines.len(), 3);
        assert_eq!(a.lines[1].line_number, 2);
        assert_eq!(a.lines[1].ranges, vec![8..11]);
        // CRLF is stripped from previews.
        assert_eq!(results[1].lines[0].preview, "foo again");
    }

    #[test]
    fn options_case_word_and_globs() {
        let dir = project();
        let mut opts = SearchOptions {
            query: query("foo").whole_word(true).case_sensitive(false),
            ..Default::default()
        };
        let results = search_collect(dir.path(), &opts).unwrap();
        let total: usize = results.iter().map(|f| f.match_count()).sum();
        assert_eq!(total, 4); // foo() x2 in a.rs (not foo_bar), Foo + foo in b.md

        opts.include = vec!["*.md".into()];
        let results = search_collect(dir.path(), &opts).unwrap();
        assert_eq!(results.len(), 1);

        opts.include.clear();
        opts.exclude = vec!["*.md".into()];
        let results = search_collect(dir.path(), &opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].relative, "src/a.rs");
    }

    #[test]
    fn regex_search() {
        let dir = project();
        let opts = SearchOptions { query: query(r"foo\(\)").regex(true), ..Default::default() };
        let results = search_collect(dir.path(), &opts).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].match_count(), 2);
    }

    #[test]
    fn max_matches_truncates() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..50 {
            fs::write(dir.path().join(format!("f{i}.txt")), "hit\n").unwrap();
        }
        let opts = SearchOptions { query: query("hit"), max_matches: 10, ..Default::default() };
        let cancel = AtomicBool::new(false);
        let stats = search(dir.path(), &opts, &cancel, |_| {}).unwrap();
        assert!(stats.truncated);
        assert!(stats.matches < 50);
    }

    #[test]
    fn long_lines_get_a_window_preview() {
        let dir = tempfile::tempdir().unwrap();
        let line = format!("{}needle{}", "x".repeat(10_000), "y".repeat(10_000));
        fs::write(dir.path().join("min.js"), &line).unwrap();
        let opts = SearchOptions { query: query("needle"), ..Default::default() };
        let results = search_collect(dir.path(), &opts).unwrap();
        let m = &results[0].lines[0];
        assert!(m.preview.len() <= MAX_PREVIEW + 4);
        assert_eq!(&m.preview[m.ranges[0].clone()], "needle");
        assert_eq!(m.line_ranges[0], 10_000..10_006);
    }

    #[test]
    fn replace_all_and_selected() {
        let dir = project();
        let path = dir.path().join("src/a.rs");
        let n = replace_in_file(&path, &query("foo").whole_word(true), "baz", None).unwrap();
        assert_eq!(n, 2);
        assert_eq!(fs::read_to_string(&path).unwrap(), "fn baz() {}\nlet x = baz();\nfoo_bar();\n");

        let path = dir.path().join("src/b.md");
        let only = [MatchRef { line_number: 3, range: 0..3 }];
        let n = replace_in_file(&path, &query("foo").case_sensitive(false), "bar", Some(&only))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(fs::read_to_string(&path).unwrap(), "# Foo\nNothing here\r\nbar again\r\n");
    }

    #[test]
    fn regex_replace_with_groups() {
        let out = replace_in_text(
            "let a = 1;\nlet b = 2;\n",
            &query(r"let (\w) = (\d)").regex(true),
            "const $1: u8 = $2",
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(out.0, "const a: u8 = 1;\nconst b: u8 = 2;\n");
        assert_eq!(out.1, 2);
    }
}
