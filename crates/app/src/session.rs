//! Window sessions: open tabs, cursors and unsaved text, kept on disk so
//! closing a window or quitting never loses work and never has to ask.
//!
//! Each window has a key: `folder-<hash>` for a folder (so reopening the
//! folder restores it) or `scratch-<n>` for a window without one. A session
//! lives in `<data dir>/sessions/<key>/` as `session.json` plus one
//! `buffer-<n>.txt` per tab with unsaved text.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use me_text::Rope;
use serde::{Deserialize, Serialize};

use crate::settings::data_dir;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSession {
    pub root: Option<PathBuf>,
    /// The window was open when the app last quit (or crashed): it comes
    /// back on the next plain launch.
    pub open: bool,
    pub tabs: Vec<TabSession>,
    pub active: Option<usize>,
    pub sidebar_visible: bool,
    pub sidebar_width: f32,
    /// x, y, width, height of a normal (not maximized) window.
    pub bounds: Option<[f32; 4]>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TabSession {
    pub path: Option<PathBuf>,
    /// Buffer file with unsaved text, relative to the session directory.
    pub unsaved: Option<String>,
    pub pinned: bool,
    pub show_preview: bool,
    /// Char offset of the cursor.
    pub cursor: usize,
    /// Scroll position, a fractional line.
    pub scroll: f64,
}

impl Default for WindowSession {
    fn default() -> Self {
        Self {
            root: None,
            open: false,
            tabs: Vec::new(),
            active: None,
            sidebar_visible: true,
            sidebar_width: 240.,
            bounds: None,
        }
    }
}

impl WindowSession {
    pub fn has_unsaved(&self) -> bool {
        self.tabs.iter().any(|t| t.unsaved.is_some())
    }
}

/// A session ready to write: the description plus the text of buffers that
/// changed since they were last written (ropes clone cheaply).
pub struct Snapshot {
    pub session: WindowSession,
    pub buffers: Vec<(String, Rope)>,
}

fn sessions_dir() -> PathBuf {
    data_dir().join("sessions")
}

fn dir_for(key: &str) -> PathBuf {
    sessions_dir().join(key)
}

/// The key of a folder's session: stable across runs.
pub fn key_for_root(root: &Path) -> String {
    // FNV-1a: tiny and stable, unlike std's hasher.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in root.to_string_lossy().bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let name: String = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(32)
        .collect();
    format!("folder-{name}-{hash:016x}")
}

pub fn new_scratch_key() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("scratch-{nanos}")
}

pub fn is_scratch(key: &str) -> bool {
    key.starts_with("scratch-")
}

/// Writes a session, replacing the previous one. Buffers not mentioned by
/// the session are removed; unchanged ones are kept as they are.
pub fn write(key: &str, snapshot: &Snapshot) -> std::io::Result<()> {
    let dir = dir_for(key);
    std::fs::create_dir_all(&dir)?;
    for (name, rope) in &snapshot.buffers {
        let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
        let mut writer = std::io::BufWriter::new(tmp.as_file_mut());
        rope.write_to(&mut writer)?;
        writer.flush()?;
        drop(writer);
        tmp.persist(dir.join(name)).map_err(|e| e.error)?;
    }
    let json = serde_json::to_vec_pretty(&snapshot.session).map_err(std::io::Error::other)?;
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(&json)?;
    tmp.persist(dir.join("session.json")).map_err(|e| e.error)?;
    // Drop buffers of tabs that are gone or saved since.
    let keep: Vec<&str> = snapshot
        .session
        .tabs
        .iter()
        .filter_map(|t| t.unsaved.as_deref())
        .collect();
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("buffer-") && !keep.contains(&name.as_str()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

pub fn load(key: &str) -> Option<WindowSession> {
    let bytes = std::fs::read(dir_for(key).join("session.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The unsaved text a tab session points at.
pub fn read_buffer(key: &str, name: &str) -> Option<String> {
    std::fs::read_to_string(dir_for(key).join(name)).ok()
}

pub fn delete(key: &str) {
    let _ = std::fs::remove_dir_all(dir_for(key));
}

/// Every stored session, oldest first.
pub fn list() -> Vec<(String, WindowSession)> {
    let Ok(entries) = std::fs::read_dir(sessions_dir()) else {
        return Vec::new();
    };
    let mut sessions: Vec<(std::time::SystemTime, String, WindowSession)> = entries
        .flatten()
        .filter_map(|entry| {
            let key = entry.file_name().to_string_lossy().into_owned();
            let modified = entry
                .path()
                .join("session.json")
                .metadata()
                .and_then(|m| m.modified())
                .ok()?;
            Some((modified, key.clone(), load(&key)?))
        })
        .collect();
    sessions.sort_by_key(|(modified, _, _)| *modified);
    sessions.into_iter().map(|(_, k, s)| (k, s)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_keys_are_stable_and_distinct() {
        let a = key_for_root(Path::new("/Users/me/code/app"));
        assert_eq!(a, key_for_root(Path::new("/Users/me/code/app")));
        assert_ne!(a, key_for_root(Path::new("/Users/me/code/app2")));
        assert!(a.starts_with("folder-app-"));
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: tests in this module are the only users of ME_DATA_DIR.
        unsafe { std::env::set_var("ME_DATA_DIR", dir.path()) };
        let session = WindowSession {
            root: Some("/tmp/x".into()),
            open: true,
            tabs: vec![
                TabSession {
                    path: Some("/tmp/x/a.rs".into()),
                    unsaved: Some("buffer-1.txt".into()),
                    cursor: 3,
                    ..Default::default()
                },
                TabSession {
                    path: Some("/tmp/x/b.rs".into()),
                    ..Default::default()
                },
            ],
            active: Some(1),
            ..Default::default()
        };
        let snapshot = Snapshot {
            session,
            buffers: vec![("buffer-1.txt".into(), Rope::from_str("unsaved text"))],
        };
        write("folder-test", &snapshot).unwrap();
        std::fs::write(dir_for("folder-test").join("buffer-9.txt"), "stale").unwrap();
        write(
            "folder-test",
            &Snapshot {
                buffers: vec![],
                ..snapshot
            },
        )
        .unwrap();
        let loaded = load("folder-test").unwrap();
        assert_eq!(loaded.tabs.len(), 2);
        assert_eq!(loaded.active, Some(1));
        assert_eq!(
            read_buffer("folder-test", "buffer-1.txt").as_deref(),
            Some("unsaved text")
        );
        assert!(read_buffer("folder-test", "buffer-9.txt").is_none());
        assert_eq!(list().len(), 1);
        delete("folder-test");
        assert!(load("folder-test").is_none());
    }
}
