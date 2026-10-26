//! Embedded assets: our brand SVGs, file-type icons and painting icons, then
//! gpui-component's icon set.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub struct Assets;

// The image painting toolbar's icons and the table toggle's, from the full
// Lucide set.
gpui_kit_assets::icon_assets!(
    PaintIcons,
    [
        Hand,
        Pencil,
        Eraser,
        PaintBucket,
        Pipette,
        Slash,
        Square,
        Circle,
        Undo2,
        Redo2,
        ZoomIn,
        ZoomOut,
        Table,
    ]
);

const LOGO_MARK: &[u8] = include_bytes!("../../../assets/logo/logo-mark.svg");

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(svg) = crate::file_icons::asset(path) {
            return Ok(Some(Cow::Borrowed(svg)));
        }
        if let Some(svg) = PaintIcons.load(path)? {
            return Ok(Some(svg));
        }
        match path {
            "brand/logo-mark.svg" => Ok(Some(Cow::Borrowed(LOGO_MARK))),
            _ => gpui_kit_assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_kit_assets::Assets.list(path)
    }
}
