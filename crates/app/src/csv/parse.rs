//! CSV and TSV without a UI: guessing the dialect and encoding, indexing
//! record starts, and splitting records into fields.
//!
//! Records are found with one pass over the bytes. Only every
//! [`STRIDE`]th record start is kept, so a ten-million-row file costs a few
//! megabytes of index; a visible row is reached by parsing forward from the
//! checkpoint before it.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use encoding_rs::Encoding;
use me_text::Rope;

/// One record start is kept per this many records.
pub const STRIDE: usize = 64;
/// Fields are cut to this many bytes for display.
pub const MAX_FIELD_BYTES: usize = 2048;
/// A record (an unclosed quote, say) is read no further than this.
const MAX_RECORD_BYTES: usize = 4 << 20;
/// How much of the file the dialect is guessed from.
pub const SNIFF_BYTES: usize = 64 * 1024;
/// Records read for column widths and the header guess.
pub const SAMPLE_RECORDS: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dialect {
    pub delimiter: u8,
    pub quote: Option<u8>,
}

impl Default for Dialect {
    fn default() -> Self {
        Self {
            delimiter: b',',
            quote: Some(b'"'),
        }
    }
}

/// The separators offered in the menus and tried when guessing.
pub const DELIMITERS: &[(u8, &str)] = &[
    (b',', "Comma"),
    (b'\t', "Tab"),
    (b';', "Semicolon"),
    (b'|', "Pipe"),
    (b' ', "Space"),
];

pub fn delimiter_name(delimiter: u8) -> String {
    DELIMITERS
        .iter()
        .find(|(d, _)| *d == delimiter)
        .map(|(_, name)| name.to_string())
        .unwrap_or_else(|| format!("“{}”", delimiter as char))
}

/// Whether a file should open as a table.
pub fn is_table(path: &std::path::Path) -> bool {
    extension_delimiter(path).is_some()
}

/// The separator a file's extension promises, if it's a table at all.
pub fn extension_delimiter(path: &std::path::Path) -> Option<u8> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "csv" => Some(b','),
        "tsv" => Some(b'\t'),
        "psv" => Some(b'|'),
        _ => None,
    }
}

// -------------------------------------------------------------------------
// Scanning
// -------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// Nothing of the field read yet (spaces before a quote are allowed).
    FieldStart,
    Unquoted,
    Quoted,
    /// Just read a quote inside a quoted field: either the field's end or
    /// the first half of `""`.
    QuoteInQuoted,
}

/// Finds record ends, counting fields on the way.
struct Scanner {
    dialect: Dialect,
    state: State,
    fields: usize,
}

impl Scanner {
    fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            state: State::FieldStart,
            fields: 1,
        }
    }

    /// Calls `on_end(index past the newline, fields)` for each record that
    /// ends in `chunk`.
    #[inline]
    fn feed(&mut self, chunk: &[u8], mut on_end: impl FnMut(usize, usize) -> bool) -> bool {
        let Dialect { delimiter, quote } = self.dialect;
        let mut i = 0;
        while i < chunk.len() {
            let b = chunk[i];
            match self.state {
                State::Quoted => {
                    // Skip to the next quote in one go.
                    match quote.and_then(|q| memchr::memchr(q, &chunk[i..])) {
                        Some(n) => {
                            i += n;
                            self.state = State::QuoteInQuoted;
                        }
                        None => return true,
                    }
                }
                State::Unquoted => match memchr::memchr2(delimiter, b'\n', &chunk[i..]) {
                    Some(n) => {
                        i += n;
                        if chunk[i] == b'\n' {
                            if !self.end(i + 1, &mut on_end) {
                                return false;
                            }
                        } else {
                            self.fields += 1;
                            self.state = State::FieldStart;
                        }
                    }
                    None => return true,
                },
                State::FieldStart | State::QuoteInQuoted => {
                    if Some(b) == quote {
                        // An opening quote, or the second half of `""`.
                        self.state = State::Quoted;
                    } else if b == delimiter {
                        self.fields += 1;
                        self.state = State::FieldStart;
                    } else if b == b'\n' {
                        if !self.end(i + 1, &mut on_end) {
                            return false;
                        }
                    } else if !(b == b' ' && self.state == State::FieldStart) {
                        self.state = State::Unquoted;
                    }
                }
            }
            i += 1;
        }
        true
    }

    #[inline]
    fn end(&mut self, at: usize, on_end: &mut impl FnMut(usize, usize) -> bool) -> bool {
        let fields = self.fields;
        self.fields = 1;
        self.state = State::FieldStart;
        on_end(at, fields)
    }

    /// Whether bytes have been read since the last record end.
    fn mid_record(&self) -> bool {
        self.state != State::FieldStart || self.fields > 1
    }
}

