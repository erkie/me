//! In-document search over the rope, without copying it into a string.

use std::ops::Range;

use regex_cursor::engines::meta::Regex;
use regex_cursor::regex_automata::PatternID;
use regex_cursor::regex_automata::util::{interpolate, syntax};
use regex_cursor::{Input, RopeyCursor};
use ropey::Rope;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SearchQuery {
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

impl SearchQuery {
    pub fn literal(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            case_sensitive: true,
            ..Default::default()
        }
    }

    pub fn whole_word(mut self, yes: bool) -> Self {
        self.whole_word = yes;
        self
    }

    pub fn case_sensitive(mut self, yes: bool) -> Self {
        self.case_sensitive = yes;
        self
    }

    pub fn regex(mut self, yes: bool) -> Self {
        self.regex = yes;
        self
    }

    pub fn compile(&self) -> Result<Matcher, String> {
        Matcher::new(self)
    }
}

/// A compiled [`SearchQuery`].
pub struct Matcher {
    regex: Regex,
}

impl Matcher {
    pub fn new(query: &SearchQuery) -> Result<Self, String> {
        if query.pattern.is_empty() {
            return Err("empty pattern".into());
        }
        let mut pattern = if query.regex {
            query.pattern.clone()
        } else {
            regex_syntax::escape(&query.pattern)
        };
        if query.whole_word {
            // A word boundary only makes sense next to a word character: a
            // literal like `//!` or `->` would never match with `\b` around it.
            let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            let (before, after) = if query.regex {
                (true, true)
            } else {
                (
                    is_word(query.pattern.chars().next()),
                    is_word(query.pattern.chars().last()),
                )
            };
            pattern = format!(
                "{}(?:{pattern}){}",
                if before { r"\b" } else { "" },
                if after { r"\b" } else { "" }
            );
        }
        let regex = Regex::builder()
            .syntax(
                syntax::Config::new()
                    .case_insensitive(!query.case_sensitive)
                    .multi_line(true)
                    .crlf(true),
            )
            .build(&pattern)
            .map_err(|e| e.to_string())?;
        Ok(Self { regex })
    }

