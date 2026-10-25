//! Cmd+K (or Cmd+Shift+P): the command palette. Every command is an action,
//! so running one is the same as using its menu item or key binding.

use gpui::{
    Action, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, HighlightStyle,
    InteractiveElement, IntoElement, KeyDownEvent, Keystroke, ParentElement, Render,
    ScrollStrategy, SharedString, StatefulInteractiveElement, Styled, StyledText, Subscription,
    UniformListScrollHandle, Window, div, prelude::FluentBuilder, px, svg, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Matcher, Utf32Str};

use crate::actions::*;
use crate::{platform, settings, theme};

const ROW_HEIGHT: f32 = 34.;
const VISIBLE_ROWS: usize = 12;

pub enum PaletteEvent {
    Run(Box<dyn Action>),
    Dismiss,
}

struct Command {
    label: SharedString,
    action: Box<dyn Action>,
    keys: Option<SharedString>,
}

struct Match {
    command: usize,
    indices: Vec<u32>,
}

pub struct CommandPalette {
    input: Entity<InputState>,
    commands: Vec<Command>,
    matches: Vec<Match>,
    selected: usize,
    scroll: UniformListScrollHandle,
    matcher: Matcher,
    _subscription: Subscription,
}

impl EventEmitter<PaletteEvent> for CommandPalette {}

impl Focusable for CommandPalette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

/// Every command, in the order shown for an empty query.
fn commands(cx: &App) -> Vec<(String, Box<dyn Action>)> {
    let word_wrap = if settings::get(cx).word_wrap {
        "View: Turn Word Wrap Off"
    } else {
        "View: Turn Word Wrap On"
    };
    let mut list: Vec<(String, Box<dyn Action>)> = vec![
        ("Go to File…".into(), Box::new(ToggleFileFinder)),
        ("Go to Line…".into(), Box::new(GoToLine)),
        ("Go to Symbol in File…".into(), Box::new(GoToSymbol)),
        (
            "Go to Symbol in Project…".into(),
            Box::new(GoToProjectSymbol),
        ),
        ("Go to Definition".into(), Box::new(GoToDefinition)),
        ("Find References".into(), Box::new(FindReferences)),
        ("Rename Symbol…".into(), Box::new(RenameSymbol)),
        ("Show Hover".into(), Box::new(ShowHover)),
        ("Show Completions".into(), Box::new(ShowCompletions)),
        (
            "Code Actions / Quick Fix…".into(),
            Box::new(ShowCodeActions),
        ),
        ("Show Problems".into(), Box::new(ShowProblems)),
        ("Go to Next Problem".into(), Box::new(NextProblem)),
        ("Go to Previous Problem".into(), Box::new(PreviousProblem)),
        ("Go Back".into(), Box::new(NavigateBack)),
        ("Go Forward".into(), Box::new(NavigateForward)),
        (
            "Language Server: Restart".into(),
            Box::new(RestartLanguageServer),
        ),
        ("Language Server: Stop".into(), Box::new(StopLanguageServer)),
        ("Check for Updates".into(), Box::new(CheckForUpdates)),
        ("Restart to Update".into(), Box::new(RestartToUpdate)),
        ("Find…".into(), Box::new(Find)),
        ("Find in Project…".into(), Box::new(ToggleProjectSearch)),
        (word_wrap.into(), Box::new(ToggleWordWrap)),
        ("View: Toggle Sidebar".into(), Box::new(ToggleSidebar)),
        (
            "View: Toggle Markdown Preview".into(),
            Box::new(TogglePreview),
        ),
        ("View: Bigger Text".into(), Box::new(IncreaseFontSize)),
        ("View: Smaller Text".into(), Box::new(DecreaseFontSize)),
        ("View: Default Text Size".into(), Box::new(ResetFontSize)),
        (
            format!("File: {}", platform::REVEAL_LABEL),
            Box::new(RevealInFinder),
        ),
        ("File: Reveal in Sidebar".into(), Box::new(RevealInSidebar)),
        ("File: Copy Path".into(), Box::new(CopyPath)),
        (
            "File: Copy Relative Path".into(),
            Box::new(CopyRelativePath),
        ),
        ("File: New File".into(), Box::new(NewFile)),
        ("File: New Window".into(), Box::new(NewWindow)),
        ("File: Open…".into(), Box::new(Open)),
        ("File: Open Folder…".into(), Box::new(OpenFolder)),
        ("File: Save".into(), Box::new(Save)),
        ("File: Save As…".into(), Box::new(SaveAs)),
        ("File: Close Tab".into(), Box::new(CloseTab)),
        ("File: Close Other Tabs".into(), Box::new(CloseOtherTabs)),
        (
            "File: Close Tabs to the Right".into(),
            Box::new(CloseTabsToTheRight),
        ),
        ("File: Reopen Closed Tab".into(), Box::new(ReopenClosedTab)),
        ("Edit: Toggle Comment".into(), Box::new(ToggleComment)),
        ("Edit: Duplicate Line".into(), Box::new(DuplicateLine)),
        ("Edit: Delete Line".into(), Box::new(DeleteLine)),
        ("Edit: Move Line Up".into(), Box::new(MoveLineUp)),
        ("Edit: Move Line Down".into(), Box::new(MoveLineDown)),
        ("Edit: Indent".into(), Box::new(IndentLines)),
        ("Edit: Outdent".into(), Box::new(OutdentLines)),
        ("Selection: Expand to Line".into(), Box::new(SelectLine)),
        (
            "Selection: Split into Lines".into(),
            Box::new(SplitIntoLines),
        ),
        (
            "Selection: Add Next Occurrence".into(),
            Box::new(AddNextOccurrence),
        ),
        (
            "Selection: Skip Occurrence".into(),
            Box::new(SkipOccurrence),
        ),
        (
            "Selection: Select All Occurrences".into(),
            Box::new(SelectAllOccurrences),
        ),
        (
            "Selection: Add Cursor Above".into(),
            Box::new(AddCursorAbove),
        ),
        (
            "Selection: Add Cursor Below".into(),
            Box::new(AddCursorBelow),
        ),
        ("Find: Next".into(), Box::new(FindNext)),
        ("Find: Previous".into(), Box::new(FindPrevious)),
        (
            "Find: Use Selection for Find".into(),
            Box::new(UseSelectionForFind),
        ),
    ];
    for (label, hard_tabs, width) in [
        ("Indentation: Spaces (2)", false, 2),
        ("Indentation: Spaces (4)", false, 4),
        ("Indentation: Spaces (8)", false, 8),
        ("Indentation: Tabs", true, 4),
    ] {
        list.push((label.into(), Box::new(SetIndentation { hard_tabs, width })));
    }
    list.push((
        "Set Syntax: Plain Text".into(),
        Box::new(SetLanguage { name: None }),
    ));
    for language in me_syntax::LANGUAGES {
        list.push((
            format!("Set Syntax: {}", language.name),
            Box::new(SetLanguage {
                name: Some(language.name.into()),
            }),
        ));
    }
    list.extend([
        (
            "Me: Install 'me' Command…".to_string(),
            Box::new(InstallCli) as Box<dyn Action>,
        ),
        (
            "Me: Make Me the Default Editor…".into(),
            Box::new(MakeDefaultEditor),
        ),
        ("Me: About".into(), Box::new(About)),
        ("Me: Quit".into(), Box::new(Quit)),
    ]);
    list
}

