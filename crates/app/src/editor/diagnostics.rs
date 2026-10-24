//! Problems a language server reported in a buffer. They're kept in byte
//! offsets and moved along with each edit, so squiggles stay on their code
//! until the server sends fresh ones.

use std::ops::Range;

use me_text::ByteEdit;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    /// Bytes of the buffer.
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
    /// Who reported it ("rustc", "Pyright") and its code ("E0308").
    pub source: Option<String>,
    pub code: Option<String>,
}

/// Moves diagnostics through edits (in the order they were applied).
/// Text typed at the start of a diagnostic goes before it, at its end after
/// it; a diagnostic whose text was replaced covers the replacement.
pub fn map_through(diagnostics: &mut [Diagnostic], edits: &[ByteEdit]) {
    for edit in edits {
        for d in diagnostics.iter_mut() {
            let start = map(d.range.start, edit, false);
            let end = map(d.range.end, edit, true).max(start);
            d.range = start..end;
        }
    }
}

fn map(pos: usize, edit: &ByteEdit, is_end: bool) -> usize {
    let insertion = edit.old_end_byte == edit.start_byte;
    if pos < edit.start_byte || (pos == edit.start_byte && (is_end || !insertion)) {
        pos
    } else if pos < edit.old_end_byte {
        if is_end {
            edit.new_end_byte
        } else {
            edit.start_byte
        }
    } else {
        pos - edit.old_end_byte + edit.new_end_byte
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(start: usize, old_end: usize, new_end: usize) -> ByteEdit {
        ByteEdit {
            start_byte: start,
            old_end_byte: old_end,
            new_end_byte: new_end,
            start_point: (0, start),
            old_end_point: (0, old_end),
            new_end_point: (0, new_end),
        }
    }

    fn diagnostic(range: Range<usize>) -> Diagnostic {
        Diagnostic {
            range,
            severity: Severity::Error,
            message: String::new(),
            source: None,
            code: None,
        }
    }

    #[test]
    fn diagnostics_follow_edits() {
        // "let x = foo;" with `foo` (8..11) flagged.
        let mut ds = vec![diagnostic(8..11)];
        // Typing before it pushes it right.
        map_through(&mut ds, &[edit(0, 0, 4)]);
        assert_eq!(ds[0].range, 12..15);
        // Typing right at its start goes before it, at its end after it.
        map_through(&mut ds, &[edit(12, 12, 13)]);
        assert_eq!(ds[0].range, 13..16);
        map_through(&mut ds, &[edit(16, 16, 18)]);
        assert_eq!(ds[0].range, 13..16);
        // Typing inside grows it.
        map_through(&mut ds, &[edit(14, 14, 15)]);
        assert_eq!(ds[0].range, 13..17);
        // Replacing text that overlaps it keeps it on the replacement.
        map_through(&mut ds, &[edit(10, 15, 11)]);
        assert_eq!(ds[0].range, 10..13);
        // Deleting all of it leaves an empty range where it was.
        map_through(&mut ds, &[edit(9, 14, 9)]);
        assert_eq!(ds[0].range, 9..9);
        // Edits after it don't move it.
        map_through(&mut ds, &[edit(20, 25, 21)]);
        assert_eq!(ds[0].range, 9..9);
    }
}
