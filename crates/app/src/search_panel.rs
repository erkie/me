//! Cmd+Shift+F: project-wide search and replace in the sidebar. Results
//! stream in from ripgrep's engine on background threads.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use editor_project::{FileMatches, SearchOptions, SearchStats};
use editor_text::SearchQuery;
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, HighlightStyle, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, PromptLevel, Render, SharedString,
    StatefulInteractiveElement, Styled, StyledText, Subscription, Task, UniformListScrollHandle,
    Window, div, px, svg, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::find_bar::{text_button, toggle_button};
use crate::theme;

const MAX_MATCHES: usize = 20_000;
const ROW_HEIGHT: f32 = 22.;

pub enum SearchPanelEvent {
    Open {
        path: PathBuf,
        line: u32,
        query: SearchQuery,
    },
    /// Replace in these files: open tabs get an undoable in-editor replace,
    /// the rest are rewritten on disk.
    ReplaceAll {
        query: SearchQuery,
        replacement: String,
        files: Vec<PathBuf>,
    },
    Dismiss,
}

#[derive(Clone)]
enum Row {
    File {
        path: PathBuf,
        relative: SharedString,
        count: usize,
        collapsed: bool,
    },
    Line {
        path: PathBuf,
        line: u64,
        preview: SharedString,
        ranges: Vec<std::ops::Range<usize>>,
    },
}

