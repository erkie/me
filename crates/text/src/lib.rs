//! The editor's text model, independent of any UI toolkit.
//!
//! * [`Document`]: a rope with line-ending detection and undo history.
//! * [`Selection`] / [`SelectionSet`]: one or many selections.
//! * [`EditorState`]: a document plus selections, with every editing and
//!   multi-cursor command.
//! * [`SearchQuery`] / [`Matcher`]: regex or literal search over the rope.

pub mod change;
pub mod document;
pub mod editor;
pub mod history;
pub mod movement;
pub mod search;
pub mod selection;
pub mod wrap;

pub use change::{Assoc, ByteEdit, Change, ChangeSet};
pub use document::{Document, LineEnding};
pub use editor::{ClipboardText, EditorState};
pub use history::EditKind;
pub use ropey::{Rope, RopeSlice};
pub use search::{Matcher, SearchQuery};
pub use selection::{Selection, SelectionSet};
