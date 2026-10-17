//! The effects manager: galleries of live preview cards for transitions,
//! with parameter sliders and the effects folder (Settings › Effects); the
//! background behind the pages and each theme's page tint (Settings ›
//! Appearance); and the same cards for one book's own choices (the
//! Effects drawer).

use std::{collections::BTreeMap, path::Path};

use eframe::egui_wgpu;
use egui::{Rect, RichText, Sense, Stroke, StrokeKind, Vec2, vec2};
use glyphst_effects::{EffectDef, EffectKind, MAX_PARAMS, ParamKind};

use glyphst_ui::{Tip, button::Button, motif, slider::Slider, toggle::Switch};

use crate::{
    book_effects::{BookEffects, Choice, ThemeColours, theme_colours, themed_use},
    gpu::{EffectUse, PreviewCallback},
    icons,
    prefs::Prefs,
    theme::{self, ThemeKind, Tokens},
};

/// What the page asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    ChooseWallpaper,
    OpenFolder,
    NewEffect,
}

/// The narrowest card in Settings; cards stretch to fill each row.
const MIN_CARD_WIDTH: f32 = 150.0;
/// A card's height for its width.
const CARD_ASPECT: f32 = 100.0 / 168.0;
/// The room between cards in a row (frame 07).
const GAP: f32 = 12.0;

/// The card size that fills a row `width` wide with as many cards as fit
/// at `MIN_CARD_WIDTH` or wider.
fn fill_card(width: f32) -> Vec2 {
    let columns = ((width + GAP) / (MIN_CARD_WIDTH + GAP)).floor().max(1.0);
    // A point less, so rounding never wraps the last card.
    let card = ((width - (columns - 1.0) * GAP) / columns).floor() - 1.0;
    let card = card.max(48.0);
    vec2(card, (card * CARD_ASPECT).round())
}

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
        ui.spacing_mut().item_spacing = vec2(GAP, 8.0);
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
    let (rect, response) = ui.allocate_exact_size(size + vec2(0.0, 20.0), Sense::click());
    let preview = Rect::from_min_size(rect.min, size);
    let painter = ui.painter();
    painter.rect_filled(preview, 0, tokens.bg);
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
        None => {
            painter.text(
                preview.center(),
                egui::Align2::CENTER_CENTER,
                "—",
                theme::mono(12.0),
                tokens.faint,
            );
        }
    }
    if card.needs_wallpaper {
        painter.text(
            preview.center(),
            egui::Align2::CENTER_CENTER,
            "no wallpaper",
            theme::mono(10.0),
            egui::Color32::from_gray(200),
        );
    }
    let stroke = if selected {
        Stroke::new(1.5, tokens.accent)
    } else if response.hovered() {
        Stroke::new(1.0, tokens.faint)
    } else {
        tokens.edge()
    };
    painter.rect_stroke(preview, 0, stroke, StrokeKind::Inside);
    let label_at = egui::pos2(rect.left(), preview.bottom() + 10.0);
    let mut width = size.x;
    if card.user {
        painter.text(
            egui::pos2(preview.right(), preview.bottom() + 10.0),
            egui::Align2::RIGHT_CENTER,
            "yours",
            theme::mono(9.0),
            tokens.accent_text,
        );
        width -= 34.0;
    }
    let label = if selected {
        format!("● {}", card.label.to_lowercase())
    } else {
        card.label.to_lowercase()
    };
    glyphst_ui::text::row(
        ui,
        label_at,
        label,
        theme::mono(10.0),
        if selected {
            tokens.accent_text
        } else {
            tokens.muted
        },
        width,
    );
    response.tip(&card.label).clicked()
}

