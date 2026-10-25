//! LSP snippets (`foo(${1:a}, ${2:b})$0`): the text to insert and its tab
//! stops, which Tab steps through after a completion.

use std::collections::BTreeMap;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    /// Tab stops in order ($1, $2, …, then $0 or the end), each with every
    /// place it appears (chars into `text`). Empty when there's nothing to
    /// step through.
    pub stops: Vec<Vec<Range<usize>>>,
}

pub fn parse(source: &str) -> Snippet {
    let mut parser = Parser {
        chars: source.chars().collect(),
        pos: 0,
        text: String::new(),
        len: 0,
        stops: BTreeMap::new(),
    };
    parser.any(&[]);
    let Parser {
        text, len, stops, ..
    } = parser;
    let final_stop = stops.get(&0).cloned();
    let mut ordered: Vec<Vec<Range<usize>>> = stops
        .into_iter()
        .filter(|(n, _)| *n != 0)
        .map(|(_, ranges)| ranges)
        .collect();
    if !ordered.is_empty() || final_stop.is_some() {
        let end = std::iter::once(len..len).collect();
        ordered.push(final_stop.unwrap_or(end));
    }
    // Only a final stop at the very end: nothing to step through.
    if ordered.len() == 1 && ordered[0].len() == 1 && ordered[0][0] == (len..len) {
        ordered.clear();
    }
    Snippet {
        text,
        stops: ordered,
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    text: String,
    /// `text`'s length in chars.
    len: usize,
    stops: BTreeMap<u32, Vec<Range<usize>>>,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn push(&mut self, c: char) {
        self.text.push(c);
        self.len += 1;
    }

    /// Text and snippet parts up to an unescaped char in `until` (left
    /// unconsumed) or the end.
    fn any(&mut self, until: &[char]) {
        while let Some(c) = self.peek() {
            if until.contains(&c) {
                return;
            }
            self.pos += 1;
            match c {
                '\\' => match self.peek() {
                    Some(next @ ('$' | '}' | '\\' | ',' | '|')) => {
                        self.pos += 1;
                        self.push(next);
                    }
                    _ => self.push('\\'),
                },
                '$' => self.dollar(),
                c => self.push(c),
            }
        }
    }

    fn number(&mut self) -> Option<u32> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        let digits: String = self.chars[start..self.pos].iter().collect();
        digits.parse().ok()
    }

    fn name(&mut self) -> Option<String> {
        let start = self.pos;
        if !self
            .peek()
            .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        {
            return None;
        }
        while self
            .peek()
            .is_some_and(|c| c == '_' || c.is_ascii_alphanumeric())
        {
            self.pos += 1;
        }
        Some(self.chars[start..self.pos].iter().collect())
    }

    fn stop(&mut self, n: u32, range: Range<usize>) {
        self.stops.entry(n).or_default().push(range);
    }

    /// After a `$`.
    fn dollar(&mut self) {
        let here = self.len;
        if let Some(n) = self.number() {
            self.stop(n, here..here);
            return;
        }
        if self.name().is_some() {
            // A variable ($TM_FILENAME …): we don't know any.
            return;
        }
        if self.peek() != Some('{') {
            self.push('$');
            return;
        }
        let brace = self.pos;
        self.pos += 1;
        if let Some(n) = self.number() {
            match self.peek() {
                Some('}') => {
                    self.pos += 1;
                    self.stop(n, here..here);
                }
                Some(':') => {
                    self.pos += 1;
                    self.any(&['}']);
                    self.pos += 1;
                    self.stop(n, here..self.len);
                }
                Some('|') => {
                    self.pos += 1;
                    let mut first = true;
                    loop {
                        let start = self.len;
                        self.any(&[',', '|']);
                        if !first {
                            // Only the first choice is inserted.
                            self.text.truncate(
                                self.text
                                    .char_indices()
                                    .nth(start)
                                    .map_or(self.text.len(), |(i, _)| i),
                            );
                            self.len = start;
                        }
                        first = false;
                        match self.peek() {
                            Some(',') => self.pos += 1,
                            _ => break,
                        }
                    }
                    // "|}"
                    self.pos = (self.pos + 2).min(self.chars.len());
                    self.stop(n, here..self.len);
                }
                _ => self.literal_from(brace),
            }
            return;
        }
        if self.name().is_some() {
            match self.peek() {
                Some('}') => self.pos += 1,
                // ${NAME:default}: the default.
                Some(':') => {
                    self.pos += 1;
                    self.any(&['}']);
                    self.pos += 1;
                }
                // ${NAME/regex/format/options}: dropped.
                Some('/') => {
                    let mut depth = 0;
                    while let Some(c) = self.peek() {
                        self.pos += 1;
                        match c {
                            '\\' => self.pos += 1,
                            '{' => depth += 1,
                            '}' if depth == 0 => break,
                            '}' => depth -= 1,
                            _ => {}
                        }
                    }
                }
                _ => self.literal_from(brace),
            }
            return;
        }
        self.literal_from(brace);
    }

    /// Not snippet syntax after all: `$` and what follows are text.
    fn literal_from(&mut self, brace: usize) {
        self.pos = brace;
        self.push('$');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stops(s: &Snippet) -> Vec<Vec<(usize, usize)>> {
        s.stops
            .iter()
            .map(|r| r.iter().map(|r| (r.start, r.end)).collect())
            .collect()
    }

    #[test]
    fn placeholders_and_final_stop() {
        let s = parse("foo(${1:a}, ${2:b})$0");
        assert_eq!(s.text, "foo(a, b)");
        assert_eq!(stops(&s), [vec![(4, 5)], vec![(7, 8)], vec![(9, 9)]]);
    }

    #[test]
    fn empty_stops_end_at_the_end() {
        let s = parse("println!(\"$1\")");
        assert_eq!(s.text, "println!(\"\")");
        assert_eq!(stops(&s), [vec![(10, 10)], vec![(12, 12)]]);
        // Only $0 at the end: nothing to step through.
        assert!(parse("foo()$0").stops.is_empty());
        assert!(parse("plain").stops.is_empty());
        // $0 elsewhere: the cursor goes there.
        assert_eq!(stops(&parse("foo($0)")), [vec![(4, 4)]]);
    }

    #[test]
    fn choices_nesting_and_mirrors() {
        let s = parse("${1|one,two|} x");
        assert_eq!(s.text, "one x");
        assert_eq!(stops(&s), [vec![(0, 3)], vec![(5, 5)]]);
        let s = parse("${1:foo${2:bar}}");
        assert_eq!(s.text, "foobar");
        assert_eq!(stops(&s), [vec![(0, 6)], vec![(3, 6)], vec![(6, 6)]]);
        let s = parse("$1 = ${1}");
        assert_eq!(s.text, " = ");
        assert_eq!(stops(&s), [vec![(0, 0), (3, 3)], vec![(3, 3)]]);
    }

    #[test]
    fn escapes_variables_and_stray_dollars() {
        assert_eq!(parse(r"\$1 costs \}").text, "$1 costs }");
        assert_eq!(parse("${TM_FILENAME:main.rs}").text, "main.rs");
        assert_eq!(parse("$TM_SELECTED_TEXT!").text, "!");
        assert_eq!(parse("${TM_FILENAME/(.*)/${1:/upcase}/}x").text, "x");
        assert_eq!(parse("cost: $ 5 and ${").text, "cost: $ 5 and ${");
        let s = parse("é${1:ü}😀$0");
        assert_eq!(s.text, "éü😀");
        assert_eq!(stops(&s), [vec![(1, 2)], vec![(3, 3)]]);
    }
}
