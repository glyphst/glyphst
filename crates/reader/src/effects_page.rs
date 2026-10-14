//! The effects manager: galleries of live preview cards for backgrounds,
//! transitions and page tints, with parameter sliders, the wallpaper and
//! the effects folder (Settings › Effects), and the same cards for one
//! book's own choices (the Effects drawer).

use std::{collections::BTreeMap, path::Path};

use eframe::egui_wgpu;
use egui::{Rect, RichText, Sense, Stroke, StrokeKind, Vec2, vec2};
use glyphst_effects::{EffectDef, EffectKind, MAX_PARAMS, ParamKind};

use crate::{
    book_effects::{BookEffects, Choice, effect_use},
    gpu::{EffectUse, PreviewCallback},
    icons,
    prefs::Prefs,
    theme::{self, Tokens},
    widgets,
};

/// What the page asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    ChooseWallpaper,
    OpenFolder,
    NewEffect,
}

/// Card sizes in Settings and in the drawer.
const CARD: Vec2 = vec2(168.0, 104.0);
const SMALL_CARD: Vec2 = vec2(124.0, 78.0);

/// One card: a choice, its label and what its preview draws.
struct Card {
    choice: Choice,
    label: String,
    preview: Option<EffectUse>,
    user: bool,
    /// A wallpaper background with no wallpaper chosen.
    needs_wallpaper: bool,
}

/// The effects of `kind` that can be chosen, in registry order.
fn choosable(effects: &[EffectDef], kind: EffectKind) -> impl Iterator<Item = &EffectDef> {
    effects
        .iter()
        .filter(move |def| def.kind == kind && !def.hidden)
}

/// A wrapping row of cards; returns the card clicked.
fn gallery(
    ui: &mut egui::Ui,
    salt: &str,
    kind: EffectKind,
    cards: &[Card],
    selected: &Choice,
    size: Vec2,
    time: f32,
) -> Option<Choice> {
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
        for card in cards {
            if card_ui(ui, salt, kind, card, *selected == card.choice, size, time) {
                clicked = Some(card.choice.clone());
            }
        }
    });
    clicked
}

fn card_ui(
    ui: &mut egui::Ui,
    salt: &str,
    kind: EffectKind,
    card: &Card,
    selected: bool,
    size: Vec2,
    time: f32,
) -> bool {
    let tokens = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(size + vec2(0.0, 22.0), Sense::click());
    let preview = Rect::from_min_size(rect.min, size);
    let painter = ui.painter();
    painter.rect_filled(preview, 8.0, tokens.pasteboard);
    match &card.preview {
        Some(effect) if ui.is_rect_visible(preview) => {
            painter.add(egui_wgpu::Callback::new_paint_callback(
                preview,
                PreviewCallback {
                    rect: preview,
                    key: format!("{salt}/{:?}", card.choice),
                    effect: effect.clone(),
                    kind,
                    time,
                },
            ));
        }
        Some(_) => {}
        None => icons::paint(ui, preview, "circle-x", 22.0, tokens.text_faint),
    }
    if card.needs_wallpaper {
        painter.text(
            preview.center(),
            egui::Align2::CENTER_CENTER,
            "Choose a wallpaper",
            theme::medium(11.5),
            egui::Color32::from_gray(200),
        );
    }
    let stroke = if selected {
        Stroke::new(2.0, tokens.accent)
    } else if response.hovered() {
        Stroke::new(1.0, tokens.text_faint)
    } else {
        Stroke::new(1.0, tokens.border)
    };
    painter.rect_stroke(preview, 8.0, stroke, StrokeKind::Inside);
    let label_at = egui::pos2(rect.left() + 2.0, preview.bottom() + 11.0);
    let mut width = size.x - 4.0;
    if card.user {
        let badge = Rect::from_min_size(
            egui::pos2(preview.right() - 40.0, preview.bottom() + 3.0),
            vec2(40.0, 16.0),
        );
        painter.rect_filled(badge, 8.0, tokens.accent_soft);
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            "Yours",
            theme::medium(10.5),
            tokens.accent_text,
        );
        width -= 44.0;
    }
    widgets::row_text(
        ui,
        label_at,
        &card.label,
        theme::medium(12.5),
        if selected {
            tokens.text
        } else {
            tokens.text_muted
        },
        width,
    );
    response.on_hover_text(&card.label).clicked()
}

