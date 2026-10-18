//! The interface fonts, after pdfcraft's `font_definitions_for`: JetBrains
//! Mono (regular, semibold, bold) for chrome and Inter (regular, medium,
//! semibold) for prose, each followed by a CJK face of its weight, then
//! egui's own faces for symbols and emoji. The CJK faces are Noto Sans CJK
//! SC Regular and Bold from the glyphst-fonts checkout, or an installed
//! family found through fontconfig. Settings → Appearance can put an
//! installed family in front of either (see [`FontChoice`]).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use egui::{
    FontData, FontDefinitions, FontFamily, FontTweak,
    epaint::text::{Blob, VariationCoords},
};
use serde::{Deserialize, Serialize};

/// The named family of Inter Medium.
pub const MEDIUM: &str = "medium";
/// The named family of Inter SemiBold.
pub const SEMIBOLD: &str = "semibold";
/// The named family of JetBrains Mono SemiBold.
pub const MONO_SEMIBOLD: &str = "mono-semibold";
/// The named family of JetBrains Mono Bold.
pub const MONO_BOLD: &str = "mono-bold";

/// Inter Regular and SemiBold, also for drawing text outside egui (the
/// assistant's diagrams).
pub const INTER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
pub const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");

/// The bundled chrome family.
pub const BUNDLED_MONO: &str = "JetBrains Mono";
/// The bundled prose family.
pub const BUNDLED_SANS: &str = "Inter";

/// The CJK faces in a glyphst-fonts checkout.
pub const CJK_REGULAR_FILE: &str = "fonts/noto-sans-cjk-sc/NotoSansCJKsc-Regular.otf";
pub const CJK_BOLD_FILE: &str = "fonts/noto-sans-cjk-sc/NotoSansCJKsc-Bold.otf";

/// Installed CJK sans families tried in order without a checkout; past
/// them, any face covering 中 (U+4E2D), Simplified Chinese ones first.
const CJK_FAMILIES: [&str; 5] = [
    "Noto Sans CJK SC",
    "Source Han Sans SC",
    "Source Han Sans CN",
    "Sarasa UI SC",
    "WenQuanYi Micro Hei",
];

/// The families drawn bold, which take the bold CJK face.
const BOLD_FAMILIES: [&str; 3] = [SEMIBOLD, MONO_SEMIBOLD, MONO_BOLD];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Weight {
    Regular,
    Bold,
}

impl Weight {
    fn fontconfig(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Bold => "bold",
        }
    }

    /// The `wght` a variable face is set to: without it a variable CJK
    /// collection draws its default instance, Thin.
    fn wght(self) -> f32 {
        match self {
            Self::Regular => 400.0,
            Self::Bold => 700.0,
        }
    }
}

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
    /// File and face index of the regular and bold CJK faces, when found.
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
    add("Inter", INTER_REGULAR);
    add(
        "Inter-Medium",
        include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    );
    add("Inter-SemiBold", INTER_SEMIBOLD);
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
    let mut found = Vec::new();
    for (key, weight) in [("cjk", Weight::Regular), ("cjk-bold", Weight::Bold)] {
        match cjk_face(weight) {
            Ok((source, data)) => {
                fonts.font_data.insert(key.into(), data.into());
                found.push(source);
            }
            Err(error) => status.error = Some(error),
        }
    }
    let regular = fonts.font_data.contains_key("cjk");
    let bold = fonts.font_data.contains_key("cjk-bold");
    for (family, stack) in &mut fonts.families {
        let wants_bold =
            matches!(family, FontFamily::Name(name) if BOLD_FAMILIES.contains(&name.as_ref()));
        let key = match (wants_bold, regular, bold) {
            (true, _, true) | (false, false, true) => "cjk-bold",
            (_, true, _) => "cjk",
            _ => continue,
        };
        insert_after_text_faces(stack, key);
    }
    if !found.is_empty() {
        status.cjk = Some(found.join("; "));
    }
    ctx.set_fonts(fonts);
    status
}

/// Puts `key` after the stack's chosen and bundled text faces, ahead of
/// egui's symbol faces, whose punctuation would otherwise win.
fn insert_after_text_faces(stack: &mut Vec<String>, key: &str) {
    let text_faces = stack
        .iter()
        .take_while(|name| {
            name.starts_with("system:")
                || name.starts_with("Inter")
                || name.starts_with("JetBrainsMono")
        })
        .count();
    stack.insert(text_faces, key.to_owned());
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
    match_pattern(name, name)
}

/// fc-match's face for `pattern`, when it is of `family`: fc-match always
/// answers with something.
fn match_pattern(pattern: &str, family: &str) -> Option<(String, u32)> {
    let output = Command::new("fc-match")
        .args([pattern, "-f", "%{file}|%{index}|%{family}"])
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&output.stdout);
    let (file, index, families) = parse_face(&line)?;
    families
        .split(',')
        .any(|candidate| candidate.eq_ignore_ascii_case(family))
        .then_some((file, index))
}

