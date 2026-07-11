pub use glyphst_translation as service;

use glyphst_translation::TARGET_LANGUAGES;
use glyphst_ui::searchable_dropdown::SearchableItem;

pub fn target_language_items(selected: &str) -> Vec<SearchableItem> {
    let mut items = TARGET_LANGUAGES
        .iter()
        .map(|language| SearchableItem {
            value: language.code.to_string(),
            label: language.label.to_string(),
        })
        .collect::<Vec<_>>();
    if !items.iter().any(|item| item.value == selected) {
        items.insert(
            0,
            SearchableItem {
                value: selected.to_string(),
                label: selected.to_string(),
            },
        );
    }
    items
}