pub struct SearchPanel {
    root: PathBuf,
    query_input: Entity<InputState>,
    replace_input: Entity<InputState>,
    include_input: Entity<InputState>,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    results: Vec<FileMatches>,
    collapsed: HashSet<PathBuf>,
    rows: Vec<Row>,
    running: bool,
    stats: Option<SearchStats>,
    error: Option<SharedString>,
    cancel: Arc<AtomicBool>,
    task: Option<Task<()>>,
    debounce: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    last_query: Option<SearchQuery>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SearchPanelEvent> for SearchPanel {}

impl SearchPanel {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let replace_input = cx.new(|cx| InputState::new(window, cx).placeholder("Replace"));
        let include_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("Files to include, e.g. *.md, src/"));
        let subscriptions = vec![
            cx.subscribe_in(
                &query_input,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.schedule_search(cx);
                    }
                },
            ),
            cx.subscribe_in(
                &include_input,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.schedule_search(cx);
                    }
                },
            ),
        ];
        Self {
            root,
            query_input,
            replace_input,
            include_input,
            case_sensitive: false,
            whole_word: false,
            regex: false,
            results: Vec::new(),
            collapsed: HashSet::new(),
            rows: Vec::new(),
            running: false,
            stats: None,
            error: None,
            cancel: Arc::new(AtomicBool::new(false)),
            task: None,
            debounce: None,
            scroll: UniformListScrollHandle::new(),
            last_query: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn show(&mut self, seed: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(seed) = seed.filter(|s| !s.is_empty()) {
            self.query_input
                .update(cx, |input, cx| input.set_value(seed, window, cx));
            self.schedule_search(cx);
        }
        self.query_input
            .update(cx, |input, cx| input.focus(window, cx));
    }

    fn query(&self, cx: &App) -> Option<SearchQuery> {
        let pattern = self.query_input.read(cx).value().to_string();
        if pattern.is_empty() {
            return None;
        }
        Some(SearchQuery {
            pattern,
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
        })
    }

    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(180))
                .await;
            this.update(cx, |this, cx| this.run(cx)).ok();
        }));
    }

    pub fn rerun(&mut self, cx: &mut Context<Self>) {
        self.run(cx);
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        self.results.clear();
        self.rows.clear();
        self.error = None;
        self.stats = None;
        let Some(query) = self.query(cx) else {
            self.running = false;
            self.last_query = None;
            cx.notify();
            return;
        };
        if let Err(err) = query.compile() {
            self.error = Some(err.into());
            self.running = false;
            cx.notify();
            return;
        }
        let include: Vec<String> = self
            .include_input
            .read(cx)
            .value()
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| {
                // "src/" means everything under src.
                if s.ends_with('/') {
                    format!("{s}**")
                } else {
                    s.to_string()
                }
            })
            .collect();
        let options = SearchOptions {
            query: query.clone(),
            include,
            exclude: Vec::new(),
            max_matches: MAX_MATCHES,
        };
        self.last_query = Some(query);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        self.running = true;
        let root = self.root.clone();
        let (tx, mut rx) = mpsc::unbounded::<FileMatches>();
        let search = cx.background_executor().spawn(async move {
            editor_project::search(&root, &options, &cancel, |found| {
                let _ = tx.unbounded_send(found);
            })
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let mut last_flush = Instant::now();
            let mut pending = Vec::new();
            while let Some(found) = rx.next().await {
                pending.push(found);
                if last_flush.elapsed() > Duration::from_millis(50) {
                    last_flush = Instant::now();
                    let batch = std::mem::take(&mut pending);
                    if this
                        .update(cx, |this, cx| this.add_results(batch, cx))
                        .is_err()
                    {
                        return;
                    }
                }
            }
            let stats = search.await;
            this.update(cx, |this, cx| {
                this.add_results(pending, cx);
                this.running = false;
                match stats {
                    Ok(stats) => this.stats = Some(stats),
                    Err(err) => this.error = Some(err.into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn add_results(&mut self, batch: Vec<FileMatches>, cx: &mut Context<Self>) {
        if batch.is_empty() {
            return;
        }
        self.results.extend(batch);
        self.results.sort_by(|a, b| a.relative.cmp(&b.relative));
        self.rebuild_rows();
        cx.notify();
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        for file in &self.results {
            let collapsed = self.collapsed.contains(&file.path);
            rows.push(Row::File {
                path: file.path.clone(),
                relative: file.relative.clone().into(),
                count: file.match_count(),
                collapsed,
            });
            if !collapsed {
                for line in &file.lines {
                    let (preview, ranges) = compact_preview(&line.preview, &line.ranges);
                    rows.push(Row::Line {
                        path: file.path.clone(),
                        line: line.line_number,
                        preview,
                        ranges,
                    });
                }
            }
        }
        self.rows = rows;
    }

    fn toggle_option(&mut self, which: u8, cx: &mut Context<Self>) {
        match which {
            0 => self.case_sensitive = !self.case_sensitive,
            1 => self.whole_word = !self.whole_word,
            _ => self.regex = !self.regex,
        }
        self.run(cx);
    }

    fn replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(query) = self.last_query.clone() else {
            return;
        };
        let replacement = self.replace_input.read(cx).value().to_string();
        let files: Vec<PathBuf> = self.results.iter().map(|f| f.path.clone()).collect();
        let total: usize = self.results.iter().map(|f| f.match_count()).sum();
        if files.is_empty() {
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Replace {total} matches in {} files?", files.len()),
            Some("Open files can be undone; files on disk are rewritten."),
            &["Replace", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            this.update(cx, |_, cx| {
                cx.emit(SearchPanelEvent::ReplaceAll {
                    query,
                    replacement,
                    files,
                })
            })
            .ok();
        })
        .detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => cx.emit(SearchPanelEvent::Dismiss),
            "enter" => self.run(cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for SearchPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let mono = theme::fonts(cx).mono.clone();
        let rows = self.rows.clone();
        let query = self.last_query.clone();
        let input_box = |input: &Entity<InputState>| {
            div()
                .h(px(28.))
                .px(px(4.))
                .rounded(px(6.))
                .border_1()
                .border_color(palette.border)
                .bg(palette.editor_bg)
                .child(Input::new(input).appearance(false))
        };
        let summary: SharedString = if let Some(err) = &self.error {
            err.clone()
        } else if self.running {
            format!("Searching… {} files", self.results.len()).into()
        } else if let Some(stats) = self.stats {
            let more = if stats.truncated { "+" } else { "" };
            format!(
                "{}{more} results in {} files",
                stats.matches, stats.files_matched
            )
            .into()
        } else {
            "".into()
        };

        div()
            .id("search-panel")
            .key_context("SearchPanel")
            .capture_key_down(cx.listener(Self::on_key))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .px(px(10.))
                    .pb(px(8.))
                    .child(input_box(&self.query_input))
                    .child(
                        div()
                            .flex()
                            .gap(px(4.))
                            .child(toggle_button(
                                "p-case",
                                "Aa",
                                "Match case",
                                self.case_sensitive,
                                &palette,
                                cx.listener(|this, _, _, cx| this.toggle_option(0, cx)),
                            ))
                            .child(toggle_button(
                                "p-word",
                                "W",
                                "Whole word",
                                self.whole_word,
                                &palette,
                                cx.listener(|this, _, _, cx| this.toggle_option(1, cx)),
                            ))
                            .child(toggle_button(
                                "p-regex",
                                ".*",
                                "Regular expression",
                                self.regex,
                                &palette,
                                cx.listener(|this, _, _, cx| this.toggle_option(2, cx)),
                            )),
                    )
                    .child(input_box(&self.replace_input))
                    .child(input_box(&self.include_input))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(palette.text_secondary)
                                    .child(summary),
                            )
                            .child(text_button(
                                "p-replace-all",
                                "Replace All",
                                &palette,
                                cx.listener(|this, _, window, cx| this.replace_all(window, cx)),
                            )),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .border_t_1()
                    .border_color(palette.border)
                    .child(
                        uniform_list(
                            "search-results",
                            rows.len(),
                            cx.processor(
                                move |_this, range: std::ops::Range<usize>, _window, cx| {
                                    range
                                        .map(|ix| {
                                            render_row(
                                                ix,
                                                &rows[ix],
                                                query.as_ref(),
                                                &palette,
                                                &mono,
                                                cx,
                                            )
                                        })
                                        .collect()
                                },
                            ),
                        )
                        .track_scroll(&self.scroll)
                        .size_full(),
                    ),
            )
    }
}

/// Drops indentation and, when the first match is far right, the text
/// before it, so matches are visible in a narrow sidebar.
fn compact_preview(
    preview: &str,
    ranges: &[std::ops::Range<usize>],
) -> (SharedString, Vec<std::ops::Range<usize>>) {
    const LEAD: usize = 16;
    let trimmed = preview.trim_end();
    let mut cut = trimmed.len() - trimmed.trim_start().len();
    if let Some(first) = ranges.first()
        && first.start > cut + LEAD * 2
    {
        cut = first.start - LEAD;
        while !trimmed.is_char_boundary(cut) {
            cut -= 1;
        }
    }
    let prefix = if cut > 0 && cut > trimmed.len() - trimmed.trim_start().len() {
        "…"
    } else {
        ""
    };
    let text = format!("{prefix}{}", &trimmed[cut.min(trimmed.len())..]);
    let shift = prefix.len() as isize - cut as isize;
    let ranges = ranges
        .iter()
        .filter(|r| r.start >= cut && r.end <= trimmed.len())
        .map(|r| (r.start as isize + shift) as usize..(r.end as isize + shift) as usize)
        .collect();
    (text.into(), ranges)
}

fn render_row(
    ix: usize,
    row: &Row,
    query: Option<&SearchQuery>,
    palette: &theme::Palette,
    mono: &SharedString,
    cx: &mut Context<SearchPanel>,
) -> gpui::AnyElement {
    match row.clone() {
        Row::File {
            path,
            relative,
            count,
            collapsed,
        } => div()
            .id(ix)
            .w_full()
            .h(px(ROW_HEIGHT))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(4.))
            .text_size(px(12.))
            .hover(|s| s.bg(palette.hover))
            .child(
                svg()
                    .path(if collapsed {
                        "icons/chevron-right.svg"
                    } else {
                        "icons/chevron-down.svg"
                    })
                    .size(px(11.))
                    .text_color(palette.text_tertiary),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(relative),
            )
            .child(
                div()
                    .px(px(6.))
                    .rounded_full()
                    .bg(palette.hover)
                    .text_size(px(10.))
                    .text_color(palette.text_secondary)
                    .child(count.to_string()),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.collapsed.remove(&path) {
                    this.collapsed.insert(path.clone());
                }
                this.rebuild_rows();
                cx.notify();
            }))
            .into_any_element(),
        Row::Line {
            path,
            line,
            preview,
            ranges,
        } => {
            let highlights: Vec<_> = ranges
                .iter()
                .filter(|r| r.end <= preview.len())
                .map(|r| {
                    (
                        r.clone(),
                        HighlightStyle {
                            background_color: Some(palette.find_match),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            let query = query.cloned();
            div()
                .id(ix)
                .w_full()
                .h(px(ROW_HEIGHT))
                .pl(px(26.))
                .pr(px(10.))
                .flex()
                .items_center()
                .gap(px(8.))
                .hover(|s| s.bg(palette.hover))
                .child(
                    div()
                        .w(px(32.))
                        .flex_none()
                        .text_size(px(10.5))
                        .text_color(palette.text_tertiary)
                        .child(line.to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(11.5))
                        .font_family(mono.clone())
                        .child(StyledText::new(preview).with_highlights(highlights)),
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    if let Some(query) = query.clone() {
                        cx.emit(SearchPanelEvent::Open {
                            path: path.clone(),
                            line: line as u32,
                            query,
                        });
                    }
                }))
                .into_any_element()
        }
    }
}
