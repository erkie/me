//! The completion list and the code actions list: what the language server
//! (or, without one, the buffer's own words) offers at the cursor, filtered
//! as you type, the edits accepting an item makes, and how both lists look.

use std::ops::Range;

use gpui::{
    AnyElement, App, Entity, FontWeight, HighlightStyle, InteractiveElement, IntoElement,
    ParentElement, Pixels, SharedString, StatefulInteractiveElement, Styled, StyledText, div,
    prelude::FluentBuilder, px, relative,
};
use gpui_base::TextView;
use me_lsp::Encoding;
use me_lsp::types as lsp;
use me_text::Rope;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Matcher, Utf32Str};

use super::EditorView;
use super::snippet::{self, Snippet};
use crate::picker::Badge;
use crate::theme::{self, Palette};

/// Rows shown at once.
pub const VISIBLE_ROWS: usize = 10;
const ROW_HEIGHT: f32 = 24.;
const LIST_WIDTH: f32 = 420.;
const DOCS_WIDTH: f32 = 360.;
/// From the list's left edge to its labels (border, padding, badge, gap):
/// the list sits this far left of the word so the labels line up with it.
pub const TEXT_INSET: f32 = 1. + 4. + 6. + 16. + 6.;

/// A row of the completion list, for the editor's font size.
pub fn row_height(font_size: f32) -> f32 {
    (font_size * 1.7).round().max(ROW_HEIGHT)
}

/// Characters that make up the words being completed.
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// How the list was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionTrigger {
    /// Ctrl+Space.
    Manual,
    /// A character was typed.
    Typed(char),
    /// The list was partial and the word changed.
    Incomplete,
}

/// What the server answered, ready for [`EditorView::show_completions`].
pub struct Completions {
    pub items: Vec<lsp::CompletionItem>,
    /// The server sent part of the list; ask again as the word grows.
    pub incomplete: bool,
    pub encoding: Encoding,
    /// Where the cursor was when asked, as the server counts.
    pub asked: lsp::Position,
    /// Items can be resolved for their documentation.
    pub resolvable: bool,
}

pub struct Entry {
    pub item: lsp::CompletionItem,
    /// Resolving it was asked for, and answered.
    pub resolved: bool,
    pub answered: bool,
}

pub struct CompletionMenu {
    pub id: u64,
    /// Where the word being completed starts (chars).
    pub start: usize,
    /// The cursor when the server was asked, as it counts.
    pub asked: lsp::Position,
    pub entries: Vec<Entry>,
    /// The entries that match the typed word, best first, with the matched
    /// chars of their labels.
    pub matches: Vec<(usize, Vec<u32>)>,
    pub selected: usize,
    /// The first row shown.
    pub scroll: usize,
    pub encoding: Encoding,
    pub incomplete: bool,
    pub resolvable: bool,
}

impl CompletionMenu {
    pub fn new(id: u64, start: usize, completions: Completions) -> Self {
        Self {
            id,
            start,
            asked: completions.asked,
            entries: completions
                .items
                .into_iter()
                .map(|item| Entry {
                    item,
                    resolved: false,
                    answered: false,
                })
                .collect(),
            matches: Vec::new(),
            selected: 0,
            scroll: 0,
            encoding: completions.encoding,
            incomplete: completions.incomplete,
            resolvable: completions.resolvable,
        }
    }

    /// Filters the entries by the typed word (fuzzy, best first; the
    /// server's order when nothing is typed yet).
    pub fn filter(&mut self, typed: &str) {
        let items: Vec<&lsp::CompletionItem> = self.entries.iter().map(|e| &e.item).collect();
        self.matches = filter(&items, typed);
        self.selected = self
            .matches
            .iter()
            .position(|(ix, _)| self.entries[*ix].item.preselect == Some(true))
            .unwrap_or(0);
        self.scroll = 0;
        self.scroll_to_selected();
    }

    pub fn select(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len() as isize;
        self.selected = if delta.abs() > 1 {
            (self.selected as isize + delta).clamp(0, n - 1) as usize
        } else {
            (self.selected as isize + delta).rem_euclid(n) as usize
        };
        self.scroll_to_selected();
    }

