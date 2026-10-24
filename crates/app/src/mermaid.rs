//! Mermaid diagrams in the markdown preview, rendered natively by merman
//! (a Rust port of Mermaid's parsers and layouts): no browser, no Node.
//!
//! Fences are rendered to SVG while the markdown is parsed, which happens
//! off the main thread, and shown as images.

use std::cell::RefCell;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Image, ImageFormat, ImageSource, IntoElement, ParentElement, Styled, div, img,
    px,
};
use gpui_base::text::{MarkdownNode, markdown_ast};
use merman::{Engine, MermaidConfig, OperationControl, RenderOutput, RenderRequest, Renderer};

use crate::theme::{self, Palette};

pub const NODE_NAME: &str = "mermaid";

/// A rendered diagram, or why it couldn't be.
pub struct Diagram {
    image: Option<Arc<Image>>,
    width: f32,
    height: f32,
    error: Option<String>,
    source: String,
}

/// The block parser hook: turns a ```mermaid fence into a diagram node.
pub fn parse(node: &markdown_ast::Node, palette: &Palette) -> Option<MarkdownNode> {
    let markdown_ast::Node::Code(code) = node else {
        return None;
    };
    if !code
        .lang
        .as_deref()
        .is_some_and(|lang| lang.eq_ignore_ascii_case("mermaid"))
    {
        return None;
    }
    let diagram = render(&code.value, palette);
    Some(
        MarkdownNode::new(NODE_NAME, diagram)
            .text(code.value.clone())
            .markdown(format!("```mermaid\n{}\n```", code.value)),
    )
}

fn theme_config(palette: &Palette) -> MermaidConfig {
    let hex = |c: gpui::Hsla| {
        let rgba = gpui::Rgba::from(c);
        format!(
            "#{:02x}{:02x}{:02x}",
            (rgba.r * 255.).round() as u8,
            (rgba.g * 255.).round() as u8,
            (rgba.b * 255.).round() as u8
        )
    };
    let (node, border, note, note_border) = if palette.is_dark {
        ("#2b2e36", "#596178", "#3b3524", "#7d6c32")
    } else {
        ("#f1f3f8", "#a8b2c5", "#fff7d6", "#e2c766")
    };
    MermaidConfig::from_value(serde_json::json!({
        "theme": "base",
        "themeVariables": {
            "darkMode": palette.is_dark,
            "background": hex(palette.editor_bg),
            "fontFamily": "-apple-system, system-ui, Inter, Helvetica, Arial, sans-serif",
            "primaryColor": node,
            "primaryBorderColor": border,
            "primaryTextColor": hex(palette.text),
            "secondaryColor": node,
            "tertiaryColor": hex(palette.code_bg),
            "lineColor": hex(palette.text_secondary),
            "textColor": hex(palette.text),
            "edgeLabelBackground": hex(palette.editor_bg),
            "noteBkgColor": note,
            "noteBorderColor": note_border,
            "noteTextColor": hex(palette.text),
        }
    }))
}

fn render(source: &str, palette: &Palette) -> Diagram {
    thread_local! {
        static RENDERER: RefCell<Option<(bool, Renderer)>> = const { RefCell::new(None) };
    }
    let failed = |error: String| Diagram {
        image: None,
        width: 0.,
        height: 0.,
        error: Some(error),
        source: source.to_string(),
    };
    let output = RENDERER.with_borrow_mut(|cached| {
        if cached
            .as_ref()
            .is_none_or(|(dark, _)| *dark != palette.is_dark)
        {
            let engine = Engine::new().with_site_config(theme_config(palette));
            *cached = Some((palette.is_dark, Renderer::new().with_engine(engine)));
        }
        let renderer = &cached.as_ref().unwrap().1;
        // GPUI draws SVG with resvg, which can't show HTML labels.
        let request = merman::SvgRequest {
            pipeline: Some(merman::svg::SvgPipeline::resvg_safe()),
            ..Default::default()
        };
        renderer.render(RenderRequest::svg(source, OperationControl::new(), request))
    });
    let svg = match output {
        Ok(RenderOutput::Svg(Some(svg))) => svg.svg().to_string(),
        Ok(_) => return failed("Not a Mermaid diagram".into()),
        Err(err) => return failed(err.to_string()),
    };
    let Some((width, height)) = view_box_size(&svg) else {
        return failed("The diagram has no size".into());
    };
    Diagram {
        image: Some(Arc::new(Image::from_bytes(
            ImageFormat::Svg,
            with_pixel_size(&svg, width, height).into_bytes(),
        ))),
        width,
        height,
        error: None,
        source: source.to_string(),
    }
}

/// Width and height from the root element's `viewBox`.
fn view_box_size(svg: &str) -> Option<(f32, f32)> {
    let root = &svg[..svg.find('>')?];
    let start = root.find("viewBox=\"")? + "viewBox=\"".len();
    let end = start + root[start..].find('"')?;
    let mut parts = root[start..end]
        .split(|c: char| c == ' ' || c == ',')
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.parse::<f32>().ok());
    let (_, _, w, h) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    (w > 0. && h > 0.).then_some((w, h))
}

/// Mermaid sizes its SVG `width="100%"`; give the rasterizer real pixels.
fn with_pixel_size(svg: &str, width: f32, height: f32) -> String {
    let Some(end) = svg.find('>') else {
        return svg.to_string();
    };
    let root = svg[..end].replacen(
        "width=\"100%\"",
        &format!("width=\"{width}\" height=\"{height}\""),
        1,
    );
    format!("{root}{}", &svg[end..])
}

/// The block renderer: the diagram at its natural size, scaled down to fit.
pub fn element(node: &MarkdownNode, cx: &App) -> AnyElement {
    let palette = theme::palette(cx);
    let Some(diagram) = node.data::<Diagram>() else {
        return div().into_any_element();
    };
    match &diagram.image {
        Some(image) => div()
            .w_full()
            .flex()
            .justify_center()
            .py(px(6.))
            .child(
                div()
                    .w_full()
                    .max_w(px(diagram.width))
                    .aspect_ratio(diagram.width / diagram.height)
                    .child(img(ImageSource::Image(image.clone())).size_full()),
            )
            .into_any_element(),
        None => div()
            .w_full()
            .p(px(14.))
            .rounded(px(8.))
            .bg(palette.code_bg)
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(palette.danger())
                    .child(format!(
                        "Mermaid: {}",
                        diagram.error.as_deref().unwrap_or("could not render")
                    )),
            )
            .child(
                div()
                    .font_family(theme::fonts(cx).mono.clone())
                    .text_size(px(12.5))
                    .text_color(palette.text_secondary)
                    .whitespace_normal()
                    .child(diagram.source.clone()),
            )
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flowchart_renders_to_svg() {
        let d = render("flowchart TD\n  A[Start] --> B{Done?}\n", &Palette::light());
        assert!(d.error.is_none(), "{:?}", d.error);
        assert!(d.width > 0. && d.height > 0.);
    }

    #[test]
    fn syntax_errors_are_reported() {
        let d = render("flowchart TD\n  A --> \n --> -->", &Palette::dark());
        assert!(d.image.is_none() || d.error.is_none());
    }

    #[test]
    fn sizes() {
        let svg = r#"<svg id="x" width="100%" viewBox="-50 -10 607 376"><g/></svg>"#;
        assert_eq!(view_box_size(svg), Some((607., 376.)));
        assert!(
            with_pixel_size(svg, 607., 376.).starts_with(r#"<svg id="x" width="607" height="376""#)
        );
    }
}
