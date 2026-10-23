//! Cmd+T: fuzzy "go to file". The index is built in the background when the
//! window opens, so the first Cmd+T is as fast as the rest.

use std::path::PathBuf;
use std::sync::Arc;

use editor_project::{FileFinder, FileMatch, split_line_suffix};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, HighlightStyle,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, ScrollStrategy,
    SharedString, StatefulInteractiveElement, Styled, StyledText, Subscription, Task,
    UniformListScrollHandle, Window, div, prelude::FluentBuilder, px, svg, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::theme;

const MAX_RESULTS: usize = 200;
const ROW_HEIGHT: f32 = 44.;
const VISIBLE_ROWS: usize = 10;

pub enum FinderEvent {
    Open {
        path: PathBuf,
        line: Option<(u32, Option<u32>)>,
    },
    Dismiss,
}

pub struct FinderView {
    root: PathBuf,
    finder: FileFinder,
    input: Entity<InputState>,
    results: Vec<FileMatch>,
    selected: usize,
    scroll: UniformListScrollHandle,
    /// Recently opened files (absolute), most recent first, shown for an
    /// empty query.
    recent: Vec<PathBuf>,
    _ticker: Task<()>,
    _subscription: Subscription,
}

impl EventEmitter<FinderEvent> for FinderView {}

