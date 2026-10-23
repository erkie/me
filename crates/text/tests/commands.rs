//! Behaviour tests for editing commands.
//!
//! Notation: `|` is a cursor, `[text]` is a forward selection (head at `]`),
//! `{text}` is a reversed selection (head at `{`).

use editor_text::{ClipboardText, EditorState, SearchQuery, Selection, SelectionSet};

fn state(marked: &str) -> EditorState {
    let mut text = String::new();
    let mut sels = Vec::new();
    let mut open: Option<usize> = None;
    let mut pos = 0;
    for ch in marked.chars() {
        match ch {
            '|' => sels.push(Selection::cursor(pos)),
            '[' | '{' => open = Some(pos),
            ']' => sels.push(Selection::new(open.take().unwrap(), pos)),
            '}' => sels.push(Selection::new(pos, open.take().unwrap())),
            _ => {
                text.push(ch);
                pos += 1;
            }
        }
    }
    let mut state = EditorState::from_str(&text);
    if !sels.is_empty() {
        let primary = sels.len() - 1;
        state.set_selections(SelectionSet::new(sels, primary));
    }
    state
}

fn render(state: &EditorState) -> String {
    let mut marks: Vec<(usize, char)> = Vec::new();
    for s in state.selections().iter() {
        if s.is_empty() {
            marks.push((s.head, '|'));
        } else if s.is_reversed() {
            marks.push((s.start(), '{'));
            marks.push((s.end(), '}'));
        } else {
            marks.push((s.start(), '['));
            marks.push((s.end(), ']'));
        }
    }
    let mut out = String::new();
    let chars: Vec<char> = state.text().chars().collect();
    let mut mi = 0;
    marks.sort_by_key(|(p, c)| (*p, matches!(c, '[' | '{')));
    for (i, ch) in chars.iter().enumerate() {
        while mi < marks.len() && marks[mi].0 == i {
            out.push(marks[mi].1);
            mi += 1;
        }
        out.push(*ch);
    }
    while mi < marks.len() {
        out.push(marks[mi].1);
        mi += 1;
    }
    out
}

#[track_caller]
fn check(before: &str, f: impl FnOnce(&mut EditorState), after: &str) {
    let mut s = state(before);
    f(&mut s);
    assert_eq!(render(&s), after, "\nbefore: {before}");
}

// -------------------------------------------------------------------------
// Typing and deleting
// -------------------------------------------------------------------------

#[test]
fn typing_at_many_cursors() {
    check("a|b|c|", |s| s.insert("x"), "ax|bx|cx|");
    check("[ab]c{de}", |s| s.insert("_"), "_|c_|");
}

#[test]
fn backspace_and_delete() {
    check("ab|c|d", |s| s.backspace(), "a|d");
    check("|abc", |s| s.backspace(), "|abc");
    check("a|bc|d", |s| s.delete_forward(), "a|c|");
    check("[ab]cd", |s| s.backspace(), "|cd");
    check("e\u{301}|x", |s| s.backspace(), "|x");
}

#[test]
fn soft_tab_backspace_removes_to_tab_stop() {
    check("        |x", |s| s.backspace(), "    |x");
    check("      |x", |s| s.backspace(), "    |x");
    check("  a |x", |s| s.backspace(), "  a|x");
}

#[test]
fn word_and_line_deletes() {
    check("foo bar|", |s| s.delete_word_left(), "foo |");
    check("|foo bar", |s| s.delete_word_right(), "| bar");
    check("abc\n  de|f", |s| s.delete_to_line_start(), "abc\n|f");
    check("abc\n|def", |s| s.delete_to_line_start(), "abc|def");
    check("a|bc\nd", |s| s.delete_to_line_end(), "a|\nd");
}

#[test]
fn newline_keeps_indentation() {
    check("    foo|", |s| s.newline(), "    foo\n    |");
    check("    if x (|)", |s| s.newline(), "    if x (\n        |)");
    check("  |  x", |s| s.newline(), "  \n  |  x");
    check("a|\nb|", |s| s.newline(), "a\n|\nb\n|");
}

#[test]
fn crlf_documents_get_crlf_newlines() {
    let mut s = state("a\r\nb|");
    s.newline();
    assert_eq!(s.text(), "a\r\nb\r\n");
}

// -------------------------------------------------------------------------
// Multiple selections
// -------------------------------------------------------------------------