    fn scroll_to_selected(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + VISIBLE_ROWS {
            self.scroll = self.selected + 1 - VISIBLE_ROWS;
        }
    }

    pub fn selected_entry(&self) -> Option<(usize, &Entry)> {
        let (ix, _) = self.matches.get(self.selected)?;
        Some((*ix, &self.entries[*ix]))
    }
}

fn sort_key(item: &lsp::CompletionItem) -> &str {
    item.sort_text.as_deref().unwrap_or(&item.label)
}

/// The items matching `typed`, as (index, matched chars of the label).
pub fn filter(items: &[&lsp::CompletionItem], typed: &str) -> Vec<(usize, Vec<u32>)> {
    if typed.is_empty() {
        let mut all: Vec<usize> = (0..items.len()).collect();
        all.sort_by(|a, b| {
            sort_key(items[*a])
                .cmp(sort_key(items[*b]))
                .then_with(|| items[*a].label.cmp(&items[*b].label))
        });
        return all.into_iter().map(|ix| (ix, Vec::new())).collect();
    }
    let pattern = Pattern::parse(typed, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(nucleo::Config::DEFAULT);
    let mut buf = Vec::new();
    let first = typed
        .chars()
        .next()
        .map(|c| c.to_lowercase().collect::<String>());
    let mut scored: Vec<(u32, usize, Vec<u32>)> = Vec::new();
    for (ix, item) in items.iter().enumerate() {
        let text = item.filter_text.as_deref().unwrap_or(&item.label);
        // The first letter typed has to start the name or one of its words
        // (`gtd` finds get_thing_done, not a stray g in the middle).
        let starts_a_word = text.char_indices().any(|(i, c)| {
            let boundary =
                i == 0 || c.is_uppercase() || text[..i].ends_with(|p: char| !p.is_alphanumeric());
            boundary && first.as_deref() == Some(&c.to_lowercase().collect::<String>())
        });
        if !starts_a_word {
            continue;
        }
        let Some(score) = pattern.score(Utf32Str::new(text, &mut buf), &mut matcher) else {
            continue;
        };
        let mut indices = Vec::new();
        pattern.indices(
            Utf32Str::new(&item.label, &mut buf),
            &mut matcher,
            &mut indices,
        );
        indices.sort_unstable();
        indices.dedup();
        scored.push((score, ix, indices));
    }
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| sort_key(items[a.1]).cmp(sort_key(items[b.1])))
            .then_with(|| items[a.1].label.cmp(&items[b.1].label))
    });
    scored
        .into_iter()
        .map(|(_, ix, indices)| (ix, indices))
        .collect()
}

/// What accepting an item does to the buffer.
#[derive(Debug, PartialEq)]
pub struct Accepted {
    /// Chars replaced by `text` (a snippet's text, if it's one).
    pub range: Range<usize>,
    pub text: String,
    pub snippet: Option<Snippet>,
    /// Other edits (an import at the top), chars of the buffer.
    pub additional: Vec<(Range<usize>, String)>,
}