impl Focusable for FinderView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl FinderView {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = mpsc::unbounded::<()>();
        let finder = FileFinder::new(root.clone(), move || {
            let _ = tx.unbounded_send(());
        });
        let ticker = cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                // Coalesce bursts of notifications into one tick.
                while rx.try_recv().is_ok() {}
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        });
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Go to file…"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, _window, cx| match event {
                InputEvent::Change => this.query_changed(cx),
                InputEvent::PressEnter { .. } => this.confirm(cx),
                _ => {}
            },
        );
        Self {
            root,
            finder,
            input,
            results: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            recent: Vec::new(),
            _ticker: ticker,
            _subscription: subscription,
        }
    }

    /// Called each time the finder opens.
    pub fn reset(&mut self, recent: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.recent = recent;
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.selected = 0;
        self.query_changed(cx);
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn query(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn query_changed(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        self.finder.set_query(&query);
        self.finder.tick(5);
        self.refresh_results(cx);
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.finder.tick(5) || self.results.is_empty() {
            self.refresh_results(cx);
        }
    }

    fn refresh_results(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        let (path_query, _) = split_line_suffix(query.trim());
        if path_query.is_empty() {
            let root = self.root.clone();
            let recent: Vec<FileMatch> = self
                .recent
                .iter()
                .filter_map(|p| p.strip_prefix(&root).ok())
                .map(|p| FileMatch {
                    path: Arc::from(p.to_string_lossy().as_ref()),
                    indices: Vec::new(),
                })
                .collect();
            let mut results = recent;
            for m in self.finder.results(MAX_RESULTS) {
                if results.len() >= MAX_RESULTS {
                    break;
                }
                if !results.iter().any(|r| r.path == m.path) {
                    results.push(m);
                }
            }
            self.results = results;
        } else {
            self.results = self.finder.results(MAX_RESULTS);
        }
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
        cx.notify();
    }

    fn select(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.results.is_empty() {
            return;
        }
        let n = self.results.len() as isize;
        self.selected = ((self.selected as isize + delta).rem_euclid(n)) as usize;
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Top);
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        let (_, line) = split_line_suffix(query.trim());
        if let Some(m) = self.results.get(self.selected) {
            let path = self.root.join(m.path.as_ref());
            cx.emit(FinderEvent::Open { path, line });
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let ctrl = ks.modifiers.control;
        match ks.key.as_str() {
            "up" => self.select(-1, cx),
            "down" => self.select(1, cx),
            "p" if ctrl => self.select(-1, cx),
            "n" if ctrl => self.select(1, cx),
            "pageup" => self.select(-(VISIBLE_ROWS as isize), cx),
            "pagedown" => self.select(VISIBLE_ROWS as isize, cx),
            "escape" => cx.emit(FinderEvent::Dismiss),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for FinderView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let results = self.results.clone();
        let selected = self.selected;
        let count = self.finder.match_count();
        let indexing = self.finder.is_indexing();
        let status: SharedString = if indexing {
            format!("Indexing… {} files", self.finder.file_count()).into()
        } else if self.query(cx).trim().is_empty() {
            format!("{} files", self.finder.file_count()).into()
        } else {
            format!("{count} of {}", self.finder.file_count()).into()
        };
        let list_height = px(ROW_HEIGHT * results.len().clamp(1, VISIBLE_ROWS) as f32);

        div()
            .id("finder")
            .key_context("Finder")
            .capture_key_down(cx.listener(Self::on_key))
            .w(px(620.))
            .flex()
            .flex_col()
            .bg(palette.overlay_bg)
            .border_1()
            .border_color(palette.border)
            .rounded(px(12.))
            .shadow_lg()
            .overflow_hidden()
            .child(
                div()
                    .p(px(10.))
                    .border_b_1()
                    .border_color(palette.border)
                    .child(
                        Input::new(&self.input)
                            .appearance(false)
                            .text_size(px(16.))
                            .prefix(
                                svg()
                                    .path("icons/search.svg")
                                    .size(px(16.))
                                    .text_color(palette.text_secondary),
                            ),
                    ),
            )
            .child(div().h(list_height).when(!results.is_empty(), |el| {
                el.child(
                    uniform_list(
                        "finder-results",
                        results.len(),
                        cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                            range
                                .map(|ix| {
                                    render_result(ix, &results[ix], ix == selected, &palette, cx)
                                })
                                .collect()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .h(list_height),
                )
            }))
            .child(
                div()
                    .px(px(14.))
                    .py(px(6.))
                    .border_t_1()
                    .border_color(theme::palette(cx).border)
                    .text_size(px(11.))
                    .text_color(theme::palette(cx).text_tertiary)
                    .child(status),
            )
    }
}

fn render_result(
    ix: usize,
    m: &FileMatch,
    selected: bool,
    palette: &theme::Palette,
    cx: &mut Context<FinderView>,
) -> gpui::AnyElement {
    let path: &str = &m.path;
    let (dir, name) = match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    };
    let name_start_char = path[..path.len() - name.len()].chars().count() as u32;
    // nucleo gives char indices into the whole path; split them per part.
    let to_byte_ranges =
        |text: &str, offset: u32| -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
            let mut out = Vec::new();
            for (ci, (bi, ch)) in text.char_indices().enumerate() {
                if m.indices.binary_search(&(offset + ci as u32)).is_ok() {
                    out.push((
                        bi..bi + ch.len_utf8(),
                        HighlightStyle {
                            color: Some(palette.accent),
                            font_weight: Some(gpui::FontWeight::BOLD),
                            ..Default::default()
                        },
                    ));
                }
            }
            out
        };
    let name_text = StyledText::new(SharedString::from(name.to_string()))
        .with_highlights(to_byte_ranges(name, name_start_char));
    let dir_text = StyledText::new(SharedString::from(if dir.is_empty() {
        "./".to_string()
    } else {
        dir.to_string()
    }))
    .with_highlights(to_byte_ranges(dir, 0));
    let row = div()
        .w_full()
        .h(px(ROW_HEIGHT))
        .px(px(10.))
        .flex()
        .flex_col()
        .justify_center()
        .rounded(px(7.))
        .when(selected, |el| el.bg(palette.row_selected))
        .when(!selected, |el| el.hover(|s| s.bg(palette.hover)))
        .child(
            div()
                .text_size(px(13.))
                .text_color(palette.text)
                .child(name_text),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(palette.text_secondary)
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(dir_text),
        );
    div()
        .id(ix)
        .w_full()
        .px(px(6.))
        .child(row)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.selected = ix;
            this.confirm(cx);
        }))
        .into_any_element()
}
