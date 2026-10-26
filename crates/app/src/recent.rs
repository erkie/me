//! Recently opened files and folders, for File > Recently Opened. Kept in
//! `<data dir>/recent.json`, most recent first.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

use crate::settings::data_dir;

/// How many entries the menu shows.
pub const LIMIT: usize = 15;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Recent {
    pub paths: Vec<PathBuf>,
}

impl Global for Recent {}

impl Recent {
    /// Moves `path` to the top, dropping the oldest past `LIMIT`.
    pub fn push(&mut self, path: PathBuf) {
        self.paths.retain(|p| *p != path);
        self.paths.insert(0, path);
        self.paths.truncate(LIMIT);
    }

    fn load_from(file: &Path) -> Self {
        std::fs::read(file)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn write_to(&self, file: &Path) -> std::io::Result<()> {
        let dir = file.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        tmp.write_all(&json)?;
        tmp.persist(file).map_err(|e| e.error)?;
        Ok(())
    }
}

fn file() -> PathBuf {
    data_dir().join("recent.json")
}

pub fn init(cx: &mut App) {
    cx.set_global(Recent::load_from(&file()));
}

/// Entries that still exist, most recent first.
pub fn paths(cx: &App) -> Vec<PathBuf> {
    cx.global::<Recent>()
        .paths
        .iter()
        .filter(|p| p.exists())
        .cloned()
        .collect()
}

/// Records opened files and folders and refreshes the menu.
pub fn note(opened: impl IntoIterator<Item = PathBuf>, cx: &mut App) {
    let mut changed = false;
    for path in opened {
        let path = std::path::absolute(&path).unwrap_or(path);
        cx.add_recent_document(&path);
        cx.global_mut::<Recent>().push(path);
        changed = true;
    }
    if changed {
        save_and_refresh(cx);
    }
}

/// Forgets one entry, such as a path that's gone.
pub fn forget(path: &Path, cx: &mut App) {
    cx.global_mut::<Recent>().paths.retain(|p| p != path);
    save_and_refresh(cx);
}

pub fn clear(cx: &mut App) {
    cx.global_mut::<Recent>().paths.clear();
    save_and_refresh(cx);
}

fn save_and_refresh(cx: &mut App) {
    if let Err(err) = cx.global::<Recent>().write_to(&file()) {
        eprintln!("me: could not save recently opened: {err}");
    }
    crate::menus::refresh(cx);
}

/// A path as shown in menus: `~/code/me` rather than `/Users/x/code/me`.
pub fn label(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        if rest.as_os_str().is_empty() {
            return "~".into();
        }
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_moves_to_top_and_caps() {
        let mut recent = Recent::default();
        for i in 0..LIMIT + 3 {
            recent.push(format!("/tmp/{i}").into());
        }
        assert_eq!(recent.paths.len(), LIMIT);
        assert_eq!(
            recent.paths[0],
            PathBuf::from(format!("/tmp/{}", LIMIT + 2))
        );
        recent.push("/tmp/5".into());
        assert_eq!(recent.paths[0], PathBuf::from("/tmp/5"));
        assert_eq!(recent.paths.len(), LIMIT);
        assert_eq!(
            recent
                .paths
                .iter()
                .filter(|p| **p == Path::new("/tmp/5"))
                .count(),
            1
        );
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nested/recent.json");
        assert_eq!(Recent::load_from(&file), Recent::default());
        let mut recent = Recent::default();
        recent.push("/a".into());
        recent.push("/b".into());
        recent.write_to(&file).unwrap();
        assert_eq!(Recent::load_from(&file), recent);
    }

    #[test]
    fn labels_abbreviate_home() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(label(&home.join("code/me")), "~/code/me");
        assert_eq!(label(&home), "~");
        assert_eq!(label(Path::new("/etc/hosts")), "/etc/hosts");
    }
}