/// The rope's bytes from `from` on, chunk by chunk.
pub fn chunks_from(rope: &Rope, from: usize) -> impl Iterator<Item = &[u8]> {
    let from = from.min(rope.len_bytes());
    let (chunks, chunk_start, _, _) = rope.chunks_at_byte(from);
    let mut skip = from - chunk_start;
    chunks.map(move |chunk| {
        let bytes = &chunk.as_bytes()[skip.min(chunk.len())..];
        skip = 0;
        bytes
    })
}

/// Where the first record starts: after a UTF-8 byte order mark.
fn data_start(rope: &Rope) -> usize {
    let mut head = [0u8; 3];
    for (i, b) in rope.bytes().take(3).enumerate() {
        head[i] = b;
    }
    if head == [0xEF, 0xBB, 0xBF] { 3 } else { 0 }
}

// -------------------------------------------------------------------------
// Index
// -------------------------------------------------------------------------

/// Where records start, and how many there are.
#[derive(Clone, Debug)]
pub struct Index {
    checkpoints: Vec<usize>,
    /// Records, header included.
    pub records: usize,
    /// The most fields in any record.
    pub columns: usize,
    /// False while only the start of the file has been indexed.
    pub complete: bool,
}

impl Index {
    /// Indexes `rope`, or its first `limit` bytes. `None` when cancelled.
    pub fn build(
        rope: &Rope,
        dialect: Dialect,
        limit: Option<usize>,
        cancel: &AtomicBool,
    ) -> Option<Self> {
        let start = data_start(rope);
        let len = rope.len_bytes();
        let end = limit.map_or(len, |l| (start + l).min(len));
        let mut scanner = Scanner::new(dialect);
        let mut checkpoints = vec![start];
        let mut records = 0;
        let mut columns = 0;
        let mut offset = start;
        for chunk in chunks_from(rope, start) {
            let chunk = &chunk[..chunk.len().min(end - offset)];
            scanner.feed(chunk, |at, fields| {
                records += 1;
                columns = columns.max(fields);
                if records % STRIDE == 0 {
                    checkpoints.push(offset + at);
                }
                true
            });
            offset += chunk.len();
            if offset >= end {
                break;
            }
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
        }
        let complete = end == len;
        // A last line without a newline is a record too, once we know it's
        // the last.
        if complete && scanner.mid_record() {
            records += 1;
            columns = columns.max(scanner.fields);
        }
        if checkpoints.len() > records.div_ceil(STRIDE).max(1) {
            checkpoints.pop();
        }
        Some(Self {
            checkpoints,
            records,
            columns,
            complete,
        })
    }

    /// Up to `count` records from record `first` on, split into fields
    /// (each cut for display).
    pub fn records(
        &self,
        rope: &Rope,
        dialect: Dialect,
        first: usize,
        count: usize,
    ) -> Vec<Vec<String>> {
        let mut out = Vec::with_capacity(count.min(self.records));
        self.each_record(rope, dialect, first, count, MAX_FIELD_BYTES, |r| {
            out.push(r)
        });
        out
    }

