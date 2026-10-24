//! The project symbol index: every definition in the folder's code, found
//! with the grammars' tags queries. It answers go to definition and project
//! symbol search without a language server, for every language we highlight.
//!
//! Built on a background thread the first time a window needs it, and
//! refreshed (changed files only) when the window regains focus or a file
//! is saved.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use me_syntax::{SymbolKind, language_for_path};

/// Files bigger than this are skipped (generated code, bundles).
const MAX_FILE_SIZE: u64 = 1024 * 1024;
/// Stop indexing huge trees after this many files.
const MAX_FILES: usize = 30_000;

#[derive(Clone, Debug)]
pub struct Location {
    pub path: PathBuf,
    pub name: Arc<str>,
    pub kind: SymbolKind,
    /// 0-based line and char column of the name.
    pub line: usize,
    pub col: usize,
}

struct Entry {
    name: Arc<str>,
    kind: SymbolKind,
    line: u32,
    col: u32,
}

struct File {
    modified: Option<SystemTime>,
    symbols: Vec<Entry>,
}

pub struct SymbolIndex {
    root: PathBuf,
    files: Mutex<HashMap<PathBuf, File>>,
    indexing: AtomicBool,
    /// At least one full pass finished.
    ready: AtomicBool,
}

/// Whether the index covers this file: code with a tags query. Markdown
/// headings stay per file.
pub fn indexable(path: &Path) -> bool {
    language_for_path(path).is_some_and(|def| def.has_symbols() && def.name != "Markdown")
}

impl SymbolIndex {
    pub fn new(root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            root,
            files: Mutex::new(HashMap::new()),
            indexing: AtomicBool::new(false),
            ready: AtomicBool::new(false),
        })
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    pub fn is_indexing(&self) -> bool {
        self.indexing.load(Ordering::Relaxed)
    }

    /// Indexes new and changed files (all of them the first time) on a
    /// background thread. Does nothing if a pass is already running.
    pub fn refresh(self: &Arc<Self>, done: impl FnOnce() + Send + 'static) {
        if self.indexing.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = self.clone();
        std::thread::Builder::new()
            .name("symbol-index".into())
            .spawn(move || {
                this.index_changed();
                this.ready.store(true, Ordering::Relaxed);
                this.indexing.store(false, Ordering::Release);
                done();
            })
            .ok();
    }

    fn index_changed(&self) {
        let mut seen = Vec::new();
        let mut todo = Vec::new();
        let walker = ignore::WalkBuilder::new(&self.root)
            .hidden(true)
            .git_global(false)
            .build();
        for entry in walker.flatten() {
            if seen.len() >= MAX_FILES {
                break;
            }
            let path = entry.path();
            if !entry.file_type().is_some_and(|t| t.is_file()) || !indexable(path) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.len() > MAX_FILE_SIZE {
                continue;
            }
            let modified = meta.modified().ok();
            seen.push(path.to_path_buf());
            let unchanged = self
                .files
                .lock()
                .unwrap()
                .get(path)
                .is_some_and(|f| f.modified == modified && modified.is_some());
            if !unchanged {
                todo.push((path.to_path_buf(), modified));
            }
        }
        // Forget files that are gone.
        {
            let seen: std::collections::HashSet<&PathBuf> = seen.iter().collect();
            self.files.lock().unwrap().retain(|p, _| seen.contains(p));
        }
        let queue = Mutex::new(todo.into_iter());
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(1, 8);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        let next = queue.lock().unwrap().next();
                        let Some((path, modified)) = next else { break };
                        if let Ok(text) = std::fs::read_to_string(&path) {
                            self.insert(path, modified, &text);
                        }
                    }
                });
            }
        });
    }

    fn insert(&self, path: PathBuf, modified: Option<SystemTime>, text: &str) {
        let Some(def) = language_for_path(&path) else {
            return;
        };
        let symbols = me_syntax::symbols_in(def, text)
            .into_iter()
            .map(|s| Entry {
                name: Arc::from(s.name),
                kind: s.kind,
                line: s.line as u32,
                col: s.col as u32,
            })
            .collect();
        self.files
            .lock()
            .unwrap()
            .insert(path, File { modified, symbols });
    }

    /// Re-indexes one file from its new contents (after a save).
    pub fn update(&self, path: &Path, text: &str) {
        if !indexable(path) || !path.starts_with(&self.root) {
            return;
        }
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        self.insert(path.to_path_buf(), modified, text);
    }

    /// Definitions named exactly `name`.
    pub fn lookup(&self, name: &str) -> Vec<Location> {
        let files = self.files.lock().unwrap();
        let mut found = Vec::new();
        for (path, file) in files.iter() {
            for s in file.symbols.iter().filter(|s| &*s.name == name) {
                found.push(Location {
                    path: path.clone(),
                    name: s.name.clone(),
                    kind: s.kind,
                    line: s.line as usize,
                    col: s.col as usize,
                });
            }
        }
        found
    }

    /// Every definition (for project symbol search), up to `limit`.
    pub fn all(&self, limit: usize) -> Vec<Location> {
        let files = self.files.lock().unwrap();
        let mut paths: Vec<&PathBuf> = files.keys().collect();
        paths.sort();
        let mut out = Vec::new();
        for path in paths {
            for s in &files[path].symbols {
                if s.kind == SymbolKind::Impl {
                    continue;
                }
                if out.len() >= limit {
                    return out;
                }
                out.push(Location {
                    path: path.clone(),
                    name: s.name.clone(),
                    kind: s.kind,
                    line: s.line as usize,
                    col: s.col as usize,
                });
            }
        }
        out
    }
}