/// A keystroke as shown in menus: `⇧⌘F` on macOS, `Ctrl+Shift+F` elsewhere.
pub fn format_keystroke(ks: &Keystroke) -> String {
    let key = match ks.key.as_str() {
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        "left" => "←".to_string(),
        "right" => "→".to_string(),
        "enter" => "↩".to_string(),
        "escape" => "Esc".to_string(),
        "backspace" => "⌫".to_string(),
        "delete" => "⌦".to_string(),
        "tab" => "⇥".to_string(),
        "space" => "Space".to_string(),
        "pageup" => "PgUp".to_string(),
        "pagedown" => "PgDn".to_string(),
        k if k.len() == 1 => k.to_uppercase(),
        k => {
            let mut c = k.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        }
    };
    let m = &ks.modifiers;
    let mut out = String::new();
    if cfg!(target_os = "macos") {
        for (on, sym) in [
            (m.control, "⌃"),
            (m.alt, "⌥"),
            (m.shift, "⇧"),
            (m.platform, "⌘"),
        ] {
            if on {
                out.push_str(sym);
            }
        }
        out.push_str(&key);
    } else {
        for (on, name) in [
            (m.control, "Ctrl+"),
            (m.alt, "Alt+"),
            (m.shift, "Shift+"),
            (m.platform, "Super+"),
        ] {
            if on {
                out.push_str(name);
            }
        }
        out.push_str(&key);
    }
    out
}