/// Sliders and switches for `def`'s parameters, one row each, with
/// `[ reset ]`; returns whether a value changed.
fn params_editor(
    ui: &mut egui::Ui,
    def: &EffectDef,
    params: &mut BTreeMap<String, [f32; MAX_PARAMS]>,
) -> bool {
    if def.params.is_empty() {
        return false;
    }
    let tokens = Tokens::get(ui.ctx());
    let mut values = params.get(&def.id).map_or_else(
        || def.default_params(),
        |values| def.sanitize_params(*values),
    );
    let before = values;
    let mut reset = false;
    for (index, param) in def.params.iter().enumerate() {
        // A themed tint's colours are the theme's.
        if def.themed && param.kind == ParamKind::Colour {
            continue;
        }
        ui.horizontal(|ui| {
            ui.set_min_height(24.0);
            ui.label(
                egui::RichText::new(param.label.to_lowercase())
                    .font(theme::mono(12.0))
                    .color(tokens.text_2),
            );
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| match param.kind {
                    ParamKind::Float { min, max } => {
                        let width = (ui.available_width() - 48.0).clamp(60.0, 200.0);
                        ui.add(
                            Slider::new(&mut values[index], min..=max)
                                .width(width)
                                .label(&param.label),
                        );
                    }
                    ParamKind::Toggle => {
                        let mut on = values[index] >= 0.5;
                        if ui.add(Switch(&mut on)).changed() {
                            values[index] = if on { 1.0 } else { 0.0 };
                        }
                    }
                    ParamKind::Colour => {
                        let mut rgb = glyphst_effects::unpack_colour(values[index]);
                        if egui::widgets::color_picker::color_edit_button_srgb(ui, &mut rgb)
                            .changed()
                        {
                            values[index] = glyphst_effects::pack_colour(rgb);
                        }
                        ui.label(
                            RichText::new(format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]))
                                .font(theme::mono(11.0))
                                .color(tokens.muted),
                        );
                    }
                },
            );
        });
    }
    if params.contains_key(&def.id) && ui.add(Button::bracket_muted("reset")).clicked() {
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
    theme: ThemeColours,
) -> Vec<Card> {
    first
        .into_iter()
        .chain(choosable(effects, kind).map(|def| Card {
            choice: Choice::Effect(def.id.clone()),
            label: def.name.clone(),
            preview: Some(themed_use(def, params, theme)),
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

    glyphst_ui::settings::group(ui, "Page transition", |group| {
        group.full(|ui| {
            let cards = cards_for(
                effects,
                EffectKind::Transition,
                &prefs.effect_params,
                has_wallpaper,
                vec![off_card()],
                theme_colours(&tokens),
            );
            let selected = as_choice(prefs.transition.as_deref());
            let card = fill_card(ui.available_width());
            match gallery(
                ui,
                "settings-transition",
                EffectKind::Transition,
                &cards,
                &selected,
                card,
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

    glyphst_ui::settings::group(ui, "Motion", |group| {
        group.row(
            "smooth scrolling",
            Some("keys and jumps glide to where they go"),
            |ui| {
                ui.add(Switch(&mut prefs.smooth_scrolling));
            },
        );
        group.row(
            "reduce motion",
            Some("replace all motion with short fades"),
            |ui| {
                ui.add(Switch(&mut prefs.reduce_motion));
            },
        );
    });

    glyphst_ui::settings::group(ui, "Your effects", |group| {
        let folder_text = folder.map(|folder| folder.display().to_string());
        group.row(
            "Effects folder",
            Some(
                "WGSL files here join the galleries and reload when they change. \
                 A book can have its own choices in the Effects drawer.",
            ),
            |ui| {
                if ui
                    .add(glyphst_ui::button::Button::plain("new effect").icon("file-plus"))
                    .clicked()
                {
                    actions.push(Action::NewEffect);
                }
                if ui
                    .add(glyphst_ui::button::Button::plain("open folder").icon("folder-open"))
                    .clicked()
                {
                    actions.push(Action::OpenFolder);
                }
            },
        );
        if let Some(folder) = folder_text {
            group.full(|ui| {
                ui.label(
                    RichText::new(folder)
                        .font(theme::mono(11.5))
                        .color(tokens.muted),
                );
            });
        }
        if !errors.is_empty() {
            group.full(|ui| {
                for error in errors {
                    ui.horizontal_wrapped(|ui| {
                        ui.add(icons::image("triangle-alert", 14.0, tokens.err));
                        ui.label(RichText::new(error).small().color(tokens.err));
                    });
                }
            });
        }
    });
}

/// What fills the window behind the pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// The solid effect in the theme's base colour.
    Theme,
    /// The solid effect in a colour of the user's.
    Colour,
    Wallpaper,
    /// Any other background effect: the built-in animated ones and the
    /// user's.
    Effect,
}

impl Source {
    fn of(prefs: &Prefs) -> Self {
        match prefs.background.as_str() {
            glyphst_effects::SOLID if prefs.effect_params.contains_key(glyphst_effects::SOLID) => {
                Self::Colour
            }
            glyphst_effects::SOLID => Self::Theme,
            glyphst_effects::IMAGE => Self::Wallpaper,
            _ => Self::Effect,
        }
    }
}

/// What Settings › Appearance › background needs.
pub struct BackgroundGroup<'a> {
    pub prefs: &'a mut Prefs,
    pub effects: &'a [EffectDef],
    pub time: f32,
    pub actions: &'a mut Vec<Action>,
}

/// Settings › Appearance › background: the source, then its settings (a
/// colour; the wallpaper with its dim, blur and parallax; or the gallery
/// of effect backgrounds).
pub fn background_group(ui: &mut egui::Ui, page: BackgroundGroup<'_>) {
    let tokens = Tokens::get(ui.ctx());
    let BackgroundGroup {
        prefs,
        effects,
        time,
        actions,
    } = page;
    let find = |id: &str| effects.iter().find(|def| def.id == id);
    let solid = glyphst_effects::SOLID;
    glyphst_ui::settings::group(ui, "background", |group| {
        let mut source = Source::of(prefs);
        let before = source;
        group.row(
            "source",
            Some("what fills the window behind the pages"),
            |ui| {
                glyphst_ui::segmented::labels(
                    ui,
                    &mut source,
                    &[
                        (Source::Theme, "theme"),
                        (Source::Colour, "colour"),
                        (Source::Wallpaper, "wallpaper"),
                        (Source::Effect, "effect"),
                    ],
                );
            },
        );
        if source != before {
            match source {
                Source::Theme => {
                    prefs.background = solid.into();
                    prefs.effect_params.remove(solid);
                }
                Source::Colour => {
                    prefs.background = solid.into();
                    let [r, g, b] = tokens.bg_rgb();
                    prefs.effect_params.insert(solid.into(), [r, g, b, 0.0]);
                }
                Source::Wallpaper => prefs.background = glyphst_effects::IMAGE.into(),
                Source::Effect => {
                    let last = prefs
                        .animated_background
                        .clone()
                        .filter(|id| find(id).is_some());
                    let first = choosable(effects, EffectKind::Background)
                        .find(|def| def.id != solid && def.id != glyphst_effects::IMAGE)
                        .map(|def| def.id.clone());
                    if let Some(id) = last.or(first) {
                        prefs.background = id;
                    }
                }
            }
        }
        match source {
            Source::Theme => {}
            Source::Colour => {
                group.row("colour", None, |ui| {
                    let values = prefs
                        .effect_params
                        .entry(solid.into())
                        .or_insert([0.3; MAX_PARAMS]);
                    let mut rgb = [values[0], values[1], values[2]]
                        .map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8);
                    ui.label(
                        RichText::new(format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]))
                            .font(theme::mono(11.0))
                            .color(tokens.muted),
                    );
                    if egui::widgets::color_picker::color_edit_button_srgb(ui, &mut rgb).changed() {
                        for (value, channel) in values.iter_mut().zip(rgb) {
                            *value = f32::from(channel) / 255.0;
                        }
                    }
                });
            }
            Source::Wallpaper => {
                wallpaper_rows(group, prefs, find(glyphst_effects::IMAGE), actions)
            }
            Source::Effect => {
                let has_wallpaper = prefs.wallpaper.is_some();
                group.full(|ui| {
                    let cards: Vec<Card> = cards_for(
                        effects,
                        EffectKind::Background,
                        &prefs.effect_params,
                        has_wallpaper,
                        Vec::new(),
                        theme_colours(&tokens),
                    )
                    .into_iter()
                    .filter(|card| {
                        !matches!(&card.choice, Choice::Effect(id)
                            if id == solid || id == glyphst_effects::IMAGE)
                    })
                    .collect();
                    let selected = Choice::Effect(prefs.background.clone());
                    let card = fill_card(ui.available_width());
                    if let Some(Choice::Effect(id)) = gallery(
                        ui,
                        "settings-background",
                        EffectKind::Background,
                        &cards,
                        &selected,
                        card,
                        time,
                    ) {
                        prefs.animated_background = Some(id.clone());
                        prefs.background = id;
                    }
                    if let Some(def) = find(&prefs.background) {
                        params_editor(ui, def, &mut prefs.effect_params);
                    }
                });
                group.row(
                    "animation rate",
                    Some("frames per second for animated backgrounds"),
                    |ui| {
                        let mut fps = prefs.background_fps as f32;
                        if ui
                            .add(
                                Slider::new(&mut fps, 1.0..=120.0)
                                    .step(1.0)
                                    .width(180.0)
                                    .label("animation rate")
                                    .format(Some(Box::new(|value| format!("{value:.0} fps")))),
                            )
                            .changed()
                        {
                            prefs.background_fps = fps.round() as u32;
                        }
                    },
                );
            }
        }
    });
}

/// What Settings › Appearance › page tint needs.
pub struct TintGroup<'a> {
    pub prefs: &'a mut Prefs,
    pub effects: &'a [EffectDef],
    pub time: f32,
}

/// Settings › Appearance › page tint: the light and the dark theme's own
/// tint, chosen from a gallery, with its parameters (a custom tint's
/// colour and strength; a themed tint takes the theme's colours).
pub fn tint_group(ui: &mut egui::Ui, page: TintGroup<'_>) {
    let tokens = Tokens::get(ui.ctx());
    let TintGroup {
        prefs,
        effects,
        time,
    } = page;
    let find = |id: &str| effects.iter().find(|def| def.id == id);
    // Which theme's tint is shown: the one drawn, until another is picked.
    let editing = ui.id().with("tint-theme");
    let mut kind = ui
        .data(|data| data.get_temp::<ThemeKind>(editing))
        .unwrap_or(tokens.kind);
    glyphst_ui::settings::group(ui, "page tint", |group| {
        group.row(
            "for",
            Some("the light and the dark theme each tint pages their own way"),
            |ui| {
                glyphst_ui::segmented::labels(
                    ui,
                    &mut kind,
                    &[(ThemeKind::Light, "light"), (ThemeKind::Dark, "dark")],
                );
            },
        );
        group.full(|ui| {
            let cards = cards_for(
                effects,
                EffectKind::Overlay,
                &prefs.effect_params,
                prefs.wallpaper.is_some(),
                vec![off_card()],
                theme_colours(&tokens),
            );
            let selected = as_choice(prefs.tint(kind));
            let card = fill_card(ui.available_width());
            match gallery(
                ui,
                "settings-tint",
                EffectKind::Overlay,
                &cards,
                &selected,
                card,
                time,
            ) {
                Some(Choice::Effect(id)) => *prefs.tint_mut(kind) = Some(id),
                Some(_) => *prefs.tint_mut(kind) = None,
                None => {}
            }
            if let Some(def) = prefs.tint(kind).and_then(find) {
                params_editor(ui, def, &mut prefs.effect_params);
            }
        });
    });
    ui.data_mut(|data| data.insert_temp(editing, kind));
}

/// The wallpaper's file, then its dim, blur and parallax (the wallpaper
/// effect's parameters, in manifest order).
fn wallpaper_rows(
    group: &mut glyphst_ui::settings::Group<'_>,
    prefs: &mut Prefs,
    def: Option<&EffectDef>,
    actions: &mut Vec<Action>,
) {
    let name = prefs
        .wallpaper
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .map(|name| name.to_string_lossy().into_owned());
    group.row(
        "wallpaper",
        Some(name.as_deref().unwrap_or("no image chosen")),
        |ui| {
            if ui
                .add(Button::plain("choose…").icon("folder-open"))
                .clicked()
            {
                actions.push(Action::ChooseWallpaper);
            }
            if name.is_some() && ui.add(Button::plain("clear").icon("x")).clicked() {
                prefs.wallpaper = None;
            }
        },
    );
    let Some(def) = def else {
        return;
    };
    let mut values = prefs.effect_params.get(&def.id).map_or_else(
        || def.default_params(),
        |values| def.sanitize_params(*values),
    );
    let before = values;
    let slider = |ui: &mut egui::Ui, value: &mut f32, index: usize, label: &str| {
        if let Some(ParamKind::Float { min, max }) = def.params.get(index).map(|param| &param.kind)
        {
            let (min, max) = (*min, *max);
            ui.add(Slider::new(value, min..=max).width(200.0).label(label));
        }
    };
    group.row("dim", Some("darkens it, so pages stand out"), |ui| {
        slider(ui, &mut values[0], 0, "dim");
    });
    group.row("blur", Some("softens the image behind the pages"), |ui| {
        slider(ui, &mut values[1], 1, "blur");
    });
    let mut parallax = values[2] >= 0.5;
    group.row(
        "parallax",
        Some("drifts a little against the pointer"),
        |ui| {
            if ui.add(Switch(&mut parallax)).changed() {
                values[2] = if parallax { 1.0 } else { 0.0 };
            }
        },
    );
    if parallax {
        group.row("depth", Some("how far it drifts"), |ui| {
            slider(ui, &mut values[3], 3, "depth");
        });
    }
    if values != before {
        prefs
            .effect_params
            .insert(def.id.clone(), def.sanitize_params(values));
    }
}

/// What the Effects tab needs: one book's choices, the defaults' names,
/// and the settings it shares with Settings (parameters, smooth scrolling
/// and reduce motion).
pub struct BookPage<'a> {
    pub title: &'a str,
    pub book: &'a mut BookEffects,
    pub default_background: Option<&'a EffectDef>,
    pub default_transition: Option<&'a EffectDef>,
    pub default_overlay: Option<&'a EffectDef>,
    pub effects: &'a [EffectDef],
    pub params: &'a mut BTreeMap<String, [f32; MAX_PARAMS]>,
    pub has_wallpaper: bool,
    pub smooth_scrolling: &'a mut bool,
    pub reduce_motion: &'a mut bool,
    pub time: f32,
}

/// What the Effects tab changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BookChanges {
    pub book: bool,
    pub params: bool,
}

