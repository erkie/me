//! Project-level services, independent of the UI:
//!
//! * [`tree`]: lazy, ignore-aware directory listing for the sidebar.
//! * [`finder`]: the Cmd+T fuzzy file finder.
//! * [`search`]: project-wide search and replace on ripgrep's engine.

pub mod finder;
pub mod search;
pub mod tree;

pub use finder::{FileFinder, FileMatch, split_line_suffix};
pub use search::{
    FileMatches, LineMatch, MatchRef, SearchOptions, SearchStats, replace_in_file, replace_in_text,
    search,
};
pub use tree::{DirEntry, list_dir};