    /// Calls `f` with up to `count` records from record `first` on, fields
    /// cut to `max_field` bytes. Doesn't hold them all, for copying a
    /// million rows.
    pub fn each_record(
        &self,
        rope: &Rope,
        dialect: Dialect,
        first: usize,
        count: usize,
        max_field: usize,
        mut f: impl FnMut(Vec<String>),
    ) {
        let first = first.min(self.records);
        let count = count.min(self.records - first);
        let Some(&start) = self.checkpoints.get(first / STRIDE) else {
            return;
        };
        let mut skip = first % STRIDE;
        let mut done = 0;
        let mut reader = FieldReader::new(dialect, max_field);
        let mut read = 0;
        if count == 0 {
            return;
        }
        'chunks: for chunk in chunks_from(rope, start) {
            for &b in chunk {
                read += 1;
                if let Some(record) = reader.push(b, skip > 0) {
                    read = 0;
                    if skip > 0 {
                        skip -= 1;
                    } else {
                        f(record);
                        done += 1;
                        if done == count {
                            break 'chunks;
                        }
                    }
                } else if read > MAX_RECORD_BYTES {
                    // Give up on a runaway record: show what we have.
                    if skip == 0 {
                        f(reader.finish());
                        done += 1;
                    }
                    break 'chunks;
                }
            }
        }
        if done < count && skip == 0 && reader.mid_record() {
            f(reader.finish());
        }
    }
}

/// Splits bytes into fields, one record at a time.
struct FieldReader {
    dialect: Dialect,
    max_field: usize,
    state: State,
    field: Vec<u8>,
    fields: Vec<String>,
    /// The field's trailing `\r` is part of the line ending, not the text.
    cr: bool,
}

impl FieldReader {
    fn new(dialect: Dialect, max_field: usize) -> Self {
        Self {
            dialect,
            max_field,
            state: State::FieldStart,
            field: Vec::new(),
            fields: Vec::new(),
            cr: false,
        }
    }

    fn mid_record(&self) -> bool {
        self.state != State::FieldStart || !self.fields.is_empty() || !self.field.is_empty()
    }

    fn put(&mut self, b: u8, keep: bool) {
        if keep && self.field.len() < self.max_field {
            self.field.push(b);
        }
    }

    fn end_field(&mut self) {
        let text = String::from_utf8_lossy(&self.field).into_owned();
        self.fields.push(text);
        self.field.clear();
        self.state = State::FieldStart;
    }

    fn finish(&mut self) -> Vec<String> {
        self.end_field();
        std::mem::take(&mut self.fields)
    }

    /// Feeds one byte; returns the record it completes. While `skipping`,
    /// field text isn't kept.
    #[inline]
    fn push(&mut self, b: u8, skipping: bool) -> Option<Vec<String>> {
        let Dialect { delimiter, quote } = self.dialect;
        let keep = !skipping;
        let was_cr = std::mem::replace(&mut self.cr, false);
        match self.state {
            State::Quoted => {
                if Some(b) == quote {
                    self.state = State::QuoteInQuoted;
                } else {
                    self.put(b, keep);
                }
                return None;
            }
            State::FieldStart if Some(b) == quote => {
                // Spaces before an opening quote aren't part of the value.
                self.field.clear();
                self.state = State::Quoted;
                return None;
            }
            State::QuoteInQuoted if Some(b) == quote => {
                self.put(b, keep);
                self.state = State::Quoted;
                return None;
            }
            _ => {}
        }
        if b == delimiter {
            if keep {
                self.end_field();
            } else {
                self.field.clear();
                self.state = State::FieldStart;
            }
            None
        } else if b == b'\n' {
            if was_cr && self.state != State::Quoted {
                self.field.pop();
            }
            if keep {
                Some(self.finish())
            } else {
                self.field.clear();
                self.state = State::FieldStart;
                Some(Vec::new())
            }
        } else {
            if !(b == b' ' && self.state == State::FieldStart) {
                self.state = State::Unquoted;
            }
            self.cr = b == b'\r';
            // A dropped (over-long) `\r` mustn't pop a kept byte.
            if self.cr && self.field.len() >= self.max_field {
                self.cr = false;
            }
            self.put(b, keep);
            None
        }
    }
}

// -------------------------------------------------------------------------
// Guessing
// -------------------------------------------------------------------------

/// Fields per record for the first records of `sample` (the last, possibly
/// cut off, record left out).
fn field_counts(sample: &[u8], dialect: Dialect, max: usize) -> Vec<usize> {
    let mut scanner = Scanner::new(dialect);
    let mut counts = Vec::new();
    scanner.feed(sample, |_, fields| {
        counts.push(fields);
        counts.len() < max
    });
    if counts.is_empty() && scanner.mid_record() {
        counts.push(scanner.fields);
    }
    counts
}