/// The edits for accepting `item` with the cursor at `cursor`. The server's
/// ranges are about the text when it was asked; the word has grown (or
/// shrunk) by what was typed since, which moves the end of the range.
pub fn accept(
    menu: &CompletionMenu,
    item: &lsp::CompletionItem,
    rope: &Rope,
    cursor: usize,
) -> Accepted {
    let encoding = menu.encoding;
    let char_at = |position: lsp::Position| me_lsp::offset(rope, position, encoding);
    // The end of a range, moved along with the cursor when it's at or past
    // where the cursor was when asked.
    let end_at = |end: lsp::Position| {
        if end <= menu.asked {
            cursor
        } else if end.line == menu.asked.line {
            let extra = (end.character - menu.asked.character) as usize;
            let line_end = {
                let line = rope.char_to_line(cursor);
                let next = rope.line_to_char(line) + rope.line(line).len_chars();
                let mut end = next;
                while end > cursor && matches!(rope.char(end - 1), '\n' | '\r') {
                    end -= 1;
                }
                end
            };
            (cursor + extra).min(line_end)
        } else {
            char_at(end)
        }
    };
    let (range, new_text) = match &item.text_edit {
        Some(lsp::CompletionTextEdit::Edit(edit)) => (
            char_at(edit.range.start)..end_at(edit.range.end),
            edit.new_text.clone(),
        ),
        Some(lsp::CompletionTextEdit::InsertAndReplace(edit)) => (
            char_at(edit.insert.start)..end_at(edit.insert.end),
            edit.new_text.clone(),
        ),
        None => (
            menu.start..cursor,
            item.insert_text
                .clone()
                .unwrap_or_else(|| item.label.clone()),
        ),
    };
    let range = range.start.min(cursor)..range.end.max(cursor);
    let (text, snippet) = if item.insert_text_format == Some(lsp::InsertTextFormat::SNIPPET) {
        let snippet = snippet::parse(&new_text);
        (
            snippet.text.clone(),
            Some(snippet).filter(|s| !s.stops.is_empty()),
        )
    } else {
        (new_text, None)
    };
    let additional = item
        .additional_text_edits
        .iter()
        .flatten()
        .map(|e| {
            (
                me_lsp::char_range(rope, e.range, encoding),
                e.new_text.clone(),
            )
        })
        .filter(|(r, _)| r.end <= range.start || r.start >= range.end)
        .collect();
    Accepted {
        range,
        text,
        snippet,
        additional,
    }
}

/// Words in the buffer that start like `typed`, for files without a
/// language server.
pub fn buffer_words(rope: &Rope, typed: &str, exclude: Range<usize>) -> Vec<lsp::CompletionItem> {
    const MAX_SCAN: usize = 2 * 1024 * 1024;
    let mut seen = std::collections::HashSet::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut word_start = 0;
    let end = rope.len_chars().min(MAX_SCAN);
    for (i, c) in rope
        .chars()
        .take(end)
        .chain(std::iter::once(' '))
        .enumerate()
    {
        if is_word_char(c) {
            if word.is_empty() {
                word_start = i;
            }
            word.push(c);
            continue;
        }
        if word.chars().count() >= 3
            && !word.starts_with(|c: char| c.is_ascii_digit())
            && word != typed
            && !(word_start >= exclude.start && word_start <= exclude.end)
            && seen.insert(word.clone())
        {
            words.push(lsp::CompletionItem {
                label: word.clone(),
                kind: Some(lsp::CompletionItemKind::TEXT),
                ..Default::default()
            });
        }
        word.clear();
    }
    words
}

/// A small colored letter for a completion's kind.
fn kind_badge(kind: Option<lsp::CompletionItemKind>, palette: &Palette) -> Badge {
    use lsp::CompletionItemKind as K;
    use me_syntax::SymbolKind as S;
    let (glyph, symbol) = match kind {
        Some(K::METHOD) => ("m", S::Method),
        Some(K::FUNCTION) | Some(K::CONSTRUCTOR) => ("ƒ", S::Function),
        Some(K::FIELD) | Some(K::PROPERTY) => ("p", S::Other),
        Some(K::VARIABLE) => ("x", S::Other),
        Some(K::CLASS) => ("C", S::Class),
        Some(K::STRUCT) => ("S", S::Type),
        Some(K::INTERFACE) => ("I", S::Interface),
        Some(K::ENUM) => ("E", S::Type),
        Some(K::ENUM_MEMBER) => ("e", S::Constant),
        Some(K::TYPE_PARAMETER) => ("T", S::Type),
        Some(K::MODULE) => ("M", S::Module),
        Some(K::CONSTANT) | Some(K::VALUE) | Some(K::UNIT) => ("π", S::Constant),
        Some(K::KEYWORD) => ("k", S::Module),
        Some(K::SNIPPET) => ("{", S::Macro),
        Some(K::OPERATOR) => ("+", S::Other),
        Some(K::FILE) | Some(K::FOLDER) => ("/", S::Other),
        Some(K::EVENT) => ("!", S::Macro),
        Some(K::REFERENCE) => ("&", S::Other),
        Some(K::COLOR) => ("●", S::Other),
        _ => ("·", S::Other),
    };
    let mut badge = Badge::symbol(symbol, palette);
    badge.glyph = glyph;
    badge
}

