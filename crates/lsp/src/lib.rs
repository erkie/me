//! Language server support: the client side of the Language Server
//! Protocol, enough for go to definition and find references with the
//! standard servers (rust-analyzer, gopls, typescript-language-server,
//! pyright, clangd, …).
//!
//! [`Server`] runs one server process. [`registry`] knows which servers
//! exist for a language and finds them on the user's `PATH`. Positions
//! convert between char offsets in a rope and LSP's line/column pairs.

pub mod registry;
mod server;

use std::path::{Path, PathBuf};

pub use lsp_types as types;
use lsp_types::{Position, Uri};
use ropey::Rope;
pub use server::{Server, Status};

/// How a server counts columns: UTF-16 code units (the LSP default) or
/// bytes, if it agreed to UTF-8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

pub fn uri_from_path(path: &Path) -> Option<Uri> {
    url::Url::from_file_path(path).ok()?.as_str().parse().ok()
}

pub fn path_from_uri(uri: &Uri) -> Option<PathBuf> {
    url::Url::parse(uri.as_str()).ok()?.to_file_path().ok()
}

/// The LSP position of char offset `offset`.
pub fn position(rope: &Rope, offset: usize, encoding: Encoding) -> Position {
    let offset = offset.min(rope.len_chars());
    let line = rope.char_to_line(offset);
    let start = rope.line_to_char(line);
    let character = match encoding {
        Encoding::Utf16 => rope.char_to_utf16_cu(offset) - rope.char_to_utf16_cu(start),
        Encoding::Utf8 => rope.char_to_byte(offset) - rope.char_to_byte(start),
    };
    Position::new(line as u32, character as u32)
}

/// The char offset of an LSP position, clamped to its line.
pub fn offset(rope: &Rope, position: Position, encoding: Encoding) -> usize {
    let line = (position.line as usize).min(rope.len_lines().saturating_sub(1));
    let start = rope.line_to_char(line);
    let mut end = start + rope.line(line).len_chars();
    while end > start && matches!(rope.char(end - 1), '\n' | '\r') {
        end -= 1;
    }
    let character = position.character as usize;
    let offset = match encoding {
        Encoding::Utf16 => {
            let cu = (rope.char_to_utf16_cu(start) + character).min(rope.len_utf16_cu());
            rope.utf16_cu_to_char(cu)
        }
        Encoding::Utf8 => {
            let byte = (rope.char_to_byte(start) + character).min(rope.len_bytes());
            rope.byte_to_char(byte)
        }
    };
    offset.clamp(start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_round_trip() {
        // "é" is one UTF-16 unit and two bytes; "😀" is two units, four bytes.
        let rope = Rope::from_str("let é = \"😀x\";\r\nnext\n");
        let x = "let é = \"😀".chars().count();
        assert_eq!(position(&rope, x, Encoding::Utf16), Position::new(0, 11));
        assert_eq!(position(&rope, x, Encoding::Utf8), Position::new(0, 14));
        assert_eq!(offset(&rope, Position::new(0, 11), Encoding::Utf16), x);
        assert_eq!(offset(&rope, Position::new(0, 14), Encoding::Utf8), x);
        let next = rope.line_to_char(1) + 2;
        assert_eq!(position(&rope, next, Encoding::Utf16), Position::new(1, 2));
        assert_eq!(offset(&rope, Position::new(1, 2), Encoding::Utf16), next);
    }

    #[test]
    fn offsets_clamp_to_the_line() {
        let rope = Rope::from_str("ab\r\ncd");
        assert_eq!(offset(&rope, Position::new(0, 99), Encoding::Utf16), 2);
        assert_eq!(offset(&rope, Position::new(9, 1), Encoding::Utf8), 5);
        assert_eq!(offset(&rope, Position::new(1, 99), Encoding::Utf8), 6);
    }

    #[test]
    fn file_uris() {
        let path = if cfg!(windows) {
            PathBuf::from(r"C:\code\my app\main.rs")
        } else {
            PathBuf::from("/code/my app/main.rs")
        };
        let uri = uri_from_path(&path).unwrap();
        assert!(uri.as_str().ends_with("/code/my%20app/main.rs"));
        assert_eq!(path_from_uri(&uri), Some(path));
    }
}
