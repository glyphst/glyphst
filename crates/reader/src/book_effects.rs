//! Which effects a book shows: its own choices (from the Effects drawer),
//! else the defaults (Settings › Effects), else the built-ins. Choices
//! name effects by id; one whose effect has gone (a user file removed)
//! falls back without being forgotten, in case the file comes back.

use std::collections::BTreeMap;

use glyphst_effects::{EffectDef, EffectKind, MAX_PARAMS, SOLID};
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
    pub transition: Choice,
    pub overlay: Choice,
    /// When it was last chosen or shown, as a counter, for trimming.
    pub used: u64,
}

impl BookEffects {
    pub fn is_default(&self) -> bool {
        [&self.background, &self.transition, &self.overlay]
            .iter()
            .all(|choice| **choice == Choice::Default)
    }
}

/// The defaults: what Settings › Effects chose.
#[derive(Clone, Copy, Debug)]
pub struct Defaults<'a> {
    pub background: &'a str,
    pub transition: Option<&'a str>,
    pub overlay: Option<&'a str>,
    /// Values the user set per effect id.
    pub params: &'a BTreeMap<String, [f32; MAX_PARAMS]>,
}

/// A book's effects as drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub background: EffectUse,
    pub transition: Option<EffectUse>,
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
        overlay: pick(
            book.map(|book| &book.overlay),
            defaults.overlay,
            EffectKind::Overlay,
        )
        .map(|def| effect_use(def, defaults.params)),
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
            params: vec![ParamDef {
                id: "amount".into(),
                label: "Amount".into(),
                kind: ParamKind::Float { min: 0.0, max: 1.0 },
                default: 0.5,
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
            overlay: None,
            params: &params,
        };

        let plain = resolve(None, defaults, find);
        assert_eq!(plain.background.id, "aurora");
        assert_eq!(plain.transition.unwrap().id, "slide-over");
        assert_eq!(plain.overlay, None);

        let book = BookEffects {
            background: Choice::Effect("gone".into()),
            transition: Choice::Off,
            overlay: Choice::Effect("night".into()),
            used: 1,
        };
        let chosen = resolve(Some(&book), defaults, find);
        assert_eq!(
            chosen.background.id, "aurora",
            "a vanished effect falls back"
        );
        assert_eq!(chosen.transition, None);
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
