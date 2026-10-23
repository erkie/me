//! The sidebar file tree: lazily listed, virtualized, ignore-aware.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use editor_project::{DirEntry, list_dir};
use gpui::{
    App, ClickEvent, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, ScrollStrategy, SharedString, StatefulInteractiveElement,
    Styled, UniformListScrollHandle, Window, actions, div, prelude::FluentBuilder, px, svg,
    uniform_list,
};

use crate::theme;

actions!(
    file_tree,
    [SelectPrev, SelectNext, Expand, Collapse, OpenSelected]
);

pub const ROW_HEIGHT: f32 = 24.;

pub enum FileTreeEvent {
    /// Open a file. `pin` is false for a single click (preview tab).
    Open { path: PathBuf, pin: bool },
}

#[derive(Clone)]
struct Row {
    entry: DirEntry,
    depth: usize,
}

pub struct FileTree {
    root: PathBuf,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    listings: HashMap<PathBuf, Vec<DirEntry>>,
    expanded: HashSet<PathBuf>,
    rows: Vec<Row>,
    selected: Option<PathBuf>,
}

impl EventEmitter<FileTreeEvent> for FileTree {}

impl Focusable for FileTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl FileTree {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            root: root.clone(),
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            listings: HashMap::new(),
            expanded: HashSet::new(),
            rows: Vec::new(),
            selected: None,
        };
        this.expanded.insert(root);
        this.rebuild();
        this
    }

    fn listing(&mut self, dir: &Path) -> Vec<DirEntry> {
        if let Some(entries) = self.listings.get(dir) {
            return entries.clone();
        }
        let entries = list_dir(dir).unwrap_or_default();
        self.listings.insert(dir.to_path_buf(), entries.clone());
        entries
    }

    fn rebuild(&mut self) {
        // Depth-first, keeping directory order.
        fn walk(this: &mut FileTree, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
            for entry in this.listing(dir) {
                let expanded = entry.is_dir && this.expanded.contains(&entry.path);
                let path = entry.path.clone();
                rows.push(Row { entry, depth });
                if expanded {
                    walk(this, &path, depth + 1, rows);
                }
            }
        }
        let mut rows = Vec::new();
        let root = self.root.clone();
        walk(self, &root, 0, &mut rows);
        self.rows = rows;
    }

    /// Re-lists every expanded directory (after the window regains focus,
    /// a save, or a file operation).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.listings.clear();
        self.rebuild();
        cx.notify();
    }

    fn toggle(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
        }
        self.rebuild();
        cx.notify();
    }

    /// Expands the parents of `path` and selects it.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !path.starts_with(&self.root) {
            return;
        }
        let mut changed = false;
        for ancestor in path.ancestors().skip(1) {
            if !ancestor.starts_with(&self.root) {
                break;
            }
            changed |= self.expanded.insert(ancestor.to_path_buf());
        }
        if changed {
            self.rebuild();
        }
        self.selected = Some(path.to_path_buf());
        if let Some(ix) = self.rows.iter().position(|r| r.entry.path == path) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }

    pub fn select_quietly(&mut self, path: Option<&Path>, cx: &mut Context<Self>) {
        let path = path.map(Path::to_path_buf);
        if self.selected != path {
            self.selected = path;
            cx.notify();
        }
    }

    fn click(
        &mut self,
        ix: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.rows.get(ix).cloned() else {
            return;
        };
        window.focus(&self.focus_handle, cx);
        self.selected = Some(row.entry.path.clone());
        if row.entry.is_dir {
            self.toggle(&row.entry.path, cx);
        } else {
            let pin = event.click_count() >= 2;
            cx.emit(FileTreeEvent::Open {
                path: row.entry.path,
                pin,
            });
        }
        cx.notify();
    }

    fn selected_ix(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.rows.iter().position(|r| &r.entry.path == selected)
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let ix = match self.selected_ix() {
            Some(ix) => (ix as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize,
            None => 0,
        };
        self.selected = Some(self.rows[ix].entry.path.clone());
        self.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        cx.notify();
    }

    fn expand_selected(&mut self, expand: bool, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_ix() else { return };
        let row = self.rows[ix].clone();
        if row.entry.is_dir {
            let is_expanded = self.expanded.contains(&row.entry.path);
            if expand != is_expanded {
                self.toggle(&row.entry.path, cx);
                return;
            }
        }
        if !expand && let Some(parent) = row.entry.path.parent().filter(|p| *p != self.root) {
            self.selected = Some(parent.to_path_buf());
            cx.notify();
        }
    }

    fn open_selected(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_ix() else { return };
        let row = self.rows[ix].clone();
        if row.entry.is_dir {
            self.toggle(&row.entry.path, cx);
        } else {
            cx.emit(FileTreeEvent::Open {
                path: row.entry.path,
                pin: true,
            });
        }
    }
}

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("FileTree");
    cx.bind_keys([
        gpui::KeyBinding::new("up", SelectPrev, ctx),
        gpui::KeyBinding::new("down", SelectNext, ctx),
        gpui::KeyBinding::new("right", Expand, ctx),
        gpui::KeyBinding::new("left", Collapse, ctx),
        gpui::KeyBinding::new("enter", OpenSelected, ctx),
        gpui::KeyBinding::new("secondary-down", OpenSelected, ctx),
    ]);
}