/// The Effects tab (frame 03): this book's background as tiles, its tint
/// and the parameters in play as sliders, the page transition, smooth
/// scrolling and reduce motion.
pub fn book_page(ui: &mut egui::Ui, page: BookPage<'_>) -> BookChanges {
    let BookPage {
        title,
        book,
        default_background,
        default_transition,
        default_overlay,
        effects,
        params,
        has_wallpaper,
        smooth_scrolling,
        reduce_motion,
        time,
    } = page;
    let tokens = Tokens::get(ui.ctx());
    let mut changes = BookChanges::default();
    let default_card = |default: Option<&EffectDef>,
                        params: &BTreeMap<String, [f32; MAX_PARAMS]>| Card {
        choice: Choice::Default,
        label: format!(
            "default · {}",
            default.map_or("none", |def| def.name.as_str())
        ),
        preview: default.map(|def| themed_use(def, params, theme_colours(&tokens))),
        user: false,
        needs_wallpaper: default.is_some_and(|def| def.image) && !has_wallpaper,
    };
    // Tiles three to a row.
    let tile = |ui: &egui::Ui| {
        let width = ((ui.available_width() - 2.0 * GAP) / 3.0).floor().max(48.0);
        vec2(width, (width * 0.6).round())
    };
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!("for {} only", title.to_lowercase()))
                    .font(theme::mono(11.0))
                    .color(tokens.faint),
            );
            ui.add_space(6.0);
            motif::rule_title(ui, "background", None);
            ui.add_space(4.0);
            let cards = cards_for(
                effects,
                EffectKind::Background,
                params,
                has_wallpaper,
                vec![default_card(default_background, params)],
                theme_colours(&tokens),
            );
            let size = tile(ui);
            if let Some(picked) = gallery(
                ui,
                "book-background",
                EffectKind::Background,
                &cards,
                &book.background,
                size,
                time,
            ) && picked != book.background
            {
                book.background = picked;
                changes.book = true;
            }
            ui.add_space(8.0);
            motif::rule_title(ui, "tint & grain", None);
            ui.add_space(4.0);
            let mut first = vec![default_card(default_overlay, params)];
            first.push(off_card());
            let cards = cards_for(
                effects,
                EffectKind::Overlay,
                params,
                has_wallpaper,
                first,
                theme_colours(&tokens),
            );
            if let Some(picked) = gallery(
                ui,
                "book-overlay",
                EffectKind::Overlay,
                &cards,
                &book.overlay,
                vec2(size.x, (size.y * 0.7).round()),
                time,
            ) && picked != book.overlay
            {
                book.overlay = picked;
                changes.book = true;
            }
            // The parameters of what this book shows.
            fn shown<'e>(
                effects: &'e [EffectDef],
                choice: &Choice,
                default: Option<&'e EffectDef>,
            ) -> Option<&'e EffectDef> {
                match choice {
                    Choice::Default => default,
                    Choice::Off => None,
                    Choice::Effect(id) => effects.iter().find(|def| &def.id == id),
                }
            }
            for def in [
                shown(effects, &book.background, default_background),
                shown(effects, &book.overlay, default_overlay),
            ]
            .into_iter()
            .flatten()
            {
                ui.push_id(&def.id, |ui| {
                    changes.params |= params_editor(ui, def, params);
                });
            }
            ui.add_space(8.0);
            motif::rule_title(ui, "page transition", None);
            ui.add_space(6.0);
            let mut options: Vec<(Choice, String)> = vec![
                (
                    Choice::Default,
                    format!(
                        "default · {}",
                        default_transition.map_or("none".to_owned(), |def| def.name.to_lowercase())
                    ),
                ),
                (Choice::Off, "none".to_owned()),
            ];
            options.extend(
                choosable(effects, EffectKind::Transition)
                    .map(|def| (Choice::Effect(def.id.clone()), def.name.to_lowercase())),
            );
            // As many as fit on a line, wrapping: the choices outgrow a
            // segmented control.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                for (choice, label) in &options {
                    let chosen = *choice == book.transition;
                    if ui
                        .add(Button::plain(label).selected(chosen).small())
                        .clicked()
                        && !chosen
                    {
                        book.transition = choice.clone();
                        changes.book = true;
                    }
                }
            });
            ui.add_space(10.0);
            for (label, on) in [
                ("smooth scrolling", smooth_scrolling),
                ("reduce motion", reduce_motion),
            ] {
                ui.horizontal(|ui| {
                    ui.set_min_height(24.0);
                    ui.label(
                        RichText::new(label)
                            .font(theme::mono(12.0))
                            .color(tokens.text_2),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(Switch(on));
                    });
                });
            }
            if !book.is_default() {
                ui.add_space(6.0);
                if ui
                    .add(Button::bracket_muted("use the defaults for this book"))
                    .clicked()
                {
                    let used = book.used;
                    *book = BookEffects {
                        used,
                        ..Default::default()
                    };
                    changes.book = true;
                }
            }
        });
    changes
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
    fn settings_cards_fill_the_row() {
        // Frame 07: four cards fill the 684 px page; five would not fit.
        let card = fill_card(684.0);
        assert_eq!(card, vec2(161.0, (161.0 * CARD_ASPECT).round()));
        // Three at 500, still no narrower than the minimum.
        assert_eq!(fill_card(500.0).x, 157.0);
        // One card takes a narrow row.
        assert_eq!(fill_card(120.0).x, 119.0);
    }

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
