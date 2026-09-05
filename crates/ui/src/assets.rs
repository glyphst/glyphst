use gpui::{AssetSource, Result, SharedString};
use gpui_component_assets::Assets as ComponentAssets;
use rust_embed::RustEmbed;
use std::{borrow::Cow, collections::BTreeSet};

#[derive(RustEmbed)]
#[folder = "assets"]
#[include = "icons/**/*"]
struct GlyphstAssets;

/// The application asset source.
///
/// Glyphst-specific assets take precedence, while the curated
/// `gpui-kit-assets` bundle supplies the component icon set.
pub struct Assets {
    component: ComponentAssets,
}

impl Default for Assets {
    fn default() -> Self {
        Self {
            component: ComponentAssets::new(""),
        }
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(file) = GlyphstAssets::get(path) {
            return Ok(Some(file.data));
        }

        self.component.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut assets = BTreeSet::new();
        assets.extend(
            GlyphstAssets::iter()
                .filter(|asset| asset.starts_with(path))
                .map(|asset| asset.to_string()),
        );
        assets.extend(
            self.component
                .list(path)?
                .into_iter()
                .map(|asset| asset.to_string()),
        );

        Ok(assets.into_iter().map(Into::into).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn glyphst_and_component_asset_sets_are_disjoint() {
        let component = ComponentAssets::new("");
        let component_assets = component
            .list("icons/")
            .expect("component icon inventory")
            .into_iter()
            .map(|path| path.to_string())
            .collect::<BTreeSet<_>>();

        let duplicates = GlyphstAssets::iter()
            .map(|path| path.to_string())
            .filter(|path| component_assets.contains(path))
            .collect::<Vec<_>>();

        assert!(
            duplicates.is_empty(),
            "Glyphst must not embed icons already supplied by gpui-kit-assets: {duplicates:?}"
        );
    }

    #[test]
    fn component_icon_bundle_stays_curated() {
        let component_icons = ComponentAssets::new("")
            .list("icons/")
            .expect("component icon inventory");

        assert!(
            component_icons.len() <= 128,
            "gpui-kit-assets grew to {} icons; review the binary-size impact before updating",
            component_icons.len()
        );
    }

    #[test]
    fn composite_assets_load_both_sources() {
        let assets = Assets::default();

        assert!(
            assets
                .load("icons/app-window.svg")
                .expect("Glyphst icon lookup")
                .is_some()
        );
        assert!(
            assets
                .load("icons/regex.svg")
                .expect("Glyphst regexp icon lookup")
                .is_some()
        );
        assert!(
            assets
                .load("icons/bookmark-fill.svg")
                .expect("Glyphst filled bookmark icon lookup")
                .is_some()
        );
        assert!(
            assets
                .load("icons/search.svg")
                .expect("component icon lookup")
                .is_some()
        );
        assert!(
            assets
                .load("icons/file-text.svg")
                .expect("shared document icon lookup")
                .is_some()
        );
    }
}
