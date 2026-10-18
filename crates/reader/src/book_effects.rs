//! Which effects a book shows: its own choices (from the Effects drawer),
//! else the defaults (Settings › Effects), else the built-ins. Choices
//! name effects by id; one whose effect has gone (a user file removed)
//! falls back without being forgotten, in case the file comes back.

use std::collections::BTreeMap;

use glyphst_effects::{EffectDef, EffectKind, MAX_PARAMS, ParamKind, RANDOM, SOLID};
use glyphst_ui::theme::Tokens;
use serde::{Deserialize, Serialize};

use crate::gpu::EffectUse;

/// Books whose choices are kept; the least recently used go first.
pub const MAX_BOOKS: usize = 500;

/// One choice of a book.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    /// The default from Settings.
    #[default]
    Default,
    /// No transition or no page tint (not offered for backgrounds).
    Off,
    Effect(String),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BookEffects {
    pub background: Choice,
    /// The page turn's transition (presenting).
    pub transition: Choice,
    /// The transition when switching to this book; may be [`RANDOM`].
    pub switch: Choice,
    pub overlay: Choice,
    /// When it was last chosen or shown, as a counter, for trimming.
    pub used: u64,
}

impl BookEffects {
    pub fn is_default(&self) -> bool {
        [
            &self.background,
            &self.transition,
            &self.switch,
            &self.overlay,
        ]
        .iter()
        .all(|choice| **choice == Choice::Default)
    }
}

/// The defaults: what Settings chose (the overlay is the page tint of the
/// theme drawn).
#[derive(Clone, Copy, Debug)]
pub struct Defaults<'a> {
    pub background: &'a str,
    pub transition: Option<&'a str>,
    /// The book switch's transition; may be [`RANDOM`].
    pub switch: Option<&'a str>,
    pub overlay: Option<&'a str>,
    /// Values the user set per effect id.
    pub params: &'a BTreeMap<String, [f32; MAX_PARAMS]>,
    /// What themed overlays draw with.
    pub theme: ThemeColours,
}

/// The theme's ink and paper for themed overlays: its text and its panes'
/// colour (sRGB, without the window's opacity).
pub type ThemeColours = [[u8; 3]; 2];

pub fn theme_colours(tokens: &Tokens) -> ThemeColours {
    [tokens.text, tokens.surface].map(|color| {
        let [r, g, b, _] = color.to_srgba_unmultiplied();
        [r, g, b]
    })
}

/// A book's effects as drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub background: EffectUse,
    pub transition: Option<EffectUse>,
    /// The book switch's: an effect, or [`RANDOM`] to pick one per switch.
    pub switch: Option<EffectUse>,
    pub overlay: Option<EffectUse>,
}

/// `def` with the user's values for its parameters (or its defaults).
pub fn effect_use(def: &EffectDef, params: &BTreeMap<String, [f32; MAX_PARAMS]>) -> EffectUse {
    EffectUse {
        id: def.id.clone(),
        params: params.get(&def.id).map_or_else(
            || def.default_params(),
            |values| def.sanitize_params(*values),
        ),
    }
}

/// [`effect_use`], with a themed overlay's first two colour parameters
/// set to the theme's ink and paper.
pub fn themed_use(
    def: &EffectDef,
    params: &BTreeMap<String, [f32; MAX_PARAMS]>,
    theme: ThemeColours,
) -> EffectUse {
    let mut effect = effect_use(def, params);
    if def.themed {
        let colours = def
            .params
            .iter()
            .enumerate()
            .filter(|(_, param)| param.kind == ParamKind::Colour)
            .map(|(index, _)| index);
        for (index, colour) in colours.zip(theme) {
            effect.params[index] = glyphst_effects::pack_colour(colour);
        }
    }
    effect
}

