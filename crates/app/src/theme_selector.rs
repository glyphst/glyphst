use crate::{
    icons::{Icon, IconName},
    picker::{Picker, PickerDelegate, fuzzy_match},
    settings::{SettingsStore, ui_rems},
    style::TextSize,
    theme::{Theme, ThemeAppearance, ThemeMetadata, ThemeSelection},
};
use gpui::{
    AnyElement, Context, FocusHandle, HighlightStyle, SharedString, StyledText, Window, div,
    prelude::*, rgb,
};
use std::ops::Range;

const MAX_VISIBLE_MATCHES: usize = 10;
const MATCH_ROW_HEIGHT: f32 = 40.0;

pub type ThemeSelector = Picker<ThemeSelectorDelegate>;

struct ThemeMatch {
    candidate_ix: usize,
    highlights: Vec<Range<usize>>,
    score: usize,
    name: String,
}

pub struct ThemeSelectorDelegate {
    themes: Vec<ThemeMetadata>,
    matches: Vec<ThemeMatch>,
    original_selection: ThemeSelection,
    original_system_appearance: ThemeAppearance,
    original_theme_id: Option<usize>,
    selection_completed: bool,
    selected_index: usize,
}

impl ThemeSelectorDelegate {
    pub fn new(
        themes: Vec<ThemeMetadata>,
        selection: ThemeSelection,
        system_appearance: ThemeAppearance,
    ) -> Self {
        let mut themes = themes;
        themes.sort_unstable_by(|a, b| {
            a.appearance
                .as_str()
                .cmp(b.appearance.as_str())
                .then(a.name.cmp(&b.name))
        });

        let active_name = selection.name(system_appearance).to_string();
        let original_theme_id = themes.iter().position(|meta| meta.id == active_name);

        let matches = themes
            .iter()
            .enumerate()
            .map(|(ix, meta)| ThemeMatch {
                candidate_ix: ix,
                highlights: Vec::new(),
                score: 0,
                name: meta.name.clone(),
            })
            .collect();

        let selected_index = original_theme_id.unwrap_or(0);

        Self {
            themes,
            matches,
            original_selection: selection,
            original_system_appearance: system_appearance,
            original_theme_id,
            selection_completed: false,
            selected_index,
        }
    }

    fn is_original_theme(&self, match_ix: usize) -> bool {
        self.matches
            .get(match_ix)
            .zip(self.original_theme_id)
            .is_some_and(|(mat, original_id)| mat.candidate_ix == original_id)
    }

    fn apply_theme_from_match(&mut self, match_ix: usize, cx: &mut Context<Picker<Self>>) {
        let Some(mat) = self.matches.get(match_ix) else {
            return;
        };
        let Some(metadata) = self.themes.get(mat.candidate_ix) else {
            return;
        };
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        settings.theme.set_theme(
            metadata.id.clone(),
            metadata.appearance,
            self.original_system_appearance,
        );
        cx.global_mut::<SettingsStore>().override_settings(settings);
    }

    fn revert(&mut self, cx: &mut Context<Picker<Self>>) {
        if !self.selection_completed {
            let mut settings = cx.global::<SettingsStore>().settings.clone();
            settings.theme = self.original_selection.clone();
            cx.global_mut::<SettingsStore>().override_settings(settings);
            self.selection_completed = true;
        }
    }
}

impl PickerDelegate for ThemeSelectorDelegate {
    fn key_context(&self) -> &'static str {
        "ThemeSelector Menu TextInput"
    }

    fn placeholder(&self) -> SharedString {
        "Select Theme…".into()
    }

    fn empty_message(&self) -> SharedString {
        "No themes found".into()
    }

    fn width(&self) -> f32 {
        480.0
    }

    fn row_height(&self) -> f32 {
        MATCH_ROW_HEIGHT
    }

    fn max_visible_matches(&self) -> usize {
        MAX_VISIBLE_MATCHES
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn update_matches(&mut self, query: &str) {
        let mut matches = Vec::new();
        for (ix, meta) in self.themes.iter().enumerate() {
            if let Some((highlights, score)) = fuzzy_match(&meta.name, query) {
                matches.push(ThemeMatch {
                    candidate_ix: ix,
                    highlights,
                    score,
                    name: meta.name.clone(),
                });
            }
        }
        matches.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.cmp(&b.name)));

        if query.is_empty() {
            self.selected_index = self.original_theme_id.unwrap_or(0);
        } else if let Some(selected_name) = self
            .matches
            .get(self.selected_index)
            .map(|m| m.name.clone())
        {
            self.selected_index = matches
                .iter()
                .position(|m| m.name == selected_name)
                .unwrap_or(0);
        } else {
            self.selected_index = 0;
        }

        self.matches = matches;
        self.selected_index = self
            .selected_index
            .min(self.matches.len().saturating_sub(1));
    }

    fn render_match(
        &self,
        match_ix: usize,
        theme: Theme,
        _previous_focus_handle: &FocusHandle,
        _window: &Window,
    ) -> AnyElement {
        let Some(theme_match) = self.matches.get(match_ix) else {
            return div().into_any_element();
        };
        let is_original = self.is_original_theme(match_ix);
        let name = theme_match.name.clone();
        let highlights = theme_match
            .highlights
            .iter()
            .cloned()
            .map(|range| {
                (
                    range,
                    HighlightStyle {
                        color: Some(rgb(theme.match_highlight).into()),
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>();

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .w_full()
            .text_size(TextSize::XSmall.rems())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(StyledText::new(name).with_highlights(highlights)),
            )
            .when(is_original, |this| {
                this.child(
                    div()
                        .flex_none()
                        .ml_3()
                        .child(Icon::new(
                            IconName::Check,
                            ui_rems(14.0),
                            rgb(theme.text_muted),
                        )),
                )
            })
            .into_any_element()
    }

    fn confirm(
        &mut self,
        match_ix: usize,
        _previous_focus_handle: &FocusHandle,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        self.selection_completed = true;
        self.apply_theme_from_match(match_ix, cx);

        let store = cx.global::<SettingsStore>();
        let settings = store.settings.clone();
        let _ = cx.global_mut::<SettingsStore>().set_settings(settings);
        let _ = cx.global_mut::<SettingsStore>().save();
        cx.refresh_windows();
    }

    fn selection_changed(&mut self, match_ix: usize, cx: &mut Context<Picker<Self>>) {
        if !self.selection_completed {
            self.apply_theme_from_match(match_ix, cx);
            cx.refresh_windows();
        }
    }

    fn dismissed(&mut self, cx: &mut Context<Picker<Self>>) {
        self.revert(cx);
        cx.refresh_windows();
    }
}
