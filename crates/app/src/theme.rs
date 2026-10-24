//! Colors and fonts. Two built-in palettes modelled on macOS and Xcode, picked
//! from the system appearance and swapped live when it changes.

use gpui::{App, Global, Hsla, SharedString, WindowAppearance, font, rgb, rgba};
use gpui_component::{Theme, ThemeMode};
use me_syntax::Highlight;

#[derive(Clone, Debug)]
pub struct Palette {
    pub is_dark: bool,
    pub editor_bg: Hsla,
    pub sidebar_bg: Hsla,
    pub tab_bar_bg: Hsla,
    pub tab_active_bg: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub accent: Hsla,
    pub selection: Hsla,
    pub selection_inactive: Hsla,
    pub current_line: Hsla,
    pub gutter_text: Hsla,
    pub gutter_active_text: Hsla,
    pub cursor: Hsla,
    pub find_match: Hsla,
    pub find_match_active: Hsla,
    pub hover: Hsla,
    pub row_selected: Hsla,
    pub row_selected_inactive: Hsla,
    pub ignored_text: Hsla,
    pub modified_dot: Hsla,
    pub overlay_bg: Hsla,
    /// Background of code in the markdown preview.
    pub code_bg: Hsla,
    syntax: SyntaxColors,
}

#[derive(Clone, Debug)]
struct SyntaxColors {
    comment: Hsla,
    keyword: Hsla,
    string: Hsla,
    escape: Hsla,
    number: Hsla,
    constant: Hsla,
    function: Hsla,
    ty: Hsla,
    property: Hsla,
    attribute: Hsla,
    tag: Hsla,
    punctuation: Hsla,
    title: Hsla,
    link: Hsla,
    literal: Hsla,
    added: Hsla,
    removed: Hsla,
}

impl Global for Palette {}

impl Palette {
    pub fn light() -> Self {
        Self {
            is_dark: false,
            editor_bg: rgb(0xffffff).into(),
            sidebar_bg: if cfg!(target_os = "macos") {
                rgba(0xf3f3f5d9).into()
            } else {
                rgb(0xf3f3f5).into()
            },
            tab_bar_bg: rgb(0xebebed).into(),
            tab_active_bg: rgb(0xffffff).into(),
            border: rgb(0xd8d8dc).into(),
            text: rgb(0x1d1d1f).into(),
            text_secondary: rgb(0x6e6e73).into(),
            text_tertiary: rgb(0xa1a1a6).into(),
            accent: rgb(0x007aff).into(),
            selection: rgba(0x007aff33).into(),
            selection_inactive: rgba(0x00000017).into(),
            current_line: rgba(0x0000000a).into(),
            gutter_text: rgb(0xb0b0b5).into(),
            gutter_active_text: rgb(0x3a3a3c).into(),
            cursor: rgb(0x1d1d1f).into(),
            find_match: rgba(0xffcc0059).into(),
            find_match_active: rgba(0xff9f0a99).into(),
            hover: rgba(0x0000000d).into(),
            row_selected: rgba(0x007aff2e).into(),
            row_selected_inactive: rgba(0x00000014).into(),
            ignored_text: rgb(0xa8a8ad).into(),
            modified_dot: rgb(0x6e6e73).into(),
            overlay_bg: rgb(0xfbfbfd).into(),
            code_bg: rgb(0xf4f4f6).into(),
            syntax: SyntaxColors {
                comment: rgb(0x707f8c).into(),
                keyword: rgb(0x9b2393).into(),
                string: rgb(0xc41a16).into(),
                escape: rgb(0x9b5c1a).into(),
                number: rgb(0x1c00cf).into(),
                constant: rgb(0x6c36a9).into(),
                function: rgb(0x326d74).into(),
                ty: rgb(0x0b4f79).into(),
                property: rgb(0x326d74).into(),
                attribute: rgb(0x815f03).into(),
                tag: rgb(0x9b2393).into(),
                punctuation: rgb(0x505055).into(),
                title: rgb(0x0b4f79).into(),
                link: rgb(0x0e0eff).into(),
                literal: rgb(0x643820).into(),
                added: rgb(0x1f8a31).into(),
                removed: rgb(0xc41a16).into(),
            },
        }
    }

