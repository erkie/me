//! Cmd+T: a background file index matched with nucleo (the fuzzy matcher
//! from Helix).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Nucleo};

use crate::tree::ALWAYS_HIDDEN;

/// One result row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMatch {
    /// Path relative to the project root, with `/` separators.
    pub path: Arc<str>,
    /// Char indices into `path` that matched, for highlighting.
    pub indices: Vec<u32>,
}

/// Fuzzy file finder over a project. Indexing starts immediately on a
/// background thread; results are available while it runs.
pub struct FileFinder {
    root: PathBuf,
    nucleo: Nucleo<Arc<str>>,
    matcher: nucleo::Matcher,
    query: String,
    indexing: Arc<AtomicBool>,
    indexed: Arc<AtomicUsize>,
}

/// Walks `root` (respecting ignore files) and calls `f` with each file's
/// path relative to `root`. Runs on the calling thread using all cores.
pub fn walk_files(root: &Path, cancel: &AtomicBool, f: impl Fn(String) + Sync) {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| {
            !e.file_name()
                .to_str()
                .is_some_and(|name| ALWAYS_HIDDEN.contains(&name))
        })
        .build_parallel();
    walker.run(|| {
        let f = &f;
        Box::new(move |entry| {
            if cancel.load(Ordering::Relaxed) {
                return ignore::WalkState::Quit;
            }
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };
            if entry
                .file_type()
                .is_some_and(|t| t.is_file() || t.is_symlink())
                && let Ok(rel) = entry.path().strip_prefix(root)
            {
                let mut rel = rel.to_string_lossy().into_owned();
                if std::path::MAIN_SEPARATOR != '/' {
                    rel = rel.replace(std::path::MAIN_SEPARATOR, "/");
                }
                f(rel);
            }
            ignore::WalkState::Continue
        })
    });
}

impl FileFinder {
    /// Starts indexing `root`. `notify` is called (from any thread) whenever
    /// new results are ready; the UI should then call [`FileFinder::tick`].
    pub fn new(root: impl Into<PathBuf>, notify: impl Fn() + Send + Sync + 'static) -> Self {
        let root = root.into();
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(notify);
        let nucleo = Nucleo::new(Config::DEFAULT.match_paths(), notify.clone(), None, 1);
        let injector = nucleo.injector();
        let indexing = Arc::new(AtomicBool::new(true));
        let indexed = Arc::new(AtomicUsize::new(0));
        {
            let root = root.clone();
            let indexing = indexing.clone();
            let indexed = indexed.clone();
            std::thread::Builder::new()
                .name("file-index".into())
                .spawn(move || {
                    let cancel = AtomicBool::new(false);
                    walk_files(&root, &cancel, |rel| {
                        let rel: Arc<str> = rel.into();
                        injector.push(rel, |item, cols| cols[0] = item.as_ref().into());
                        indexed.fetch_add(1, Ordering::Relaxed);
                    });
                    indexing.store(false, Ordering::Release);
                    notify();
                })
                .expect("spawn file index thread");
        }
        Self {
            root,
            nucleo,
            matcher: nucleo::Matcher::new(Config::DEFAULT.match_paths()),
            query: String::new(),
            indexing,
            indexed,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_indexing(&self) -> bool {
        self.indexing.load(Ordering::Acquire)
    }

    pub fn file_count(&self) -> usize {
        self.indexed.load(Ordering::Relaxed)
    }

    /// Sets the query. Typing more characters is matched incrementally.
    pub fn set_query(&mut self, query: &str) {
        let (query, _) = split_line_suffix(query);
        let query = query.trim();
        if query == self.query {
            return;
        }
        let append = query.starts_with(self.query.as_str());
        self.nucleo
            .pattern
            .reparse(0, query, CaseMatching::Smart, Normalization::Smart, append);
        self.query = query.to_string();
    }

    /// Lets the matcher make progress for up to `timeout_ms`. Returns true if
    /// the results changed.
    pub fn tick(&mut self, timeout_ms: u64) -> bool {
        self.nucleo.tick(timeout_ms).changed
    }

    /// Number of files matching the current query.
    pub fn match_count(&self) -> usize {
        self.nucleo.snapshot().matched_item_count() as usize
    }

    /// The best `limit` matches with highlight indices.
    pub fn results(&mut self, limit: usize) -> Vec<FileMatch> {
        let snapshot = self.nucleo.snapshot();
        let n = snapshot.matched_item_count().min(limit as u32);
        let pattern = snapshot.pattern().column_pattern(0);
        let mut out = Vec::with_capacity(n as usize);
        let mut indices = Vec::new();
        for item in snapshot.matched_items(0..n) {
            indices.clear();
            pattern.indices(
                item.matcher_columns[0].slice(..),
                &mut self.matcher,
                &mut indices,
            );
            indices.sort_unstable();
            indices.dedup();
            out.push(FileMatch {
                path: item.data.clone(),
                indices: indices.clone(),
            });
        }
        out
    }

    /// Blocks until indexing has finished and matching settled. For tests
    /// and the CLI benchmark.
    pub fn wait(&mut self) {
        while self.is_indexing() {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        while self.nucleo.tick(10).running {}
    }
}

/// Splits `path:line[:col]` into the path and the 1-based line/column.
pub fn split_line_suffix(input: &str) -> (&str, Option<(u32, Option<u32>)>) {
    let mut parts = input.rsplitn(3, ':');
    let last = parts.next();
    let mid = parts.next();
    let first = parts.next();
    match (first, mid, last) {
        (Some(path), Some(line), Some(col)) => {
            if let (Ok(line), Ok(col)) = (line.parse(), col.parse()) {
                return (path, Some((line, Some(col))));
            }
        }
        _ => {}
    }
    if let Some((path, line)) = input.rsplit_once(':')
        && let Ok(line) = line.parse()
    {
        return (path, Some((line, None)));
    }
    (input, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn line_suffixes() {
        assert_eq!(split_line_suffix("src/a.rs"), ("src/a.rs", None));
        assert_eq!(
            split_line_suffix("src/a.rs:12"),
            ("src/a.rs", Some((12, None)))
        );
        assert_eq!(
            split_line_suffix("src/a.rs:12:3"),
            ("src/a.rs", Some((12, Some(3))))
        );
        assert_eq!(split_line_suffix("a:b"), ("a:b", None));
    }

    #[test]
    fn finds_files_fuzzily_and_respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src/editor")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        fs::write(root.join("src/editor/element.rs"), "").unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join("README.md"), "").unwrap();
        fs::write(root.join("target/debug/element.rs"), "").unwrap();
        fs::write(root.join(".git/config"), "").unwrap();

        let mut finder = FileFinder::new(root, || {});
        finder.wait();
        assert_eq!(finder.file_count(), 4); // .gitignore, element.rs, main.rs, README.md

        finder.set_query("edel");
        finder.wait();
        let results = finder.results(10);
        assert_eq!(results[0].path.as_ref(), "src/editor/element.rs");
        assert_eq!(results.len(), 1);
        assert!(!results[0].indices.is_empty());

        finder.set_query("readme:20");
        finder.wait();
        assert_eq!(finder.results(10)[0].path.as_ref(), "README.md");
    }
}