/// The CJK face of `weight` and where it came from: the glyphst-fonts
/// checkout, else an installed family.
fn cjk_face(weight: Weight) -> Result<(String, FontData), String> {
    let file = match weight {
        Weight::Regular => CJK_REGULAR_FILE,
        Weight::Bold => CJK_BOLD_FILE,
    };
    let checkout_error = match glyphst_core::paths::fonts_dir() {
        Some(dir) => match shared_bytes(&dir.join(file)) {
            Ok(blob) => {
                return Ok((
                    dir.join(file).display().to_string(),
                    FontData::from_blob(blob, 0),
                ));
            }
            Err(error) => format!("{error}; "),
        },
        None => String::new(),
    };
    let (file, index) = installed_cjk_face(weight)
        .ok_or_else(|| format!("{checkout_error}fontconfig found no face covering 中"))?;
    let blob =
        shared_bytes(Path::new(&file)).map_err(|error| format!("{checkout_error}{error}"))?;
    let mut data = FontData::from_blob(blob, index);
    if data
        .variation_axes()
        .iter()
        .any(|axis| &axis.tag == b"wght")
    {
        data = data.tweak(FontTweak {
            coords: VariationCoords::new([(b"wght", weight.wght())]),
            ..FontTweak::default()
        });
    }
    Ok((format!("{file} (face {index})"), data))
}

/// The bytes of a font file, read once per process: `install` runs again
/// on every font change, and CJK files are tens of megabytes.
fn shared_bytes(path: &Path) -> Result<Blob, String> {
    static FILES: Mutex<Option<HashMap<PathBuf, Blob>>> = Mutex::new(None);
    let mut files = FILES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let files = files.get_or_insert_default();
    if let Some(blob) = files.get(path) {
        return Ok(blob.clone());
    }
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let blob: Blob = Arc::new(bytes);
    files.insert(path.to_owned(), blob.clone());
    Ok(blob)
}

fn installed_cjk_face(weight: Weight) -> Option<(String, u32)> {
    CJK_FAMILIES
        .iter()
        .find_map(|family| {
            match_pattern(&format!("{family}:weight={}", weight.fontconfig()), family)
        })
        .or_else(|| any_cjk_face(weight))
}

/// Any installed face covering 中; fontconfig's listing order is arbitrary,
/// so Simplified Chinese, then sans, faces come first.
fn any_cjk_face(weight: Weight) -> Option<(String, u32)> {
    let pattern = format!(":charset=4e2d:weight={}", weight.fontconfig());
    [pattern.as_str(), ":charset=4e2d"]
        .iter()
        .find_map(|pattern| {
            let output = Command::new("fc-list")
                .args([pattern, "-f", "%{file}|%{index}|%{family}\n"])
                .output()
                .ok()?;
            let listing = String::from_utf8_lossy(&output.stdout);
            let mut faces: Vec<_> = listing.lines().filter_map(parse_face).collect();
            faces.sort_by_key(|(file, _, family)| {
                (
                    ![" SC", " CN", " GB"].iter().any(|tag| family.contains(tag)),
                    !family.contains("Sans"),
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
    fn cjk_faces_follow_the_text_faces() {
        let mut stack: Vec<String> = [
            "system:Maple Mono",
            "Inter",
            "Ubuntu-Light",
            "NotoEmoji-Regular",
        ]
        .map(String::from)
        .into();
        insert_after_text_faces(&mut stack, "cjk");
        assert_eq!(
            stack,
            [
                "system:Maple Mono",
                "Inter",
                "cjk",
                "Ubuntu-Light",
                "NotoEmoji-Regular"
            ]
        );
    }

    #[test]
    fn the_checkout_gives_regular_and_bold_cjk_faces() {
        let Some(dir) = glyphst_core::paths::fonts_dir() else {
            return;
        };
        if !dir.join(CJK_BOLD_FILE).is_file() {
            return;
        }
        let (regular, _) = cjk_face(Weight::Regular).unwrap();
        let (bold, data) = cjk_face(Weight::Bold).unwrap();
        assert!(regular.ends_with("NotoSansCJKsc-Regular.otf"), "{regular}");
        assert!(bold.ends_with("NotoSansCJKsc-Bold.otf"), "{bold}");
        // Read once: a second lookup shares the bytes.
        let (_, again) = cjk_face(Weight::Bold).unwrap();
        assert!(std::ptr::eq(data.bytes(), again.bytes()));
    }

    #[test]
    fn the_bundled_families_are_the_default_choice() {
        let choice = FontChoice::default();
        assert_eq!(choice.chrome, BUNDLED_MONO);
        assert_eq!(choice.prose, BUNDLED_SANS);
    }
}