impl Render for FileTree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let focused = self.focus_handle.contains_focused(window, cx);
        let rows = self.rows.clone();
        let selected = self.selected.clone();
        let expanded = self.expanded.clone();
        div()
            .key_context("FileTree")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &Expand, _, cx| this.expand_selected(true, cx)))
            .on_action(cx.listener(|this, _: &Collapse, _, cx| this.expand_selected(false, cx)))
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| this.open_selected(cx)))
            .size_full()
            .child(
                uniform_list(
                    "file-tree",
                    rows.len(),
                    cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                        range
                            .map(|ix| {
                                let row = &rows[ix];
                                let is_selected = selected.as_ref() == Some(&row.entry.path);
                                let is_expanded = expanded.contains(&row.entry.path);
                                render_row(ix, row, is_selected, is_expanded, focused, &palette, cx)
                            })
                            .collect()
                    }),
                )
                .track_scroll(&self.scroll)
                .size_full()
                .pb(px(8.)),
            )
    }
}

fn render_row(
    ix: usize,
    row: &Row,
    selected: bool,
    expanded: bool,
    focused: bool,
    palette: &theme::Palette,
    cx: &mut Context<FileTree>,
) -> gpui::AnyElement {
    let entry = &row.entry;
    let text_color = if entry.ignored {
        palette.ignored_text
    } else {
        palette.text
    };
    let icon_color = if entry.ignored {
        palette.ignored_text
    } else {
        palette.text_secondary
    };
    let chevron = if expanded {
        "icons/chevron-down.svg"
    } else {
        "icons/chevron-right.svg"
    };
    let icon = if entry.is_dir {
        if expanded {
            "icons/folder-open.svg"
        } else {
            "icons/folder.svg"
        }
    } else {
        file_icon(&entry.name)
    };
    let name: SharedString = entry.name.clone().into();
    let row_el = div()
        .w_full()
        .h(px(ROW_HEIGHT))
        .pl(px(6. + row.depth as f32 * 14.))
        .pr(px(8.))
        .flex()
        .items_center()
        .gap(px(4.))
        .rounded(px(5.))
        .text_size(px(13.))
        .text_color(text_color)
        .when(selected, |el| {
            el.bg(if focused {
                palette.row_selected
            } else {
                palette.row_selected_inactive
            })
        })
        .when(!selected, |el| el.hover(|s| s.bg(palette.hover)))
        .child(div().w(px(12.)).flex_none().when(entry.is_dir, |el| {
            el.child(
                svg()
                    .path(chevron)
                    .size(px(11.))
                    .text_color(palette.text_tertiary),
            )
        }))
        .child(
            svg()
                .path(icon)
                .size(px(14.))
                .flex_none()
                .text_color(icon_color),
        )
        .child(
            div()
                .flex_1()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(name),
        );
    div()
        .id(ix)
        .w_full()
        .px(px(8.))
        .child(row_el)
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            this.click(ix, event, window, cx)
        }))
        .into_any_element()
}

fn file_icon(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".txt") {
        "icons/file-text.svg"
    } else {
        "icons/file.svg"
    }
}