fn documentation(item: &lsp::CompletionItem) -> Option<SharedString> {
    let text = match item.documentation.as_ref()? {
        lsp::Documentation::String(s) => s.clone(),
        lsp::Documentation::MarkupContent(m) => match m.kind {
            lsp::MarkupKind::Markdown => m.value.clone(),
            lsp::MarkupKind::PlainText => format!("```\n{}\n```", m.value.trim_end()),
        },
    };
    Some(text.into()).filter(|t: &SharedString| !t.trim().is_empty())
}

/// The completion list, with the selected item's documentation beside it.
pub fn completion_popover(
    view: &Entity<EditorView>,
    menu: &CompletionMenu,
    cx: &App,
) -> AnyElement {
    let palette = theme::palette(cx).clone();
    let mono = theme::fonts(cx).mono.clone();
    let size = crate::settings::get(cx).font_size;
    let rows = menu
        .matches
        .iter()
        .enumerate()
        .skip(menu.scroll)
        .take(VISIBLE_ROWS)
        .map(|(row, (ix, indices))| {
            let item = &menu.entries[*ix].item;
            let selected = row == menu.selected;
            let deprecated = item.deprecated == Some(true)
                || item
                    .tags
                    .as_ref()
                    .is_some_and(|t| t.contains(&lsp::CompletionItemTag::DEPRECATED));
            let highlights: Vec<(Range<usize>, HighlightStyle)> = item
                .label
                .char_indices()
                .enumerate()
                .filter(|(ci, _)| indices.binary_search(&(*ci as u32)).is_ok())
                .map(|(_, (bi, ch))| {
                    (
                        bi..bi + ch.len_utf8(),
                        HighlightStyle {
                            color: Some(palette.accent),
                            font_weight: Some(FontWeight::BOLD),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            let details = item.label_details.as_ref();
            let inline = details.and_then(|d| d.detail.clone());
            let right = details
                .and_then(|d| d.description.clone())
                .or_else(|| item.detail.clone().filter(|_| inline.is_none()))
                .map(|d| d.lines().next().unwrap_or_default().to_string());
            let view = view.clone();
            div()
                .id(row)
                .h(px(row_height(size)))
                .px(px(6.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(4.))
                .when(selected, |el| el.bg(palette.row_selected))
                .when(!selected, |el| el.hover(|s| s.bg(palette.hover)))
                .child(kind_badge(item.kind, &palette).element())
                .child(
                    div()
                        .flex_none()
                        .max_w(px(LIST_WIDTH * 0.6))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .when(deprecated, |el| el.line_through())
                        .child(StyledText::new(item.label.clone()).with_highlights(highlights)),
                )
                .when_some(inline, |el, detail| {
                    el.child(
                        div()
                            .flex_none()
                            .whitespace_nowrap()
                            .text_color(palette.text_tertiary)
                            .child(detail),
                    )
                })
                .child(div().flex_1())
                .when_some(right, |el, detail| {
                    el.child(
                        div()
                            .min_w(px(0.))
                            .max_w(px(LIST_WIDTH * 0.4))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px((size - 1.5).max(9.)))
                            .text_color(palette.text_tertiary)
                            .child(detail),
                    )
                })
                .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                    cx.stop_propagation();
                    view.update(cx, |view, cx| view.accept_completion(Some(row), cx));
                })
        });
    let row_h = px(row_height(size));
    let wheel_view = view.clone();
    // Trackpads scroll a few pixels at a time: whole rows once they add up.
    let wheel_rest = std::rc::Rc::new(std::cell::Cell::new(px(0.)));
    let list = div()
        .id("completions")
        .occlude()
        .on_scroll_wheel(move |event, _, cx| {
            cx.stop_propagation();
            let dy = wheel_rest.get() - event.delta.pixel_delta(row_h).y;
            let rows = (dy / row_h).trunc();
            wheel_rest.set(dy - row_h * rows);
            if rows != 0. {
                wheel_view.update(cx, |view, cx| view.scroll_completion(rows as isize, cx));
            }
        })
        .w(px(LIST_WIDTH))
        .p(px(4.))
        .flex()
        .flex_col()
        .bg(palette.overlay_bg)
        .border_1()
        .border_color(palette.border)
        .rounded(px(8.))
        .shadow_lg()
        .font_family(mono)
        .text_size(px(size))
        .text_color(palette.text)
        .children(rows);
    let docs = menu.selected_entry().and_then(|(ix, entry)| {
        let detail = entry.item.detail.clone().filter(|d| !d.trim().is_empty());
        let docs = documentation(&entry.item);
        if detail.is_none() && docs.is_none() {
            return None;
        }
        Some(
            div()
                .id("completion-docs")
                .occlude()
                .w(px(DOCS_WIDTH))
                .max_h(px(row_height(size) * VISIBLE_ROWS as f32 + 10.))
                .overflow_y_scroll()
                .p(px(10.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .bg(palette.overlay_bg)
                .border_1()
                .border_color(palette.border)
                .rounded(px(8.))
                .shadow_lg()
                .text_size(px(13.))
                .line_height(relative(1.5))
                .text_color(palette.text)
                .when_some(detail, |el, detail| {
                    el.child(
                        div()
                            .font_family(theme::fonts(cx).mono.clone())
                            .text_size(px(12.5))
                            .text_color(palette.text_secondary)
                            .child(detail),
                    )
                })
                .when_some(docs, |el, docs| {
                    el.child(
                        TextView::markdown(
                            SharedString::from(format!("completion-doc-{}-{ix}", menu.id)),
                            docs,
                        )
                        .style(super::hover::hover_text_style(&palette))
                        .shared_code_block_highlighter(crate::markdown::shared_code_highlighter(
                            &palette,
                        ))
                        .code_block_actions(|_, _, _| div()),
                    )
                }),
        )
    });
    div()
        .flex()
        .items_start()
        .gap(px(4.))
        .child(list)
        .children(docs)
        .into_any_element()
}

/// One entry of the code actions list.
#[derive(Clone, Debug)]
pub struct ActionItem {
    pub title: SharedString,
    /// "Fix", "Refactor", "Source"…
    pub kind: Option<SharedString>,
    pub preferred: bool,
    /// Why it can't be used now, if it can't.
    pub disabled: Option<SharedString>,
}

pub struct ActionsMenu {
    pub id: u64,
    /// The char the list is shown at.
    pub anchor: usize,
    pub items: Vec<ActionItem>,
    pub selected: usize,
}

impl ActionsMenu {
    pub fn select(&mut self, delta: isize) {
        let n = self.items.len() as isize;
        if n == 0 {
            return;
        }
        // Skip the ones that can't be used.
        for _ in 0..n {
            self.selected = (self.selected as isize + delta.signum()).rem_euclid(n) as usize;
            if self.items[self.selected].disabled.is_none() {
                break;
            }
        }
    }
}

pub fn actions_popover(
    view: &Entity<EditorView>,
    menu: &ActionsMenu,
    max_height: Pixels,
    cx: &App,
) -> AnyElement {
    let palette = theme::palette(cx).clone();
    let rows = menu.items.iter().enumerate().map(|(row, item)| {
        let selected = row == menu.selected;
        let enabled = item.disabled.is_none();
        let view = view.clone();
        div()
            .id(row)
            .min_h(px(ROW_HEIGHT))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(px(4.))
            .when(selected && enabled, |el| el.bg(palette.row_selected))
            .when(!selected && enabled, |el| el.hover(|s| s.bg(palette.hover)))
            .when(!enabled, |el| el.text_color(palette.text_tertiary))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .when(item.preferred, |el| el.font_weight(FontWeight::SEMIBOLD))
                    .child(item.title.clone()),
            )
            .when_some(item.disabled.clone().or(item.kind.clone()), |el, note| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(11.5))
                        .text_color(palette.text_tertiary)
                        .child(note),
                )
            })
            .when(enabled, |el| {
                el.on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                    cx.stop_propagation();
                    view.update(cx, |view, cx| view.pick_code_action(Some(row), cx));
                })
            })
    });
    div()
        .id("code-actions")
        .occlude()
        .min_w(px(260.))
        .max_w(px(520.))
        .max_h(max_height)
        .overflow_y_scroll()
        .p(px(4.))
        .flex()
        .flex_col()
        .bg(palette.overlay_bg)
        .border_1()
        .border_color(palette.border)
        .rounded(px(8.))
        .shadow_lg()
        .text_size(px(13.))
        .text_color(palette.text)
        .children(rows)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> lsp::CompletionItem {
        lsp::CompletionItem {
            label: label.into(),
            ..Default::default()
        }
    }

    fn labels(items: &[lsp::CompletionItem], typed: &str) -> Vec<String> {
        let refs: Vec<&lsp::CompletionItem> = items.iter().collect();
        filter(&refs, typed)
            .into_iter()
            .map(|(ix, _)| items[ix].label.clone())
            .collect()
    }

    #[test]
    fn filters_fuzzily_from_word_starts() {
        let items = [
            item("get_thing_done"),
            item("gather"),
            item("target"),
            item("getThing"),
            item("unrelated"),
        ];
        let found = labels(&items, "gt");
        assert!(found.contains(&"get_thing_done".to_string()));
        assert!(found.contains(&"getThing".to_string()));
        // "t" in the middle of a word doesn't start a match.
        assert!(!found.contains(&"target".to_string()));
        assert!(!found.contains(&"unrelated".to_string()));
        // Nothing typed: the server's order (sort text, else label).
        let mut sorted = [item("b"), item("a")];
        sorted[0].sort_text = Some("0".into());
        sorted[1].sort_text = Some("1".into());
        assert_eq!(labels(&sorted, ""), ["b", "a"]);
    }

    fn menu(start: usize, asked: lsp::Position) -> CompletionMenu {
        CompletionMenu::new(
            1,
            start,
            Completions {
                items: Vec::new(),
                incomplete: false,
                encoding: Encoding::Utf16,
                asked,
                resolvable: false,
            },
        )
    }

    #[test]
    fn accepting_replaces_the_typed_word() {
        // "let v = thi|" asked at col 11; the user then typed "n".
        let rope = Rope::from_str("let v = thin\n");
        let menu = menu(8, lsp::Position::new(0, 11));
        let mut it = item("thing");
        let accepted = accept(&menu, &it, &rope, 12);
        assert_eq!((accepted.range, accepted.text.as_str()), (8..12, "thing"));
        // A text edit over the word as it was, plus an import.
        it.text_edit = Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit::new(
            lsp::Range::new(lsp::Position::new(0, 8), lsp::Position::new(0, 11)),
            "thing()".into(),
        )));
        it.additional_text_edits = Some(vec![lsp::TextEdit::new(
            lsp::Range::new(lsp::Position::new(0, 0), lsp::Position::new(0, 0)),
            "use x::thing;\n".into(),
        )]);
        let accepted = accept(&menu, &it, &rope, 12);
        assert_eq!(accepted.range, 8..12);
        assert_eq!(
            accepted.additional,
            vec![(0..0, "use x::thing;\n".to_string())]
        );
    }

    #[test]
    fn accepting_snippets_and_replace_ranges() {
        // "foo.ba|r()": the replace range covers the rest of the word.
        let rope = Rope::from_str("foo.bar()\n");
        let menu = menu(4, lsp::Position::new(0, 6));
        let mut it = item("baz");
        it.text_edit = Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit::new(
            lsp::Range::new(lsp::Position::new(0, 4), lsp::Position::new(0, 7)),
            "baz(${1:n})$0".into(),
        )));
        it.insert_text_format = Some(lsp::InsertTextFormat::SNIPPET);
        let accepted = accept(&menu, &it, &rope, 6);
        assert_eq!(accepted.range, 4..7);
        assert_eq!(accepted.text, "baz(n)");
        assert_eq!(
            accepted.snippet.unwrap().stops,
            vec![vec![4..5], vec![6..6]]
        );
    }

    #[test]
    fn buffer_words_skip_the_current_word() {
        let rope = Rope::from_str("alpha beta alphabet al 42abc\n");
        let words: Vec<String> = buffer_words(&rope, "al", 20..22)
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert_eq!(words, ["alpha", "beta", "alphabet"]);
    }
}
