//! CJK fallback fonts for egui, found through fontconfig.
//!
//! Like pdfcraft's `installed_font_definitions`, egui's own faces come first
//! and the system CJK face is the last fallback of every family.

use std::process::Command;

use egui::{FontData, FontDefinitions, FontFamily};

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

pub fn install(ctx: &egui::Context) -> FontStatus {
    let mut fonts = FontDefinitions::default();
    let mut status = FontStatus::default();
    match find_cjk_face() {
        Some((file, index)) => match std::fs::read(&file) {
            Ok(bytes) => {
                let mut data = FontData::from_owned(bytes);
                data.index = index;
                fonts.font_data.insert("cjk".into(), data.into());
                for family in [FontFamily::Proportional, FontFamily::Monospace] {
                    fonts.families.entry(family).or_default().push("cjk".into());
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