/// The effects for a book with `book`'s choices (if any).
pub fn resolve<'a>(
    book: Option<&BookEffects>,
    defaults: Defaults<'_>,
    find: impl Fn(&str) -> Option<&'a EffectDef>,
) -> Resolved {
    // An effect of `kind` named `id`, if it is (still) there.
    let usable =
        |id: &str, kind: EffectKind| find(id).filter(|def| def.kind == kind && !def.hidden);
    let pick = |choice: Option<&Choice>, default: Option<&str>, kind| match choice {
        Some(Choice::Off) => None,
        Some(Choice::Effect(id)) if usable(id, kind).is_some() => usable(id, kind),
        _ => default.and_then(|id| usable(id, kind)),
    };
    let background = pick(
        book.map(|book| &book.background)
            .filter(|choice| **choice != Choice::Off),
        Some(defaults.background),
        EffectKind::Background,
    )
    .or_else(|| usable(SOLID, EffectKind::Background));
    Resolved {
        background: background.map_or_else(
            || EffectUse {
                id: SOLID.into(),
                params: [0.0; MAX_PARAMS],
            },
            |def| effect_use(def, defaults.params),
        ),
        transition: pick(
            book.map(|book| &book.transition),
            defaults.transition,
            EffectKind::Transition,
        )
        .map(|def| effect_use(def, defaults.params)),
        switch: {
            let random = |id: &str| id == RANDOM;
            let chosen = match book.map(|book| &book.switch) {
                Some(Choice::Effect(id))
                    if random(id) || usable(id, EffectKind::Transition).is_some() =>
                {
                    Some(Some(id.as_str()))
                }
                Some(Choice::Off) => Some(None),
                _ => None,
            };
            match chosen.unwrap_or(defaults.switch) {
                Some(id) if random(id) => Some(EffectUse {
                    id: RANDOM.into(),
                    params: [0.0; MAX_PARAMS],
                }),
                Some(id) => {
                    usable(id, EffectKind::Transition).map(|def| effect_use(def, defaults.params))
                }
                None => None,
            }
        },
        overlay: pick(
            book.map(|book| &book.overlay),
            defaults.overlay,
            EffectKind::Overlay,
        )
        .map(|def| themed_use(def, defaults.params, defaults.theme)),
    }
}