/// The separator that splits the sample's records most consistently.
/// `preferred` (from the extension) wins ties and single-column files.
pub fn sniff_delimiter(sample: &[u8], preferred: u8) -> u8 {
    let sample = sample.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(sample);
    let mut best: Option<(u64, u8)> = None;
    let mut candidates: Vec<u8> = vec![preferred];
    candidates.extend(
        DELIMITERS
            .iter()
            .map(|(d, _)| *d)
            .filter(|d| *d != preferred),
    );
    for delimiter in candidates {
        let dialect = Dialect {
            delimiter,
            quote: Some(b'"'),
        };
        let counts = field_counts(sample, dialect, 200);
        if counts.is_empty() {
            continue;
        }
        // The most common field count, and how many records have it.
        let mut freq = std::collections::HashMap::new();
        for &c in &counts {
            *freq.entry(c).or_insert(0usize) += 1;
        }
        let (&mode, &hits) = freq
            .iter()
            .max_by_key(|(fields, hits)| (**hits, **fields))
            .unwrap();
        if mode < 2 {
            continue;
        }
        let consistency = hits as f64 / counts.len() as f64;
        // Spaces separate words in prose too: only a last resort, and only
        // when every record agrees.
        if delimiter == b' ' && (consistency < 1. || best.is_some()) {
            continue;
        }
        // Consistency first, then more columns.
        let score = ((consistency * 100.).round() as u64) << 16 | mode.min(0xFFFF) as u64;
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, delimiter));
        }
    }
    best.map_or(preferred, |(_, d)| d)
}

pub fn is_numeric(s: &str) -> bool {
    let s = s.trim();
    let s = s.strip_prefix(['-', '+', '$', '€', '£']).unwrap_or(s);
    let s = s.strip_suffix('%').unwrap_or(s);
    !s.is_empty() && s.replace(',', "").parse::<f64>().is_ok()
}

/// Whether the first record names the columns, judged column by column
/// against the records under it (like Python's `csv.Sniffer.has_header`).
pub fn sniff_header(records: &[Vec<String>]) -> bool {
    let Some((first, rest)) = records.split_first() else {
        return false;
    };
    let rest = &rest[..rest.len().min(50)];
    if rest.is_empty() {
        // A lone record with text in every field reads as a header.
        return first.iter().all(|f| !f.trim().is_empty() && !is_numeric(f));
    }
    let mut votes = 0i32;
    for (col, name) in first.iter().enumerate() {
        let values: Vec<&str> = rest
            .iter()
            .filter_map(|r| r.get(col).map(String::as_str))
            .filter(|v| !v.trim().is_empty())
            .collect();
        if values.is_empty() {
            continue;
        }
        if values.iter().all(|v| is_numeric(v)) {
            // Types say more than lengths: headers are rarely numbers.
            votes += if is_numeric(name) { -2 } else { 2 };
            continue;
        }
        let len = values[0].chars().count();
        if values.len() > 1 && values.iter().all(|v| v.chars().count() == len) {
            votes += if name.chars().count() == len { -1 } else { 1 };
        }
    }
    if votes != 0 {
        return votes > 0;
    }
    // Nothing to go on: names are usually unique, short and filled in.
    let mut seen = std::collections::HashSet::new();
    first
        .iter()
        .all(|f| !f.trim().is_empty() && f.len() <= 64 && seen.insert(f.as_str()))
}

/// A file's encoding: its byte order mark, UTF-16 when every other byte is
/// zero, UTF-8 when it's valid, else Windows-1252, which is what Excel
/// writes on most machines.
pub fn sniff_encoding(bytes: &[u8]) -> &'static Encoding {
    if let Some((encoding, _)) = Encoding::for_bom(bytes) {
        return encoding;
    }
    let head = &bytes[..bytes.len().min(4096) & !1];
    if head.len() >= 4 {
        let zeros_at = |parity: usize| {
            head.iter()
                .skip(parity)
                .step_by(2)
                .filter(|b| **b == 0)
                .count()
        };
        let half = head.len() / 2;
        if zeros_at(1) * 10 > half * 7 {
            return encoding_rs::UTF_16LE;
        }
        if zeros_at(0) * 10 > half * 7 {
            return encoding_rs::UTF_16BE;
        }
    }
    if std::str::from_utf8(bytes).is_ok() {
        return encoding_rs::UTF_8;
    }
    encoding_rs::WINDOWS_1252
}