    /// All matches inside the char range `within`, as char ranges.
    /// Empty matches are skipped.
    pub fn find_all(&self, rope: &Rope, within: Range<usize>, limit: usize) -> Vec<Range<usize>> {
        let start_byte = rope.char_to_byte(within.start.min(rope.len_chars()));
        let end_byte = rope.char_to_byte(within.end.min(rope.len_chars()));
        let input = Input::new(RopeyCursor::new(rope.slice(..))).range(start_byte..end_byte);
        let mut out = Vec::new();
        for m in self.regex.find_iter(input) {
            if m.start() == m.end() {
                continue;
            }
            out.push(rope.byte_to_char(m.start())..rope.byte_to_char(m.end()));
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    /// Every match in `within` paired with its replacement text. In regex
    /// mode, `$1`, `${name}` and `$$` in `replacement` expand to capture
    /// groups.
    pub fn replacements(
        &self,
        rope: &Rope,
        within: Range<usize>,
        replacement: &str,
        regex: bool,
    ) -> Vec<(Range<usize>, String)> {
        if !regex {
            return self
                .find_all(rope, within, usize::MAX)
                .into_iter()
                .map(|range| (range, replacement.to_string()))
                .collect();
        }
        let start_byte = rope.char_to_byte(within.start.min(rope.len_chars()));
        let end_byte = rope.char_to_byte(within.end.min(rope.len_chars()));
        let input = Input::new(RopeyCursor::new(rope.slice(..))).range(start_byte..end_byte);
        let mut out = Vec::new();
        for caps in self.regex.captures_iter(input) {
            let Some(m) = caps.get_match() else { continue };
            if m.start() == m.end() {
                continue;
            }
            let mut text = String::new();
            interpolate::string(
                replacement,
                |index, dst| {
                    if let Some(span) = caps.get_group(index) {
                        dst.extend(rope.byte_slice(span.start..span.end).chunks());
                    }
                },
                |name| caps.group_info().to_index(PatternID::ZERO, name),
                &mut text,
            );
            out.push((
                rope.byte_to_char(m.start())..rope.byte_to_char(m.end()),
                text,
            ));
        }
        out
    }

    /// First match starting at or after `from`, wrapping around to the start.
    pub fn find_next(&self, rope: &Rope, from: usize) -> Option<Range<usize>> {
        let len = rope.len_chars();
        self.first_in(rope, from.min(len)..len)
            .or_else(|| self.first_in(rope, 0..from.min(len)))
    }

    /// Last match ending at or before `before`, wrapping around to the end.
    pub fn find_prev(&self, rope: &Rope, before: usize) -> Option<Range<usize>> {
        let len = rope.len_chars();
        let before = before.min(len);
        self.last_in(rope, 0..before)
            .or_else(|| self.last_in(rope, before..len))
    }

    fn first_in(&self, rope: &Rope, within: Range<usize>) -> Option<Range<usize>> {
        self.find_all(rope, within, 1).into_iter().next()
    }

    fn last_in(&self, rope: &Rope, within: Range<usize>) -> Option<Range<usize>> {
        // Search backwards in growing windows so a match near `within.end`
        // is found without scanning the whole document.
        let mut window = 64 * 1024;
        loop {
            let start = within.end.saturating_sub(window).max(within.start);
            let found = self.find_all(rope, start..within.end, usize::MAX);
            // A match that starts exactly at the window edge might extend
            // further back; only trust windows that reach the range start or
            // whose last match starts past the window edge.
            if let Some(last) = found.last()
                && (start == within.start || last.start > start)
            {
                return Some(last.clone());
            }
            if start == within.start {
                return None;
            }
            window *= 4;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_case_and_word() {
        let rope = Rope::from_str("Foo foo food (foo)\nfoo");
        let m = SearchQuery::literal("foo").compile().unwrap();
        assert_eq!(m.find_all(&rope, 0..rope.len_chars(), 100).len(), 4);
        let m = SearchQuery::literal("foo")
            .whole_word(true)
            .compile()
            .unwrap();
        assert_eq!(
            m.find_all(&rope, 0..rope.len_chars(), 100),
            vec![4..7, 14..17, 19..22]
        );
        let m = SearchQuery::literal("foo")
            .case_sensitive(false)
            .whole_word(true)
            .compile()
            .unwrap();
        assert_eq!(m.find_all(&rope, 0..rope.len_chars(), 100).len(), 4);
    }

    #[test]
    fn whole_word_with_punctuation_needles() {
        let rope = Rope::from_str("//! a\n//!\nx//!y -> b->c");
        let m = SearchQuery::literal("//!")
            .whole_word(true)
            .compile()
            .unwrap();
        assert_eq!(m.find_all(&rope, 0..rope.len_chars(), 100).len(), 3);
        let m = SearchQuery::literal("->")
            .whole_word(true)
            .compile()
            .unwrap();
        assert_eq!(m.find_all(&rope, 0..rope.len_chars(), 100).len(), 2);
    }

    #[test]
    fn special_chars_are_literal() {
        let rope = Rope::from_str("a.b a*b (a.b)");
        let m = SearchQuery::literal("a.b").compile().unwrap();
        assert_eq!(
            m.find_all(&rope, 0..rope.len_chars(), 100),
            vec![0..3, 9..12]
        );
    }

    #[test]
    fn next_and_prev_wrap() {
        let rope = Rope::from_str("x ab x ab x");
        let m = SearchQuery::literal("ab").compile().unwrap();
        assert_eq!(m.find_next(&rope, 0), Some(2..4));
        assert_eq!(m.find_next(&rope, 3), Some(7..9));
        assert_eq!(m.find_next(&rope, 8), Some(2..4));
        assert_eq!(m.find_prev(&rope, 7), Some(2..4));
        assert_eq!(m.find_prev(&rope, 2), Some(7..9));
    }

    #[test]
    fn multibyte_offsets_are_chars() {
        let rope = Rope::from_str("åäö ÅÄÖ åäö");
        let m = SearchQuery::literal("åäö")
            .case_sensitive(false)
            .compile()
            .unwrap();
        assert_eq!(
            m.find_all(&rope, 0..rope.len_chars(), 100),
            vec![0..3, 4..7, 8..11]
        );
    }

    #[test]
    fn regex_line_anchors() {
        let rope = Rope::from_str("todo: a\r\nnot todo\r\ntodo: b");
        let m = SearchQuery::literal("^todo").regex(true).compile().unwrap();
        assert_eq!(m.find_all(&rope, 0..rope.len_chars(), 100).len(), 2);
    }

    #[test]
    fn regex_replacement_expands_groups() {
        let rope = Rope::from_str("let a = 1;\nlet bb = 22;");
        let m = SearchQuery::literal(r"let (\w+) = (?P<v>\d+)")
            .regex(true)
            .compile()
            .unwrap();
        let reps = m.replacements(&rope, 0..rope.len_chars(), "const $1: i32 = ${v}$$", true);
        assert_eq!(reps[0], (0..9, "const a: i32 = 1$".to_string()));
        assert_eq!(reps[1].1, "const bb: i32 = 22$");
    }

    #[test]
    fn prev_in_large_document() {
        let mut text = "ab".to_string();
        text.push_str(&"x".repeat(300_000));
        let rope = Rope::from_str(&text);
        let m = SearchQuery::literal("ab").compile().unwrap();
        assert_eq!(m.find_prev(&rope, rope.len_chars()), Some(0..2));
    }
}
