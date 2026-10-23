//! Embedded assets: our brand SVGs, then gpui-component's icon set.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub struct Assets;

const LOGO_MARK: &[u8] = include_bytes!("../../../assets/logo/logo-mark.svg");

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match path {
            "brand/logo-mark.svg" => Ok(Some(Cow::Borrowed(LOGO_MARK))),
            _ => gpui_kit_assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_kit_assets::Assets.list(path)
    }
}