/// Sliders and toggles for `def`'s parameters, with Reset; returns
/// whether a value changed.
fn params_editor(
    ui: &mut egui::Ui,
    def: &EffectDef,
    params: &mut BTreeMap<String, [f32; MAX_PARAMS]>,
) -> bool {
    if def.params.is_empty() {
        return false;
    }
    let mut values = params.get(&def.id).map_or_else(
        || def.default_params(),
        |values| def.sanitize_params(*values),
    );
    let before = values;
    let mut reset = false;
    egui::Grid::new(("effect-params", &def.id))
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            for (index, param) in def.params.iter().enumerate() {
                ui.label(&param.label);
                match param.kind {
                    ParamKind::Float { min, max } => {
                        widgets::slider(ui, egui::Slider::new(&mut values[index], min..=max));
                    }
                    ParamKind::Toggle => {
                        let mut on = values[index] >= 0.5;
                        if ui.checkbox(&mut on, "").changed() {
                            values[index] = if on { 1.0 } else { 0.0 };
                        }
                    }
                }
                ui.end_row();
            }
        });
    if params.contains_key(&def.id) && widgets::ghost_button(ui, "rotate-ccw", "Reset").clicked() {
        reset = true;
    }
    if reset {
        params.remove(&def.id);
        return true;
    }
    if values != before {
        params.insert(def.id.clone(), def.sanitize_params(values));
        return true;
    }
    false
}

fn cards_for(
    effects: &[EffectDef],
    kind: EffectKind,
    params: &BTreeMap<String, [f32; MAX_PARAMS]>,
    has_wallpaper: bool,
    first: Vec<Card>,
) -> Vec<Card> {
    first
        .into_iter()
        .chain(choosable(effects, kind).map(|def| Card {
            choice: Choice::Effect(def.id.clone()),
            label: def.name.clone(),
            preview: Some(effect_use(def, params)),
            user: def.user,
            needs_wallpaper: def.image && !has_wallpaper,
        }))
        .collect()
}

fn as_choice(id: Option<&str>) -> Choice {
    id.map_or(Choice::Off, |id| Choice::Effect(id.to_string()))
}

fn off_card() -> Card {
    Card {
        choice: Choice::Off,
        label: "None".into(),
        preview: None,
        user: false,
        needs_wallpaper: false,
    }
}

/// What Settings › Effects needs.
pub struct SettingsPage<'a> {
    pub prefs: &'a mut Prefs,
    pub effects: &'a [EffectDef],
    pub errors: &'a [String],
    pub folder: Option<&'a Path>,
    pub time: f32,
    pub actions: &'a mut Vec<Action>,
}