/// Keeps at most [`MAX_BOOKS`] books with choices, dropping the least
/// recently used and those back at the defaults.
pub fn trim(books: &mut BTreeMap<String, BookEffects>) {
    books.retain(|_, book| !book.is_default());
    if books.len() <= MAX_BOOKS {
        return;
    }
    let mut used: Vec<u64> = books.values().map(|book| book.used).collect();
    used.sort_unstable();
    let cutoff = used[books.len() - MAX_BOOKS];
    books.retain(|_, book| book.used >= cutoff);
    // Equal counters past the cutoff: drop the extra in key order.
    while books.len() > MAX_BOOKS {
        let Some(key) = books
            .iter()
            .filter(|(_, book)| book.used == cutoff)
            .map(|(key, _)| key.clone())
            .next()
        else {
            break;
        };
        books.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use glyphst_effects::{ParamDef, ParamKind};

    use super::*;

    fn def(id: &str, kind: EffectKind) -> EffectDef {
        EffectDef {
            id: id.into(),
            name: id.into(),
            kind,
            animated: false,
            duration_ms: 300,
            image: false,
            hidden: false,
            themed: false,
            params: vec![ParamDef {
                id: "amount".into(),
                label: "Amount".into(),
                kind: ParamKind::Float { min: 0.0, max: 1.0 },
                default: 0.5,
                random: false,
            }],
            source: format!("{id}.wgsl"),
            user: false,
        }
    }

    #[test]
    fn a_books_choice_wins_over_the_defaults_and_vanished_effects_fall_back() {
        let effects = [
            def(SOLID, EffectKind::Background),
            def("aurora", EffectKind::Background),
            def("slide-over", EffectKind::Transition),
            def("crossfade", EffectKind::Transition),
            def("night", EffectKind::Overlay),
        ];
        let find = |id: &str| effects.iter().find(|def| def.id == id);
        let mut params = BTreeMap::new();
        params.insert("night".to_string(), [3.0, 0.0, 0.0, 0.0]);
        let defaults = Defaults {
            background: "aurora",
            transition: Some("slide-over"),
            switch: Some(RANDOM),
            overlay: None,
            params: &params,
            theme: [[0; 3], [255; 3]],
        };

        let plain = resolve(None, defaults, find);
        assert_eq!(plain.background.id, "aurora");
        assert_eq!(plain.transition.unwrap().id, "slide-over");
        assert_eq!(plain.overlay, None);
        assert_eq!(plain.switch.unwrap().id, RANDOM, "random is kept");

        let book = BookEffects {
            background: Choice::Effect("gone".into()),
            transition: Choice::Off,
            switch: Choice::Effect("crossfade".into()),
            overlay: Choice::Effect("night".into()),
            used: 1,
        };
        let chosen = resolve(Some(&book), defaults, find);
        assert_eq!(
            chosen.background.id, "aurora",
            "a vanished effect falls back"
        );
        assert_eq!(chosen.transition, None);
        assert_eq!(chosen.switch.unwrap().id, "crossfade");
        let off = BookEffects {
            switch: Choice::Off,
            ..BookEffects::default()
        };
        assert_eq!(resolve(Some(&off), defaults, find).switch, None);
        let gone = BookEffects {
            switch: Choice::Effect("gone".into()),
            ..BookEffects::default()
        };
        assert_eq!(
            resolve(Some(&gone), defaults, find).switch.unwrap().id,
            RANDOM,
            "a vanished switch falls back to the default"
        );
        let overlay = chosen.overlay.unwrap();
        assert_eq!(overlay.id, "night");
        assert_eq!(overlay.params[0], 1.0, "params kept within range");

        // A default that names the wrong kind or nothing ends at Solid.
        let odd = Defaults {
            background: "crossfade",
            ..defaults
        };
        assert_eq!(resolve(None, odd, find).background.id, SOLID);
    }

    /// A themed tint draws with the theme's ink and paper, whatever was
    /// saved for it; its other parameters stay the user's.
    #[test]
    fn themed_tints_take_the_themes_colours() {
        let mut tint = def("theme-tint", EffectKind::Overlay);
        tint.themed = true;
        let colour = |id: &str| ParamDef {
            id: id.into(),
            label: id.into(),
            kind: ParamKind::Colour,
            default: 0.0,
            random: false,
        };
        tint.params = vec![colour("ink"), colour("paper"), tint.params[0].clone()];
        let mut params = BTreeMap::new();
        params.insert("theme-tint".to_string(), [5.0, 6.0, 0.25, 0.0]);
        let effect = themed_use(&tint, &params, [[1, 2, 3], [250, 251, 252]]);
        assert_eq!(glyphst_effects::unpack_colour(effect.params[0]), [1, 2, 3]);
        assert_eq!(
            glyphst_effects::unpack_colour(effect.params[1]),
            [250, 251, 252]
        );
        assert_eq!(effect.params[2], 0.25);
    }

    #[test]
    fn only_the_most_recently_used_books_are_kept() {
        let mut books: BTreeMap<String, BookEffects> = (0..MAX_BOOKS as u64 + 20)
            .map(|used| {
                (
                    format!("/book-{used:04}.pdf"),
                    BookEffects {
                        overlay: Choice::Off,
                        used,
                        ..Default::default()
                    },
                )
            })
            .collect();
        books.insert("/plain.pdf".into(), BookEffects::default());
        trim(&mut books);
        assert_eq!(books.len(), MAX_BOOKS);
        assert!(!books.contains_key("/plain.pdf"));
        assert!(!books.contains_key("/book-0019.pdf"));
        assert!(books.contains_key("/book-0020.pdf"));
    }
}
