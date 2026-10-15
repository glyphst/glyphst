//! The interface fonts, after pdfcraft's `font_definitions_for`: JetBrains
//! Mono (regular, semibold, bold) for chrome and Inter (regular, medium,
//! semibold) for prose, then egui's own faces for symbols and emoji, then a
//! system CJK face found through fontconfig as the last fallback of every
//! family. Settings → Appearance can put an installed family in front of
//! either (see [`FontChoice`]).

use std::{process::Command, sync::Arc};

use egui::{FontData, FontDefinitions, FontFamily};
use serde::{Deserialize, Serialize};

/// The named family of Inter Medium.
pub const MEDIUM: &str = "medium";
/// The named family of Inter SemiBold.
pub const SEMIBOLD: &str = "semibold";
/// The named family of JetBrains Mono SemiBold.
pub const MONO_SEMIBOLD: &str = "mono-semibold";
/// The named family of JetBrains Mono Bold.
pub const MONO_BOLD: &str = "mono-bold";

/// The bundled chrome family.
pub const BUNDLED_MONO: &str = "JetBrains Mono";
/// The bundled prose family.
pub const BUNDLED_SANS: &str = "Inter";

/// fontconfig patterns tried in order; the first installed face covering
/// 中 (U+4E2D) wins.
const CJK_PATTERNS: [&str; 4] = [
    ":lang=zh-cn:style=Regular:charset=4e2d",
    ":lang=zh-cn:charset=4e2d",
    ":lang=ja:charset=4e2d",
    ":charset=4e2d",
];

/// The chrome and prose families, by fontconfig family name; the bundled
/// ones by default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FontChoice {
    pub chrome: String,
    pub prose: String,
}

impl Default for FontChoice {
    fn default() -> Self {
        Self {
            chrome: BUNDLED_MONO.into(),
            prose: BUNDLED_SANS.into(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct FontStatus {
    /// File and face index of the CJK fallback, when one was found.
    pub cjk: Option<String>,
    pub error: Option<String>,
}

/// JetBrains Mono and Inter ahead of egui's own faces, and the
/// [`MEDIUM`], [`SEMIBOLD`], [`MONO_SEMIBOLD`] and [`MONO_BOLD`] families.
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
    add(
        "JetBrainsMono-SemiBold",
        include_bytes!("../assets/fonts/JetBrainsMono-SemiBold.ttf"),
    );
    add(
        "JetBrainsMono-Bold",
        include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
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
    for (family, primary) in [
        (MEDIUM, "Inter-Medium"),
        (SEMIBOLD, "Inter-SemiBold"),
        (MONO_SEMIBOLD, "JetBrainsMono-SemiBold"),
        (MONO_BOLD, "JetBrainsMono-Bold"),
    ] {
        let mut stack = vec![primary.to_owned()];
        stack.extend(fallback.iter().cloned());
        fonts
            .families
            .insert(FontFamily::Name(family.into()), stack);
    }
    fonts
}

/// The embedded fonts with `choice`'s installed families in front, and
/// the CJK fallback.
pub fn install(ctx: &egui::Context, choice: &FontChoice) -> FontStatus {
    let mut fonts = embedded();
    let mut status = FontStatus::default();
    for (name, families) in [
        (
            choice.chrome.as_str(),
            [
                FontFamily::Monospace,
                FontFamily::Name(MONO_SEMIBOLD.into()),
            ],
        ),
        (
            choice.prose.as_str(),
            [FontFamily::Proportional, FontFamily::Name(SEMIBOLD.into())],
        ),
    ] {
        if name == BUNDLED_MONO || name == BUNDLED_SANS || name.is_empty() {
            continue;
        }
        let Some((file, index)) = match_family(name) else {
            status.error = Some(format!("fontconfig has no family {name}"));
            continue;
        };
        match std::fs::read(&file) {
            Ok(bytes) => {
                let key = format!("system:{name}");
                let mut data = FontData::from_owned(bytes);
                data.index = index;
                fonts.font_data.insert(key.clone(), data.into());
                for family in families {
                    fonts
                        .families
                        .entry(family)
                        .or_default()
                        .insert(0, key.clone());
                }
            }
            Err(error) => status.error = Some(format!("{file}: {error}")),
        }
    }
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

/// Installed families for the font pickers: monospaced ones when `mono`,
/// sorted, with the bundled family first.
pub fn installed_families(mono: bool) -> Vec<String> {
    let bundled = if mono { BUNDLED_MONO } else { BUNDLED_SANS };
    let pattern = if mono { ":spacing=mono" } else { ":" };
    let mut families: Vec<String> = Command::new("fc-list")
        .args([pattern, "-f", "%{family[0]}\n"])
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|family| !family.is_empty() && *family != bundled)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    families.sort();
    families.dedup();
    families.insert(0, bundled.to_owned());
    families
}

fn match_family(name: &str) -> Option<(String, u32)> {
    let output = Command::new("fc-match")
        .args([name, "-f", "%{file}|%{index}|%{family}"])
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&output.stdout);
    let (file, index, family) = parse_face(&line)?;
    // fc-match always answers; only take a face of the family asked for.
    family
        .split(',')
        .any(|candidate| candidate.eq_ignore_ascii_case(name))
        .then_some((file, index))
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
            (
                FontFamily::Name(MONO_SEMIBOLD.into()),
                "JetBrainsMono-SemiBold",
            ),
            (FontFamily::Name(MONO_BOLD.into()), "JetBrainsMono-Bold"),
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

    #[test]
    fn the_bundled_families_are_the_default_choice() {
        let choice = FontChoice::default();
        assert_eq!(choice.chrome, BUNDLED_MONO);
        assert_eq!(choice.prose, BUNDLED_SANS);
    }
}
