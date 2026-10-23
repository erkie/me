//! Lazy directory listing for the sidebar file tree.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Names never shown in the tree.
pub const ALWAYS_HIDDEN: &[&str] = &[".git", ".hg", ".svn", ".DS_Store", "Thumbs.db"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// Matched by a `.gitignore` (or similar). Shown dimmed.
    pub ignored: bool,
}

/// Lists one directory: folders first, then files, each in natural order
/// ("file2" before "file10", case-insensitive). Entries matched by ignore
/// files are included and flagged so the tree can dim them.
pub fn list_dir(dir: &Path) -> io::Result<Vec<DirEntry>> {
    let not_ignored = not_ignored_names(dir);
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if ALWAYS_HIDDEN.contains(&name.as_str()) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else { continue };
        let path = entry.path();
        let is_symlink = file_type.is_symlink();
        let is_dir = if is_symlink { path.is_dir() } else { file_type.is_dir() };
        let ignored = not_ignored.as_ref().is_some_and(|set| !set.contains(&name));
        entries.push(DirEntry { name, path, is_dir, is_symlink, ignored });
    }
    entries.sort_by(compare_entries);
    Ok(entries)
}

/// Names in `dir` that ignore rules keep. `None` if the walker failed (then
/// nothing is flagged).
fn not_ignored_names(dir: &Path) -> Option<HashSet<String>> {
    let walker = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .parents(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .build();
    let mut names = HashSet::new();
    for entry in walker {
        let entry = entry.ok()?;
        if entry.depth() == 1 {
            names.insert(entry.file_name().to_string_lossy().into_owned());
        }
    }
    Some(names)
}

fn compare_entries(a: &DirEntry, b: &DirEntry) -> Ordering {
    b.is_dir.cmp(&a.is_dir).then_with(|| natural_cmp(&a.name, &b.name))
}

/// Case-insensitive comparison that orders digit runs numerically.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na: String = std::iter::from_fn(|| ai.next_if(char::is_ascii_digit)).collect();
                let nb: String = std::iter::from_fn(|| bi.next_if(char::is_ascii_digit)).collect();
                let ta = na.trim_start_matches('0');
                let tb = nb.trim_start_matches('0');
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut names = vec!["file10", "File2", "file1", "a", "B", "file02b"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["a", "B", "file1", "File2", "file02b", "file10"]);
    }

    #[test]
    fn lists_dirs_first_and_flags_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("src")).unwrap();
        fs::create_dir(root.join("target")).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(root.join("README.md"), "hi").unwrap();
        fs::write(root.join("debug.log"), "x").unwrap();

        let entries = list_dir(root).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["src", "target", ".gitignore", "debug.log", "README.md"]);
        let ignored: Vec<_> = entries.iter().filter(|e| e.ignored).map(|e| &e.name).collect();
        assert_eq!(ignored, vec!["target", "debug.log"]);
    }
}