#[test]
fn cmd_d_expands_word_then_adds_whole_word_occurrences() {
    let mut s = state("foo food fo|o (foo) foo");
    assert!(s.add_next_occurrence());
    assert_eq!(render(&s), "foo food [foo] (foo) foo");
    assert!(s.add_next_occurrence());
    assert_eq!(render(&s), "foo food [foo] ([foo]) foo");
    assert!(s.add_next_occurrence());
    assert!(s.add_next_occurrence());
    assert_eq!(render(&s), "[foo] food [foo] ([foo]) [foo]");
    // Every whole-word occurrence is selected; "food" never matches.
    assert!(!s.add_next_occurrence());
    assert_eq!(s.selections().len(), 4);
}

#[test]
fn cmd_d_on_punctuation() {
    let mut s = state("|//! a\n//! b\nx");
    s.add_next_occurrence();
    s.add_next_occurrence();
    assert_eq!(render(&s), "[//!] a\n[//!] b\nx");
}

#[test]
fn cmd_d_on_manual_selection_matches_substrings() {
    let mut s = state("[foo] food");
    assert!(s.add_next_occurrence());
    assert_eq!(render(&s), "[foo] [foo]d");
}

#[test]
fn cmd_d_then_type_edits_every_occurrence() {
    let mut s = state("let |a = a + ab;");
    s.add_next_occurrence();
    s.add_next_occurrence();
    s.insert("x");
    assert_eq!(render(&s), "let x| = x| + ab;");
}

#[test]
fn skip_occurrence_moves_primary() {
    let mut s = state("x [a] a a");
    s.add_next_occurrence();
    assert_eq!(render(&s), "x [a] [a] a");
    s.skip_occurrence();
    assert_eq!(render(&s), "x [a] a [a]");
}

#[test]
fn select_all_occurrences() {
    check(
        "one two o|ne three one",
        |s| {
            s.select_all_occurrences();
        },
        "[one] two [one] three [one]",
    );
}

#[test]
fn cmd_l_selects_lines_and_extends() {
    let mut s = state("a\nb|b\nc\nd");
    s.select_line();
    assert_eq!(render(&s), "a\n[bb\n]c\nd");
    s.select_line();
    assert_eq!(render(&s), "a\n[bb\nc\n]d");
    s.select_line();
    s.select_line();
    assert_eq!(render(&s), "a\n[bb\nc\nd]");
}

#[test]
fn cmd_shift_l_splits_into_lines() {
    let mut s = state("a[bc\ndef\n\ngh]i");
    s.split_into_lines();
    assert_eq!(render(&s), "a[bc]\n[def]\n|\n[gh]i");
    s.move_line_end(false);
    s.insert(";");
    assert_eq!(s.text(), "abc;\ndef;\n;\nghi;");
}

#[test]
fn split_into_lines_ignores_trailing_line_start() {
    check("[ab\ncd\n]ef", |s| s.split_into_lines(), "[ab]\n[cd]\nef");
}

#[test]
fn add_cursor_above_and_below_keeps_goal_column() {
    let mut s = state("abcdef\nab\nabc|def\n");
    s.add_cursor_vertically(false);
    assert_eq!(render(&s), "abcdef\nab|\nabc|def\n");
    s.add_cursor_vertically(false);
    assert_eq!(render(&s), "abc|def\nab|\nabc|def\n");
    assert!(!s.add_cursor_vertically(false));
    s.add_cursor_vertically(true);
    assert_eq!(render(&s), "abc|def\nab|\nabc|def\n|");
}

#[test]
fn column_selection_skips_short_lines() {
    let mut s = state("hello world\nhi\nhello there\n");
    s.select_column((0, 6), (2, 9));
    assert_eq!(render(&s), "hello [wor]ld\nhi\nhello [the]re\n");
    s.insert("X");
    assert_eq!(s.text(), "hello Xld\nhi\nhello Xre\n");
}

#[test]
fn zero_width_column_selection_puts_a_cursor_on_every_line() {
    check(
        "abcd\nab\nabcd",
        |s| s.select_column((0, 3), (2, 3)),
        "abc|d\nab|\nabc|d",
    );
}

#[test]
fn leftward_column_drag_makes_reversed_selections() {
    check(
        "abcd\nabcd",
        |s| s.select_column((0, 3), (1, 1)),
        "a{bc}d\na{bc}d",
    );
}

#[test]
fn upward_column_drag() {
    let mut s = state("abcd\nabcd\nabcd");
    s.select_column((2, 1), (0, 2));
    assert_eq!(render(&s), "a[b]cd\na[b]cd\na[b]cd");
    // The head line (top) is primary.
    assert_eq!(s.selections().primary().start(), 1);
}