/// Settings › Effects: the defaults for every book.
pub fn settings_page(ui: &mut egui::Ui, page: SettingsPage<'_>) {
    let tokens = Tokens::get(ui.ctx());
    let SettingsPage {
        prefs,
        effects,
        errors,
        folder,
        time,
        actions,
    } = page;
    let find = |id: &str| effects.iter().find(|def| def.id == id);
    let has_wallpaper = prefs.wallpaper.is_some();

    widgets::settings_group(ui, "Background", |group| {
        group.full(|ui| {
            let cards = cards_for(
                effects,
                EffectKind::Background,
                &prefs.effect_params,
                has_wallpaper,
                Vec::new(),
            );
            let selected = Choice::Effect(prefs.background.clone());
            if let Some(Choice::Effect(id)) = gallery(
                ui,
                "settings-background",
                EffectKind::Background,
                &cards,
                &selected,
                CARD,
                time,
            ) {
                prefs.background = id;
            }
            if let Some(def) = find(&prefs.background) {
                params_editor(ui, def, &mut prefs.effect_params);
            }
        });
        let name = prefs
            .wallpaper
            .as_deref()
            .and_then(|path| Path::new(path).file_name())
            .map(|name| name.to_string_lossy().into_owned());
        let description = match &name {
            Some(name) => format!("{name}. Wallpaper backgrounds draw it; others ignore it."),
            None => "None. Wallpaper backgrounds draw it; others ignore it.".into(),
        };
        group.row("Wallpaper", Some(&description), |ui| {
            if name.is_some() && widgets::ghost_button(ui, "x", "Clear").clicked() {
                prefs.wallpaper = None;
            }
            if widgets::ghost_button(ui, "folder-open", "Choose…").clicked() {
                actions.push(Action::ChooseWallpaper);
            }
        });
        group.row(
            "Animation rate",
            Some("Frames per second for animated backgrounds."),
            |ui| {
                widgets::slider(
                    ui,
                    egui::Slider::new(&mut prefs.background_fps, 1..=120).suffix(" fps"),
                );
            },
        );
    });

    widgets::settings_group(ui, "Page transition", |group| {
        group.full(|ui| {
            let cards = cards_for(
                effects,
                EffectKind::Transition,
                &prefs.effect_params,
                has_wallpaper,
                vec![off_card()],
            );
            let selected = as_choice(prefs.transition.as_deref());
            match gallery(
                ui,
                "settings-transition",
                EffectKind::Transition,
                &cards,
                &selected,
                CARD,
                time,
            ) {
                Some(Choice::Effect(id)) => prefs.transition = Some(id),
                Some(_) => prefs.transition = None,
                None => {}
            }
            if let Some(def) = prefs.transition.as_deref().and_then(find) {
                params_editor(ui, def, &mut prefs.effect_params);
            }
        });
    });

    widgets::settings_group(ui, "Page tint", |group| {
        group.full(|ui| {
            let cards = cards_for(
                effects,
                EffectKind::Overlay,
                &prefs.effect_params,
                has_wallpaper,
                vec![off_card()],
            );
            let selected = as_choice(prefs.overlay.as_deref());
            match gallery(
                ui,
                "settings-overlay",
                EffectKind::Overlay,
                &cards,
                &selected,
                CARD,
                time,
            ) {
                Some(Choice::Effect(id)) => prefs.overlay = Some(id),
                Some(_) => prefs.overlay = None,
                None => {}
            }
            if let Some(def) = prefs.overlay.as_deref().and_then(find) {
                params_editor(ui, def, &mut prefs.effect_params);
            }
        });
    });

    widgets::settings_group(ui, "Your effects", |group| {
        let folder_text = folder.map(|folder| folder.display().to_string());
        group.row(
            "Effects folder",
            Some(
                "WGSL files here join the galleries and reload when they change. \
                 A book can have its own choices in the Effects drawer.",
            ),
            |ui| {
                if widgets::ghost_button(ui, "file-plus", "New effect").clicked() {
                    actions.push(Action::NewEffect);
                }
                if widgets::ghost_button(ui, "folder-open", "Open folder").clicked() {
                    actions.push(Action::OpenFolder);
                }
            },
        );
        if let Some(folder) = folder_text {
            group.full(|ui| {
                ui.label(
                    RichText::new(folder)
                        .font(theme::mono(11.5))
                        .color(tokens.text_muted),
                );
            });
        }
        if !errors.is_empty() {
            group.full(|ui| {
                for error in errors {
                    ui.horizontal_wrapped(|ui| {
                        ui.add(icons::image("triangle-alert", 14.0, tokens.danger));
                        ui.label(RichText::new(error).small().color(tokens.danger));
                    });
                }
            });
        }
    });
}