/// Orders candidate definitions for `from`: the same file first, then the
/// same folder, then nearer folders.
pub fn rank(candidates: &mut [Location], from: Option<&Path>) {
    let from_dir = from.and_then(Path::parent);
    candidates.sort_by_key(|loc| {
        let same_file = Some(loc.path.as_path()) == from;
        let same_dir = loc.path.parent() == from_dir;
        let shared = from_dir
            .map(|d| {
                d.components()
                    .zip(loc.path.components())
                    .take_while(|(a, b)| a == b)
                    .count()
            })
            .unwrap_or(0);
        (
            !same_file,
            !same_dir,
            std::cmp::Reverse(shared),
            loc.kind == SymbolKind::Impl,
            loc.path.clone(),
            loc.line,
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_and_finds_definitions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/util")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() { helper(); }\n").unwrap();
        std::fs::write(
            root.join("src/util/mod.rs"),
            "pub fn helper() {}\npub struct Thing;\n",
        )
        .unwrap();
        std::fs::write(root.join("notes.md"), "# helper\n").unwrap();
        let index = SymbolIndex::new(root.to_path_buf());
        index.index_changed();
        let found = index.lookup("helper");
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("src/util/mod.rs"));
        assert_eq!((found[0].line, found[0].col), (0, 7));
        assert_eq!(index.all(100).len(), 3);

        // Changed files are picked up; unchanged ones aren't re-read.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(root.join("src/util/mod.rs"), "pub fn other() {}\n").unwrap();
        index.index_changed();
        assert!(index.lookup("helper").is_empty());
        assert_eq!(index.lookup("other").len(), 1);
    }

    #[test]
    fn ranks_nearby_definitions_first() {
        let loc = |path: &str| Location {
            path: path.into(),
            name: Arc::from("x"),
            kind: SymbolKind::Function,
            line: 0,
            col: 0,
        };
        let mut found = vec![loc("/p/b/z.rs"), loc("/p/a/y.rs"), loc("/p/a/x.rs")];
        rank(&mut found, Some(Path::new("/p/a/x.rs")));
        let order: Vec<&str> = found.iter().map(|l| l.path.to_str().unwrap()).collect();
        assert_eq!(order, vec!["/p/a/x.rs", "/p/a/y.rs", "/p/b/z.rs"]);
    }
}