impl CommandPalette {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Run a command…"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, _window, cx| match event {
                InputEvent::Change => this.filter(cx),
                InputEvent::PressEnter { .. } => this.confirm(cx),
                _ => {}
            },
        );
        Self {
            input,
            commands: Vec::new(),
            matches: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            matcher: Matcher::new(nucleo::Config::DEFAULT),
            _subscription: subscription,
        }
    }

    /// Called each time the palette opens. Key bindings are looked up in the
    /// context of `target`, the element focused before the palette.
    pub fn reset(
        &mut self,
        target: Option<&FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commands = commands(cx)
            .into_iter()
            .map(|(label, action)| {
                let binding = match target {
                    Some(target) => {
                        window.highest_precedence_binding_for_action_in(action.as_ref(), target)
                    }
                    None => window.highest_precedence_binding_for_action(action.as_ref()),
                };
                let keys = binding.map(|b| {
                    b.keystrokes()
                        .iter()
                        .map(|k| format_keystroke(k.inner()))
                        .collect::<Vec<_>>()
                        .join(" ")
                        .into()
                });
                Command {
                    label: label.into(),
                    action,
                    keys,
                }
            })
            .collect();
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.selected = 0;
        self.filter(cx);
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        let pattern = Pattern::parse(query.trim(), CaseMatching::Ignore, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, Match)> = self
            .commands
            .iter()
            .enumerate()
            .filter_map(|(ix, command)| {
                let mut indices = Vec::new();
                let haystack = Utf32Str::new(&command.label, &mut buf);
                let score = pattern.indices(haystack, &mut self.matcher, &mut indices)?;
                indices.sort_unstable();
                indices.dedup();
                Some((
                    score,
                    Match {
                        command: ix,
                        indices,
                    },
                ))
            })
            .collect();
        // Stable: equal scores keep the list's order.
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        self.matches = scored.into_iter().map(|(_, m)| m).collect();
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn select(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Top);
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(m) = self.matches.get(self.selected) {
            let action = self.commands[m.command].action.boxed_clone();
            cx.emit(PaletteEvent::Run(action));
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
            "escape" => cx.emit(PaletteEvent::Dismiss),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for CommandPalette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let rows: Vec<(SharedString, Option<SharedString>, Vec<u32>)> = self
            .matches
            .iter()
            .map(|m| {
                let c = &self.commands[m.command];
                (c.label.clone(), c.keys.clone(), m.indices.clone())
            })
            .collect();
        let selected = self.selected;
        let list_height = px(ROW_HEIGHT * rows.len().clamp(1, VISIBLE_ROWS) as f32 + 8.);

        div()
            .id("command-palette")
            .key_context("CommandPalette")
            .capture_key_down(cx.listener(Self::on_key))
            .w(px(580.))
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
                            .text_size(px(17.))
                            .prefix(
                                svg()
                                    .path("icons/chevron-right.svg")
                                    .size(px(16.))
                                    .text_color(palette.text_secondary),
                            ),
                    ),
            )
            .child(
                div()
                    .h(list_height)
                    .py(px(4.))
                    .when(rows.is_empty(), |el| {
                        el.child(
                            div()
                                .px(px(16.))
                                .py(px(8.))
                                .text_size(px(13.))
                                .text_color(palette.text_tertiary)
                                .child("No matching commands"),
                        )
                    })
                    .when(!rows.is_empty(), |el| {
                        el.child(
                            uniform_list(
                                "palette-results",
                                rows.len(),
                                cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                                    range
                                        .map(|ix| {
                                            let (label, keys, indices) = &rows[ix];
                                            render_row(
                                                ix,
                                                label,
                                                keys.clone(),
                                                indices,
                                                ix == selected,
                                                &palette,
                                                cx,
                                            )
                                        })
                                        .collect()
                                }),
                            )
                            .track_scroll(&self.scroll)
                            .h(list_height - px(8.)),
                        )
                    }),
            )
    }
}

fn render_row(
    ix: usize,
    label: &SharedString,
    keys: Option<SharedString>,
    indices: &[u32],
    selected: bool,
    palette: &theme::Palette,
    cx: &mut Context<CommandPalette>,
) -> gpui::AnyElement {
    let highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = label
        .char_indices()
        .enumerate()
        .filter(|(ci, _)| indices.binary_search(&(*ci as u32)).is_ok())
        .map(|(_, (bi, ch))| {
            (
                bi..bi + ch.len_utf8(),
                HighlightStyle {
                    color: Some(palette.accent),
                    font_weight: Some(gpui::FontWeight::SEMIBOLD),
                    ..Default::default()
                },
            )
        })
        .collect();
    let row = div()
        .id("row")
        .w_full()
        .h(px(ROW_HEIGHT))
        .px(px(10.))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .rounded(px(7.))
        .text_size(px(14.))
        .text_color(palette.text)
        .when(selected, |el| el.bg(palette.row_selected))
        .when(!selected, |el| el.hover(|s| s.bg(palette.hover)))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .whitespace_nowrap()
                .child(StyledText::new(label.clone()).with_highlights(highlights)),
        )
        .when_some(keys, |el, keys| {
            el.child(
                div()
                    .flex_none()
                    .text_size(px(12.5))
                    .text_color(palette.text_secondary)
                    .child(keys),
            )
        });
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