#[test]
fn column_selection_with_tabs_uses_visual_columns() {
    check(
        "\tab\n    ab",
        |s| s.select_column((0, 4), (1, 5)),
        "\t[a]b\n    [a]b",
    );
}

#[test]
fn cmd_click_toggles_cursors() {
    let mut s = state("ab|cd");
    s.toggle_cursor(4);
    assert_eq!(render(&s), "ab|cd|");
    s.toggle_cursor(2);
    assert_eq!(render(&s), "abcd|");
}

#[test]
fn escape_collapses_to_primary() {
    let mut s = state("a|b|c[d]");
    s.collapse_selections();
    assert_eq!(render(&s), "abc[d]");
    s.collapse_selections();
    assert_eq!(render(&s), "abcd|");
}

#[test]
fn many_cursors_edit_in_one_transaction() {
    let n = 10_000;
    let text = "x\n".repeat(n);
    let mut s = EditorState::from_str(&text);
    let sels: Vec<Selection> = (0..n).map(|i| Selection::cursor(i * 2)).collect();
    s.set_selections(SelectionSet::new(sels, 0));
    s.insert("ab");
    assert_eq!(s.text(), "abx\n".repeat(n));
    s.undo();
    assert_eq!(s.text(), text);
    assert_eq!(s.selections().len(), n);
}

// -------------------------------------------------------------------------
// Undo
// -------------------------------------------------------------------------

#[test]
fn typing_coalesces_into_one_undo_step() {
    let mut s = state("|");
    for ch in ["h", "e", "l", "l", "o"] {
        s.insert(ch);
    }
    assert_eq!(s.text(), "hello");
    s.undo();
    assert_eq!(render(&s), "|");
    s.redo();
    assert_eq!(render(&s), "hello|");
}

#[test]
fn moving_the_cursor_breaks_coalescing() {
    let mut s = state("|");
    s.insert("a");
    s.move_left(false);
    s.insert("b");
    assert_eq!(s.text(), "ba");
    s.undo();
    assert_eq!(render(&s), "|a");
    s.undo();
    assert_eq!(render(&s), "|");
}

#[test]
fn undo_restores_multiple_selections() {
    let mut s = state("[a] [a] [a]");
    s.insert("bb");
    assert_eq!(render(&s), "bb| bb| bb|");
    s.undo();
    assert_eq!(render(&s), "[a] [a] [a]");
}

// -------------------------------------------------------------------------
// Clipboard
// -------------------------------------------------------------------------

#[test]
fn copy_paste_distributes_across_cursors() {
    let s = state("[a] [bb] [c]\n| | |");
    let clip = s.copy();
    assert_eq!(clip.text, "a\nbb\nc");
    let mut t = state("1| 2| 3|");
    t.paste(&clip);
    assert_eq!(render(&t), "1a| 2bb| 3c|");
}

#[test]
fn pasting_lines_into_matching_cursor_count() {
    let mut s = state("|\n|\n|");
    s.paste(&ClipboardText {
        text: "x\ny\nz".into(),
        per_selection: vec!["x\ny\nz".into()],
        whole_lines: false,
    });
    assert_eq!(render(&s), "x|\ny|\nz|");
}

#[test]
fn copy_without_selection_copies_the_line() {
    let mut s = state("one\ntw|o\nthree");
    let clip = s.copy();
    assert_eq!(clip.text, "two\n");
    assert!(clip.whole_lines);
    let mut t = state("a\nb|c");
    t.paste(&clip);
    assert_eq!(render(&t), "a\ntwo\nb|c");
    let cut = s.cut();
    assert_eq!(cut.text, "two\n");
    assert_eq!(s.text(), "one\nthree");
}

// -------------------------------------------------------------------------
// Lines
// -------------------------------------------------------------------------

#[test]
fn tab_indents_lines_or_inserts_spaces() {
    check("a|b", |s| s.tab(), "a   |b");
    check("[a\nb]", |s| s.tab(), "[    a\n    b]");
    check("a[a\nb]", |s| s.tab(), "    a[a\n    b]");
    check("[a\n\nb\n]c", |s| s.indent_lines(), "[    a\n\n    b\n]c");
}

#[test]
fn outdent_lines() {
    check("    [a\n  b\n\tc]", |s| s.outdent_lines(), "[a\nb\nc]");
    check("      a|", |s| s.outdent_lines(), "  a|");
}

#[test]
fn hard_tabs_are_detected() {
    let mut s = state("\tfoo\n\tbar|");
    assert!(s.hard_tabs);
    s.newline();
    assert_eq!(s.text(), "\tfoo\n\tbar\n\t");
}

