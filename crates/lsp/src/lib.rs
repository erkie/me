//! Language server support: the client side of the Language Server
//! Protocol, enough for go to definition, references, hover, rename and
//! diagnostics with the standard servers (rust-analyzer, gopls,
//! typescript-language-server, pyright, clangd, …).
//!
//! [`Server`] runs one server process. [`registry`] knows which servers
//! exist for a language and finds them on the user's `PATH`. Positions
//! convert between char offsets in a rope and LSP's line/column pairs.

pub mod registry;
mod server;

use std::ops::Range;
use std::path::{Path, PathBuf};

pub use lsp_types as types;
use lsp_types::{Position, Uri};
use ropey::Rope;
pub use server::{Event, Server, Status};

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
    // Up to the line break (`\n` or `\r\n`; a lone `\r` is text, as in
    // the editor).
    let mut end = start + rope.line(line).len_chars();
    if end > start && rope.char(end - 1) == '\n' {
        end -= 1;
        if end > start && rope.char(end - 1) == '\r' {
            end -= 1;
        }
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

/// The char range of an LSP range.
pub fn char_range(rope: &Rope, range: lsp_types::Range, encoding: Encoding) -> Range<usize> {
    let start = offset(rope, range.start, encoding);
    start..offset(rope, range.end, encoding).max(start)
}

/// The edit that turns `old` into `new`: the range of `old` between the
/// text they start and end with, and what replaces it. `None` if they're
/// the same. Ranges never split a char or a `\r\n`.
pub fn diff(old: &Rope, new: &Rope, encoding: Encoding) -> Option<(lsp_types::Range, String)> {
    let (old_len, new_len) = (old.len_bytes(), new.len_bytes());
    let mut prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    if prefix == old_len && prefix == new_len {
        return None;
    }
    let suffix = old
        .bytes_at(old_len)
        .reversed()
        .zip(new.bytes_at(new_len).reversed())
        .take(old_len.min(new_len) - prefix)
        .take_while(|(a, b)| a == b)
        .count();
    // Whether a byte starts a char depends on that byte alone, so a
    // boundary in `old` is one in `new` too.
    prefix = old.char_to_byte(old.byte_to_char(prefix));
    if prefix > 0 && old.byte(prefix - 1) == b'\r' {
        prefix -= 1;
    }
    let mut old_end = old_len - suffix;
    let end_char = old.byte_to_char(old_end);
    if old.char_to_byte(end_char) != old_end {
        old_end = old.char_to_byte(end_char + 1);
    }
    if old_end > prefix
        && old_end < old_len
        && old.byte(old_end) == b'\n'
        && old.byte(old_end - 1) == b'\r'
    {
        old_end += 1;
    }
    let new_end = new_len - (old_len - old_end);
    let range = lsp_types::Range::new(
        position(old, old.byte_to_char(prefix), encoding),
        position(old, old.byte_to_char(old_end), encoding),
    );
    Some((range, new.byte_slice(prefix..new_end).to_string()))
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

    /// Applies an LSP edit the way a server would.
    fn apply(rope: &mut Rope, range: lsp_types::Range, text: &str, encoding: Encoding) {
        let range = char_range(rope, range, encoding);
        rope.remove(range.clone());
        rope.insert(range.start, text);
    }

    #[test]
    fn diffs_reproduce_the_new_text() {
        let base = "fn main() {\r\n    let é = \"😀\";\r\n}\nnext line\n";
        let pieces = ["", "x", "é", "😀", "\n", "\r\n", "\r", "ab\ncd"];
        let mut seed: u64 = 7;
        let mut next = |n: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize % n.max(1)
        };
        for encoding in [Encoding::Utf16, Encoding::Utf8] {
            let mut text = Rope::from_str(base);
            for _ in 0..400 {
                let old = text.clone();
                let len = text.len_chars();
                let start = next(len + 1);
                let end = (start + next(4)).min(len);
                text.remove(start..end);
                text.insert(start, pieces[next(pieces.len())]);
                let mut server = old.clone();
                match diff(&old, &text, encoding) {
                    Some((range, inserted)) => {
                        apply(&mut server, range, &inserted, encoding);
                        assert_eq!(server, text, "{old:?} -> {text:?}: {range:?} {inserted:?}");
                    }
                    None => assert_eq!(old, text),
                }
            }
        }
    }

    #[test]
    fn diffs_are_small() {
        let old = Rope::from_str("one\ntwo\nthree\n");
        let new = Rope::from_str("one\ntwo!\nthree\n");
        let (range, text) = diff(&old, &new, Encoding::Utf16).unwrap();
        assert_eq!(
            range,
            lsp_types::Range::new(Position::new(1, 3), Position::new(1, 3))
        );
        assert_eq!(text, "!");
        assert!(diff(&old, &old.clone(), Encoding::Utf16).is_none());
        // Never between \r and \n.
        let old = Rope::from_str("a\r\nb");
        let new = Rope::from_str("a\r\r\nb");
        let (range, text) = diff(&old, &new, Encoding::Utf8).unwrap();
        let mut server = old.clone();
        apply(&mut server, range, &text, Encoding::Utf8);
        assert_eq!(server, new);
        assert_eq!(range.start, Position::new(0, 1));
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