/// The text of `bytes` in `encoding` (a byte order mark wins).
pub fn decode(bytes: &[u8], encoding: &'static Encoding) -> String {
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

/// What a file looks like: its dialect and whether it has a header row.
pub struct Guess {
    pub dialect: Dialect,
    pub header: bool,
}

pub fn guess(rope: &Rope, preferred: u8) -> Guess {
    let sample: Vec<u8> = chunks_from(rope, 0)
        .flat_map(|c| c.iter().copied())
        .take(SNIFF_BYTES)
        .collect();
    let dialect = Dialect {
        delimiter: sniff_delimiter(&sample, preferred),
        quote: Some(b'"'),
    };
    Guess {
        dialect,
        header: guess_header(rope, dialect),
    }
}

pub fn guess_header(rope: &Rope, dialect: Dialect) -> bool {
    let cancel = AtomicBool::new(false);
    let Some(index) = Index::build(rope, dialect, Some(SNIFF_BYTES), &cancel) else {
        return false;
    };
    sniff_header(&index.records(rope, dialect, 0, 51))
}

// -------------------------------------------------------------------------
// Writing
// -------------------------------------------------------------------------

/// Where one record's fields sit in the text, quotes included.
#[derive(Debug, PartialEq)]
pub struct RawRecord {
    pub fields: Vec<Range<usize>>,
    /// Where the next record starts.
    pub next: usize,
}

/// The byte ranges of the fields of the record starting at `start`.
/// `None` for a record too long to edit.
pub fn raw_record(rope: &Rope, dialect: Dialect, start: usize) -> Option<RawRecord> {
    let Dialect { delimiter, quote } = dialect;
    let mut fields = Vec::new();
    let mut field_start = start;
    let mut state = State::FieldStart;
    let mut pos = start;
    let mut last = 0u8;
    for chunk in chunks_from(rope, start) {
        for &b in chunk {
            match state {
                State::Quoted => {
                    if Some(b) == quote {
                        state = State::QuoteInQuoted;
                    }
                }
                _ if Some(b) == quote
                    && matches!(state, State::FieldStart | State::QuoteInQuoted) =>
                {
                    state = State::Quoted;
                }
                _ if b == delimiter => {
                    fields.push(field_start..pos);
                    field_start = pos + 1;
                    state = State::FieldStart;
                }
                _ if b == b'\n' => {
                    let end = if last == b'\r' { pos - 1 } else { pos };
                    fields.push(field_start..end.max(field_start));
                    return Some(RawRecord {
                        fields,
                        next: pos + 1,
                    });
                }
                _ => {
                    if !(b == b' ' && state == State::FieldStart) {
                        state = State::Unquoted;
                    }
                }
            }
            last = b;
            pos += 1;
            if pos - start > MAX_RECORD_BYTES {
                return None;
            }
        }
    }
    fields.push(field_start..pos);
    Some(RawRecord { fields, next: pos })
}

impl Index {
    /// Where record `record` starts.
    pub fn record_start(&self, rope: &Rope, dialect: Dialect, record: usize) -> Option<usize> {
        if record >= self.records {
            return None;
        }
        let mut start = *self.checkpoints.get(record / STRIDE)?;
        for _ in 0..record % STRIDE {
            start = raw_record(rope, dialect, start)?.next;
        }
        Some(start)
    }
}

/// `value` as a field: quoted when it holds the separator, a quote or a
/// line break.
pub fn quote_field(value: &str, dialect: Dialect) -> String {
    let Some(q) = dialect.quote.map(char::from) else {
        return value.to_string();
    };
    let special =
        |c: char| c == q || c == '\n' || c == '\r' || c as u32 == dialect.delimiter as u32;
    if !value.contains(special) {
        return value.to_string();
    }
    let doubled = value.replace(q, &format!("{q}{q}"));
    format!("{q}{doubled}{q}")
}

/// The edit that puts `value` in column `col` of a record: the byte range
/// to replace and its new text. Short records get the missing separators.
pub fn field_edit(
    raw: &RawRecord,
    col: usize,
    value: &str,
    dialect: Dialect,
) -> (Range<usize>, String) {
    let text = quote_field(value, dialect);
    match raw.fields.get(col) {
        Some(range) => (range.clone(), text),
        None => {
            let end = raw.fields.last().map_or(0, |f| f.end);
            let separators = (dialect.delimiter as char)
                .to_string()
                .repeat(col + 1 - raw.fields.len());
            (end..end, separators + &text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str, dialect: Dialect) -> Vec<Vec<String>> {
        let rope = Rope::from_str(text);
        let index = Index::build(&rope, dialect, None, &AtomicBool::new(false)).unwrap();
        index.records(&rope, dialect, 0, usize::MAX)
    }

    fn csv(text: &str) -> Vec<Vec<String>> {
        rows(text, Dialect::default())
    }

    #[test]
    fn splits_plain_records() {
        assert_eq!(csv("a,b\n1,2\n"), vec![vec!["a", "b"], vec!["1", "2"]]);
        // No newline at the end.
        assert_eq!(csv("a,b\n1,2"), vec![vec!["a", "b"], vec!["1", "2"]]);
        // CRLF line endings.
        assert_eq!(csv("a,b\r\n1,2\r\n"), vec![vec!["a", "b"], vec!["1", "2"]]);
        // Empty fields and lines.
        assert_eq!(csv(",\n\n"), vec![vec!["", ""], vec![""]]);
    }

    #[test]
    fn handles_quotes() {
        assert_eq!(
            csv("\"a,b\",\"say \"\"hi\"\"\"\n\"multi\nline\",x\n"),
            vec![vec!["a,b", "say \"hi\""], vec!["multi\nline", "x"]]
        );
        // Quotes in the middle of a field are just text.
        assert_eq!(
            csv("5\" pipe,x\ny,z\n"),
            vec![vec!["5\" pipe", "x"], vec!["y", "z"]]
        );
        // A space before the opening quote.
        assert_eq!(csv("a, \"b,c\"\n"), vec![vec!["a", "b,c"]]);
        // No quoting at all.
        let none = Dialect {
            delimiter: b',',
            quote: None,
        };
        assert_eq!(rows("\"a,b\"\n", none), vec![vec!["\"a", "b\""]]);
    }

    #[test]
    fn skips_byte_order_mark() {
        assert_eq!(csv("\u{FEFF}name,age\nbo,3\n")[0], vec!["name", "age"]);
    }

    #[test]
    fn index_counts_and_seeks() {
        let mut text = String::from("n,sq\n");
        for i in 0..1000 {
            text.push_str(&format!("{i},\"{}\n\"\n", i * i));
        }
        let rope = Rope::from_str(&text);
        let dialect = Dialect::default();
        let index = Index::build(&rope, dialect, None, &AtomicBool::new(false)).unwrap();
        assert_eq!(index.records, 1001);
        assert_eq!(index.columns, 2);
        assert!(index.complete);
        for first in [0, 1, 63, 64, 65, 500, 999, 1000] {
            let got = index.records(&rope, dialect, first, 2);
            if first == 0 {
                assert_eq!(got[0], vec!["n", "sq"]);
                continue;
            }
            let i = first - 1;
            assert_eq!(got[0], vec![i.to_string(), format!("{}\n", i * i)]);
            assert_eq!(got.len(), if first == 1000 { 1 } else { 2 });
        }
        // A prefix index stops at the limit.
        let partial = Index::build(&rope, dialect, Some(100), &AtomicBool::new(false)).unwrap();
        assert!(!partial.complete && partial.records < 100);
    }

    #[test]
    fn sniffs_delimiters() {
        assert_eq!(sniff_delimiter(b"a,b,c\n1,2,3\n", b','), b',');
        assert_eq!(sniff_delimiter(b"a\tb\tc\n1\t2,5\t3\n", b','), b'\t');
        assert_eq!(sniff_delimiter(b"a;b;c\n1,5;2,5;3\n", b','), b';');
        assert_eq!(sniff_delimiter(b"a|b\n1|2\n", b','), b'|');
        // One column: trust the extension.
        assert_eq!(sniff_delimiter(b"just\nwords\n", b'\t'), b'\t');
        // Commas inside quotes don't count.
        assert_eq!(
            sniff_delimiter(b"name;note\n\"a\";\"x, y, z\"\n\"b\";\"q\"\n", b','),
            b';'
        );
    }

    #[test]
    fn sniffs_headers() {
        let r = |rows: &[&[&str]]| -> Vec<Vec<String>> {
            rows.iter()
                .map(|r| r.iter().map(|s| s.to_string()).collect())
                .collect()
        };
        assert!(sniff_header(&r(&[
            &["name", "age"],
            &["ann", "31"],
            &["bo", "4"]
        ])));
        assert!(!sniff_header(&r(&[&["1", "2"], &["3", "4"]])));
        assert!(!sniff_header(&r(&[
            &["ann", "31"],
            &["bo", "4"],
            &["cy", "12"]
        ])));
        assert!(sniff_header(&r(&[&["first", "last"], &["ann", "lee"]])));
    }

    #[test]
    fn sniffs_encodings() {
        assert_eq!(sniff_encoding(b"a,b\n"), encoding_rs::UTF_8);
        assert_eq!(sniff_encoding(b"caf\xe9,b\n"), encoding_rs::WINDOWS_1252);
        assert_eq!(sniff_encoding(b"\xff\xfea\0,\0"), encoding_rs::UTF_16LE);
        assert_eq!(sniff_encoding(b"a\0,\0b\0\n\0"), encoding_rs::UTF_16LE);
        assert_eq!(sniff_encoding(b"\0a\0,\0b\0\n"), encoding_rs::UTF_16BE);
        assert_eq!(decode(b"caf\xe9", encoding_rs::WINDOWS_1252), "café");
    }

    #[test]
    fn finds_raw_fields() {
        let text = "a,\"b,\"\"c\"\"\"\r\nx,y\nlast";
        let rope = Rope::from_str(text);
        let d = Dialect::default();
        let first = raw_record(&rope, d, 0).unwrap();
        let slices: Vec<&str> = first.fields.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(slices, vec!["a", "\"b,\"\"c\"\"\""]);
        let second = raw_record(&rope, d, first.next).unwrap();
        assert_eq!(&text[second.fields[1].clone()], "y");
        let third = raw_record(&rope, d, second.next).unwrap();
        assert_eq!(&text[third.fields[0].clone()], "last");
        assert_eq!(third.next, text.len());

        let index = Index::build(&rope, d, None, &AtomicBool::new(false)).unwrap();
        assert_eq!(index.record_start(&rope, d, 2), Some(second.next));
        assert_eq!(index.record_start(&rope, d, 3), None);
    }

    #[test]
    fn writes_fields() {
        let d = Dialect::default();
        assert_eq!(quote_field("plain", d), "plain");
        assert_eq!(quote_field("a,b", d), "\"a,b\"");
        assert_eq!(quote_field("say \"hi\"", d), "\"say \"\"hi\"\"\"");
        assert_eq!(quote_field("two\nlines", d), "\"two\nlines\"");
        let tab = Dialect {
            delimiter: b'\t',
            quote: Some(b'"'),
        };
        assert_eq!(quote_field("a,b", tab), "a,b");

        // Editing round-trips through the parser.
        let text = "id,name\n1,ann\n2\n";
        let rope = Rope::from_str(text);
        let index = Index::build(&rope, d, None, &AtomicBool::new(false)).unwrap();
        let edit = |record: usize, col: usize, value: &str| {
            let start = index.record_start(&rope, d, record).unwrap();
            let raw = raw_record(&rope, d, start).unwrap();
            let (range, new) = field_edit(&raw, col, value, d);
            let mut out = text.to_string();
            out.replace_range(range, &new);
            out
        };
        assert_eq!(edit(1, 1, "bo, jr"), "id,name\n1,\"bo, jr\"\n2\n");
        assert_eq!(edit(2, 1, "cy"), "id,name\n1,ann\n2,cy\n");
        assert_eq!(edit(2, 3, "z"), "id,name\n1,ann\n2,,,z\n");
        assert_eq!(csv(&edit(1, 1, "a\"b\nc"))[1], vec!["1", "a\"b\nc"]);
    }
}