#[test]
fn toggle_line_comment() {
    let mut s = state("  [foo\n    bar]\n");
    s.toggle_line_comment("//");
    assert_eq!(s.text(), "  // foo\n  //   bar\n");
    s.toggle_line_comment("//");
    assert_eq!(s.text(), "  foo\n    bar\n");
}

#[test]
fn move_lines_up_and_down() {
    let mut s = state("a\nb|\nc");
    s.move_lines(false);
    assert_eq!(render(&s), "b|\na\nc");
    s.move_lines(false);
    assert_eq!(render(&s), "b|\na\nc");
    s.move_lines(true);
    s.move_lines(true);
    assert_eq!(render(&s), "a\nc\nb|");
    s.undo();
    assert_eq!(s.text(), "a\nb\nc");
}

#[test]
fn duplicate_lines_and_selections() {
    check("a\nb|c\nd", |s| s.duplicate(), "a\nbc\nb|c\nd");
    check("x[ab]y", |s| s.duplicate(), "xab[ab]y");
    check("last|", |s| s.duplicate(), "last\nlast|");
}

#[test]
fn delete_lines() {
    check("a\nb|b\nc", |s| s.delete_lines(), "a\n|c");
    check("a\nb|", |s| s.delete_lines(), "|a");
    check("[a\nb]\nc", |s| s.delete_lines(), "|c");
}

// -------------------------------------------------------------------------
// Movement
// -------------------------------------------------------------------------

#[test]
fn horizontal_movement() {
    check("a[bc]d", |s| s.move_left(false), "a|bcd");
    check("a[bc]d", |s| s.move_right(false), "abc|d");
    check("ab|cd", |s| s.move_right(true), "ab[c]d");
    check("ab|cd", |s| s.move_left(true), "a{b}cd");
    check("foo bar| baz", |s| s.move_word_left(false), "foo |bar baz");
    check("foo |bar baz", |s| s.move_word_right(false), "foo bar| baz");
}

#[test]
fn smart_line_start() {
    let mut s = state("    fo|o");
    s.move_line_start(false);
    assert_eq!(render(&s), "    |foo");
    s.move_line_start(false);
    assert_eq!(render(&s), "|    foo");
    s.move_line_end(false);
    assert_eq!(render(&s), "    foo|");
}

#[test]
fn vertical_movement_keeps_goal_column() {
    let mut s = state("abcdef|\nab\nabcdefgh");
    s.move_down(false);
    assert_eq!(render(&s), "abcdef\nab|\nabcdefgh");
    s.move_down(false);
    assert_eq!(render(&s), "abcdef\nab\nabcdef|gh");
    s.move_down(false);
    assert_eq!(render(&s), "abcdef\nab\nabcdefgh|");
    s.move_doc_start(true);
    assert_eq!(render(&s), "{abcdef\nab\nabcdefgh}");
}

// -------------------------------------------------------------------------
// Find and replace
// -------------------------------------------------------------------------

#[test]
fn find_next_and_previous() {
    let q = SearchQuery::literal("ab");
    let mut s = state("|ab ab ab");
    s.find(&q, false);
    assert_eq!(render(&s), "[ab] ab ab");
    s.find(&q, false);
    assert_eq!(render(&s), "ab [ab] ab");
    s.find(&q, true);
    assert_eq!(render(&s), "[ab] ab ab");
    s.find(&q, true);
    assert_eq!(render(&s), "ab ab [ab]");
}

#[test]
fn replace_all_is_one_undo_step() {
    let mut s = state("|a1 b22 c333");
    let q = SearchQuery::literal(r"(\w)(\d+)").regex(true);
    assert_eq!(s.replace_all(&q, "$2$1"), 3);
    assert_eq!(s.text(), "1a 22b 333c");
    s.undo();
    assert_eq!(s.text(), "a1 b22 c333");
}

#[test]
fn replace_next_steps_through_matches() {
    let q = SearchQuery::literal("x");
    let mut s = state("|x x x");
    s.find(&q, false);
    s.replace_next(&q, "y");
    assert_eq!(render(&s), "y [x] x");
    s.replace_next(&q, "y");
    assert_eq!(render(&s), "y y [x]");
}

#[test]
fn select_all_matches_from_find() {
    check(
        "|cat dog cat",
        |s| {
            s.select_all_matches(&SearchQuery::literal("cat"));
        },
        "[cat] dog [cat]",
    );
}