    pub fn dark() -> Self {
        Self {
            is_dark: true,
            editor_bg: rgb(0x1f1f24).into(),
            sidebar_bg: if cfg!(target_os = "macos") {
                rgba(0x26262bcc).into()
            } else {
                rgb(0x26262b).into()
            },
            tab_bar_bg: rgb(0x29292e).into(),
            tab_active_bg: rgb(0x1f1f24).into(),
            border: rgb(0x3a3a3f).into(),
            text: rgb(0xe6e6e8).into(),
            text_secondary: rgb(0x98989d).into(),
            text_tertiary: rgb(0x68686d).into(),
            accent: rgb(0x0a84ff).into(),
            selection: rgba(0x0a84ff4d).into(),
            selection_inactive: rgba(0xffffff1f).into(),
            current_line: rgba(0xffffff0a).into(),
            gutter_text: rgb(0x5c5c61).into(),
            gutter_active_text: rgb(0xd0d0d3).into(),
            cursor: rgb(0xe6e6e8).into(),
            find_match: rgba(0xffd60a40).into(),
            find_match_active: rgba(0xff9f0a8c).into(),
            hover: rgba(0xffffff0f).into(),
            row_selected: rgba(0x0a84ff59).into(),
            row_selected_inactive: rgba(0xffffff1a).into(),
            ignored_text: rgb(0x6e6e73).into(),
            modified_dot: rgb(0xa1a1a6).into(),
            overlay_bg: rgb(0x2c2c31).into(),
            code_bg: rgb(0x2a2a30).into(),
            syntax: SyntaxColors {
                comment: rgb(0x7f8c98).into(),
                keyword: rgb(0xfc5fa3).into(),
                string: rgb(0xfc6a5d).into(),
                escape: rgb(0xfd8f3f).into(),
                number: rgb(0xd0bf69).into(),
                constant: rgb(0xa167e6).into(),
                function: rgb(0x67b7a4).into(),
                ty: rgb(0x5dd8ff).into(),
                property: rgb(0x67b7a4).into(),
                attribute: rgb(0xbf8555).into(),
                tag: rgb(0xfc5fa3).into(),
                punctuation: rgb(0xb4b4b8).into(),
                title: rgb(0x5dd8ff).into(),
                link: rgb(0x6699ff).into(),
                literal: rgb(0xfd8f3f).into(),
                added: rgb(0x7bd88f).into(),
                removed: rgb(0xfc6a5d).into(),
            },
        }
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
        }
    }

    /// Error text.
    pub fn danger(&self) -> Hsla {
        self.syntax.removed
    }

    pub fn syntax(&self, highlight: Highlight) -> Option<Hsla> {
        let s = &self.syntax;
        Some(match highlight {
            Highlight::Comment => s.comment,
            Highlight::Keyword => s.keyword,
            Highlight::String => s.string,
            Highlight::StringEscape => s.escape,
            Highlight::Number => s.number,
            Highlight::Constant => s.constant,
            Highlight::Function => s.function,
            Highlight::Type => s.ty,
            Highlight::Property => s.property,
            Highlight::Attribute => s.attribute,
            Highlight::Tag => s.tag,
            Highlight::Operator => return None,
            Highlight::Punctuation => s.punctuation,
            Highlight::Label => s.attribute,
            Highlight::Title => s.title,
            Highlight::Emphasis => return None,
            Highlight::Strong => return None,
            Highlight::Link => s.link,
            Highlight::Literal => s.literal,
            Highlight::DiffPlus => s.added,
            Highlight::DiffMinus => s.removed,
        })
    }
}

pub fn palette(cx: &App) -> &Palette {
    cx.global::<Palette>()
}

/// Font families resolved once at startup.
#[derive(Clone, Debug)]
pub struct Fonts {
    pub ui: SharedString,
    pub mono: SharedString,
}

impl Global for Fonts {}

pub fn fonts(cx: &App) -> &Fonts {
    cx.global::<Fonts>()
}

const MONO_CANDIDATES: &[&str] = if cfg!(target_os = "macos") {
    &["SF Mono", "Menlo", "Monaco"]
} else if cfg!(target_os = "windows") {
    &["Cascadia Mono", "Consolas", "Courier New"]
} else {
    &[
        "DejaVu Sans Mono",
        "Noto Sans Mono",
        "Liberation Mono",
        "Ubuntu Mono",
    ]
};

/// UI fonts for platforms without a system UI font alias that has weights
/// (GPUI maps `.SystemUIFont` to San Francisco on macOS).
const UI_CANDIDATES: &[&str] = &[
    "Inter",
    "Cantarell",
    "Ubuntu",
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
    "Segoe UI",
];

/// Picks the first family that actually resolves. Probing a few names is
/// much cheaper than enumerating every installed font.
fn resolve_family(candidates: &[&'static str], cx: &App) -> SharedString {
    let text_system = cx.text_system();
    for family in candidates {
        let id = text_system.resolve_font(&font(*family));
        if text_system
            .get_font_for_id(id)
            .is_some_and(|f| f.family.as_ref() == *family)
        {
            return SharedString::from(*family);
        }
    }
    SharedString::from(candidates[0])
}

pub fn init(appearance: WindowAppearance, cx: &mut App) {
    let ui = if cfg!(target_os = "macos") {
        ".SystemUIFont".into()
    } else {
        resolve_family(UI_CANDIDATES, cx)
    };
    let fonts = Fonts {
        ui,
        mono: resolve_family(MONO_CANDIDATES, cx),
    };
    cx.set_global(fonts);
    apply(appearance, cx);
}

/// Switches to the palette for `appearance` and restyles gpui-component's
/// widgets (inputs, markdown) to match.
pub fn apply(appearance: WindowAppearance, cx: &mut App) {
    // ME_APPEARANCE=dark|light overrides the system (handy for testing).
    let appearance = match std::env::var("ME_APPEARANCE").as_deref() {
        Ok("dark") => WindowAppearance::Dark,
        Ok("light") => WindowAppearance::Light,
        _ => appearance,
    };
    let palette = Palette::for_appearance(appearance);
    if cx.has_global::<Palette>() && cx.global::<Palette>().is_dark == palette.is_dark {
        return;
    }
    let mode = if palette.is_dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    Theme::change(mode, None, cx);
    let mono = fonts(cx).mono.clone();
    let ui = fonts(cx).ui.clone();
    let p = palette.clone();
    Theme::update(cx, move |theme| {
        theme.mono_font_family = mono;
        theme.font_family = ui;
        theme.font_size = gpui::px(14.);
        theme.mono_font_size = gpui::px(13.);
        theme.background = p.editor_bg;
        theme.foreground = p.text;
        theme.border = p.border;
        theme.input = p.border;
        theme.ring = p.accent;
        theme.caret = p.cursor;
        theme.selection = p.selection;
        theme.link = p.accent;
        theme.link_hover = p.accent;
        theme.muted_foreground = p.text_secondary;
        theme.popover = p.overlay_bg;
        theme.popover_foreground = p.text;
        theme.list_active = p.row_selected;
        theme.list_hover = p.hover;
        theme.muted = p.code_bg;
        theme.table_head = p.code_bg;
    });
    cx.set_global(palette);
    cx.refresh_windows();
}
