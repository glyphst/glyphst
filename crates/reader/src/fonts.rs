//! The interface fonts, after pdfcraft's `font_definitions_for`: Inter
//! (regular, medium, semibold) and JetBrains Mono first, then egui's own
//! faces for symbols and emoji, then a system CJK face found through
//! fontconfig as the last fallback of every family.

use std::{process::Command, sync::Arc};

use egui::{FontData, FontDefinitions, FontFamily};

/// The named family of Inter Medium.
pub const MEDIUM: &str = "medium";
/// The named family of Inter SemiBold.
pub const SEMIBOLD: &str = "semibold";

/// fontconfig patterns tried in order; the first installed face covering
/// 中 (U+4E2D) wins.
const CJK_PATTERNS: [&str; 4] = [
    ":lang=zh-cn:style=Regular:charset=4e2d",
    ":lang=zh-cn:charset=4e2d",
    ":lang=ja:charset=4e2d",
    ":charset=4e2d",
];

#[derive(Clone, Debug, Default)]
pub struct FontStatus {
    /// File and face index of the CJK fallback, when one was found.
    pub cjk: Option<String>,
    pub error: Option<String>,
}

/// Inter and JetBrains Mono ahead of egui's own faces, and the
/// [`MEDIUM`] and [`SEMIBOLD`] families.
pub fn embedded() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    add("Inter", include_bytes!("../assets/fonts/Inter-Regular.ttf"));
    add(
        "Inter-Medium",
        include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    );
    add(
        "Inter-SemiBold",
        include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    );
    add(
        "JetBrainsMono",
        include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "Inter".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "JetBrainsMono".to_owned());
    let fallback = fonts.families[&FontFamily::Proportional][1..].to_vec();
    for (family, primary) in [(MEDIUM, "Inter-Medium"), (SEMIBOLD, "Inter-SemiBold")] {
        let mut stack = vec![primary.to_owned()];
        stack.extend(fallback.iter().cloned());
        fonts
            .families
            .insert(FontFamily::Name(family.into()), stack);
    }
    fonts
}

pub fn install(ctx: &egui::Context) -> FontStatus {
    let mut fonts = embedded();
    let mut status = FontStatus::default();
    match find_cjk_face() {
        Some((file, index)) => match std::fs::read(&file) {
            Ok(bytes) => {
                let mut data = FontData::from_owned(bytes);
                data.index = index;
                fonts.font_data.insert("cjk".into(), data.into());
                for stack in fonts.families.values_mut() {
                    stack.push("cjk".into());
                }
                status.cjk = Some(format!("{file} (face {index})"));
            }
            Err(error) => status.error = Some(format!("{file}: {error}")),
        },
        None => status.error = Some("fontconfig found no face covering 中".into()),
    }
    ctx.set_fonts(fonts);
    status
}

fn find_cjk_face() -> Option<(String, u32)> {
    CJK_PATTERNS.iter().find_map(|pattern| {
        let output = Command::new("fc-list")
            .args([pattern, "-f", "%{file}|%{index}|%{family}\n"])
            .output()
            .ok()?;
        let listing = String::from_utf8_lossy(&output.stdout);
        let mut faces: Vec<_> = listing.lines().filter_map(parse_face).collect();
        // Prefer sans faces; fontconfig's listing order is arbitrary.
        faces.sort_by_key(|(file, _, family)| {
            (
                !family.contains("Sans"),
                !file.ends_with(".ttc"),
                file.clone(),
            )
        });
        faces
            .into_iter()
            .next()
            .map(|(file, index, _)| (file, index))
    })
}

/// Parses `file|index|family`. fontconfig puts the named instance of a
/// variable font in the high 16 bits of the index; egui wants the face.
fn parse_face(line: &str) -> Option<(String, u32, String)> {
    let mut parts = line.splitn(3, '|');
    let file = parts.next()?.to_string();
    let index = parts.next()?.parse::<u32>().ok()? & 0xffff;
    let family = parts.next().unwrap_or_default().to_string();
    (!file.is_empty()).then_some((file, index, family))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_family_starts_with_an_embedded_face() {
        let fonts = embedded();
        for (family, first) in [
            (FontFamily::Proportional, "Inter"),
            (FontFamily::Monospace, "JetBrainsMono"),
            (FontFamily::Name(MEDIUM.into()), "Inter-Medium"),
            (FontFamily::Name(SEMIBOLD.into()), "Inter-SemiBold"),
        ] {
            let stack = &fonts.families[&family];
            assert_eq!(stack[0], first);
            assert!(stack.len() > 1, "egui's faces follow for symbols and emoji");
        }
    }

    #[test]
    fn fontconfig_lines_give_the_collection_face() {
        assert_eq!(
            parse_face(
                "/f/NotoSansCJK-VF.otf.ttc|262146|Noto Sans CJK SC,Noto Sans CJK SC Regular"
            ),
            Some((
                "/f/NotoSansCJK-VF.otf.ttc".into(),
                2,
                "Noto Sans CJK SC,Noto Sans CJK SC Regular".into()
            ))
        );
        assert_eq!(parse_face("|0|x"), None);
        assert_eq!(parse_face("/f/a.ttf|x|y"), None);
    }
}