/// What the Effects drawer needs: one book's choices and the defaults'
/// names.
pub struct BookPage<'a> {
    pub title: &'a str,
    pub book: &'a mut BookEffects,
    pub default_background: Option<&'a EffectDef>,
    pub default_transition: Option<&'a EffectDef>,
    pub default_overlay: Option<&'a EffectDef>,
    pub effects: &'a [EffectDef],
    pub params: &'a BTreeMap<String, [f32; MAX_PARAMS]>,
    pub has_wallpaper: bool,
    pub time: f32,
}

/// The Effects drawer: this book's background, transition and tint;
/// returns whether a choice changed.
pub fn book_page(ui: &mut egui::Ui, page: BookPage<'_>) -> bool {
    let tokens = Tokens::get(ui.ctx());
    let BookPage {
        title,
        book,
        default_background,
        default_transition,
        default_overlay,
        effects,
        params,
        has_wallpaper,
        time,
    } = page;
    let mut changed = false;
    ui.label(RichText::new(format!("For {title} only")).color(tokens.text_muted));
    let default_card = |default: Option<&EffectDef>| Card {
        choice: Choice::Default,
        label: format!(
            "Default · {}",
            default.map_or("None", |def| def.name.as_str())
        ),
        preview: default.map(|def| effect_use(def, params)),
        user: false,
        needs_wallpaper: default.is_some_and(|def| def.image) && !has_wallpaper,
    };
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (title, kind, choice, default, offer_off) in [
                (
                    "Background",
                    EffectKind::Background,
                    &mut book.background,
                    default_background,
                    false,
                ),
                (
                    "Page transition",
                    EffectKind::Transition,
                    &mut book.transition,
                    default_transition,
                    true,
                ),
                (
                    "Page tint",
                    EffectKind::Overlay,
                    &mut book.overlay,
                    default_overlay,
                    true,
                ),
            ] {
                widgets::section_title(ui, title);
                let mut first = vec![default_card(default)];
                if offer_off {
                    first.push(off_card());
                }
                let cards = cards_for(effects, kind, params, has_wallpaper, first);
                if let Some(picked) = gallery(
                    ui,
                    &format!("book-{title}"),
                    kind,
                    &cards,
                    choice,
                    SMALL_CARD,
                    time,
                ) && picked != *choice
                {
                    *choice = picked;
                    changed = true;
                }
                ui.add_space(6.0);
            }
            if !book.is_default()
                && widgets::ghost_button(ui, "rotate-ccw", "Use the defaults for this book")
                    .clicked()
            {
                let used = book.used;
                *book = BookEffects {
                    used,
                    ..Default::default()
                };
                changed = true;
            }
        });
    changed
}

/// Writes the starter effect into `dir` as a new `my-effect-N.wgsl`, with
/// an id and name of its own; returns its path.
pub fn write_template(dir: &Path) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let number = (1..)
        .find(|number| !dir.join(format!("my-effect-{number}.wgsl")).exists())
        .unwrap_or(1);
    let path = dir.join(format!("my-effect-{number}.wgsl"));
    let text = glyphst_effects::TEMPLATE
        .replace(
            "@effect id: my-effect",
            &format!("@effect id: my-effect-{number}"),
        )
        .replace(
            "@effect name: My effect",
            &format!("@effect name: My effect {number}"),
        );
    std::fs::write(&path, text)?;
    Ok(path)
}

/// Shows `dir` in the system's file manager, creating it first.
pub fn open_folder(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(dir)
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_effects_get_their_own_file_and_id() {
        let dir = std::env::temp_dir().join(format!("glyphst-new-effect-{}", std::process::id()));
        let first = write_template(&dir).unwrap();
        let second = write_template(&dir).unwrap();
        let text = std::fs::read_to_string(&second).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(first.ends_with("my-effect-1.wgsl"));
        assert!(second.ends_with("my-effect-2.wgsl"));
        let def = glyphst_effects::parse_manifest("x", &text).unwrap();
        assert_eq!(
            (def.id.as_str(), def.name.as_str()),
            ("my-effect-2", "My effect 2")
        );
    }
}
