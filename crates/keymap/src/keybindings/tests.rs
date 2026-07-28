use super::*;
use gpui::{DummyKeyboardMapper, KeyContext, Keymap, Keystroke};

fn assert_action_aliases<A>(aliases: &[&str])
where
    A: Action + Default + 'static,
{
    let expected = A::default().name().to_string();

    for alias in aliases {
        let specs = command_specs()
            .iter()
            .filter(|spec| action_matches_spec(alias, spec) && spec.legacy_default)
            .collect::<Vec<_>>();
        assert_eq!(specs.len(), 1, "alias `{alias}` should resolve once");
        assert_eq!((specs[0].build)().name(), expected);
        assert_eq!(specs[0].category, KeyBindingCategory::DocumentView);
    }
}

#[test]
fn search_navigation_aliases_resolve_to_result_actions() {
    assert_action_aliases::<NextSearchResult>(&[
        "next-search-result",
        "search-next",
        "search-next-result",
        "select-next-match",
        "search::SelectNextMatch",
    ]);

    assert_action_aliases::<PrevSearchResult>(&[
        "prev-search-result",
        "previous-search-result",
        "search-prev",
        "search-previous",
        "search-previous-result",
        "select-prev-match",
        "select-previous-match",
        "search::SelectPrevMatch",
        "search::SelectPreviousMatch",
    ]);
}

#[test]
fn rotate_aliases_resolve_to_rotate_actions() {
    assert_action_aliases::<RotateClockwise>(&["rotate-clockwise", "rotate", "rotate-cw"]);
    assert_action_aliases::<RotateCounterclockwise>(&["rotate-counterclockwise", "rotate-ccw"]);
}

#[test]
fn rotate_and_reload_bindings_do_not_collide() {
    for preset in [KeymapPreset::Modern, KeymapPreset::Vim] {
        let rows = editable_bindings(preset, &[]);
        let document_binding = |action| {
            rows.iter()
                .find(|row| {
                    row.action == action && row.category == KeyBindingCategory::DocumentView
                })
                .expect("document command binding should exist")
        };

        assert_eq!(
            document_binding("rotate-clockwise").keystrokes,
            vec!["r".to_string()]
        );
        assert_eq!(
            document_binding("rotate-counterclockwise").keystrokes,
            vec!["shift-r".to_string()]
        );
        assert_eq!(
            document_binding("reload").keystrokes,
            vec!["ctrl-r".to_string()]
        );
    }
}

#[test]
fn categories_hide_raw_predicates() {
    assert_eq!(KeyBindingCategory::DocumentView.label(), "Document Reader");
    let predicate = KeyBindingCategory::DocumentView.predicate();
    assert!(predicate.starts_with("DocumentView && !GridOverview"));
    assert!(predicate.contains("!Search"));

    let predicate = KeyBindingCategory::PageOverview.predicate();
    assert!(predicate.starts_with("GridOverview && !CommandPalette"));
    assert!(predicate.contains("!Search"));
}

#[test]
fn document_view_category_uses_canonical_id() {
    assert_eq!(KeyBindingCategory::DocumentView.id(), "document_view");
    assert_eq!(
        KeyBindingCategory::from_id("document_view"),
        Some(KeyBindingCategory::DocumentView)
    );
}

#[test]
fn primary_modifier_expands_for_current_platform() {
    let expected = if cfg!(target_os = "macos") {
        "cmd-shift-p cmd-o"
    } else {
        "ctrl-shift-p ctrl-o"
    };

    assert_eq!(
        expand_platform_keystroke("primary-shift-p primary-o"),
        expected
    );
}

#[test]
fn action_can_have_multiple_categories() {
    let rows = editable_bindings(KeymapPreset::Vim, &[])
        .into_iter()
        .filter(|row| row.action == "next-search-result")
        .collect::<Vec<_>>();
    assert!(
        rows.iter()
            .any(|row| row.category == KeyBindingCategory::DocumentView)
    );
    assert!(
        rows.iter()
            .any(|row| row.category == KeyBindingCategory::SearchResults)
    );
    assert!(
        rows.iter()
            .any(|row| row.category == KeyBindingCategory::PageOverview)
    );
}

#[test]
fn command_palette_is_available_in_workspace_and_page_overview() {
    let expected = vec![expand_platform_keystroke("primary-shift-p")];

    for preset in [KeymapPreset::Modern, KeymapPreset::Vim] {
        let rows = editable_bindings(preset, &[])
            .into_iter()
            .filter(|row| row.action == "toggle-command-palette")
            .collect::<Vec<_>>();

        assert_eq!(rows.len(), 2);
        for category in [
            KeyBindingCategory::Workspace,
            KeyBindingCategory::PageOverview,
        ] {
            let row = rows
                .iter()
                .find(|row| row.category == category)
                .expect("command palette binding should exist in category");
            assert_eq!(row.keystrokes, expected);
            assert_eq!(row.default_keystrokes, expected);
        }
    }
}

#[test]
fn source_reflects_user_override_versus_default() {
    let preset = KeymapPreset::Modern;
    let defaults = editable_bindings(preset, &[]);
    let sample = defaults
        .iter()
        .find(|row| !row.default_keystrokes.is_empty())
        .expect("expected at least one bound command");

    // Without an override the effective keystrokes equal the defaults (Source = Default).
    assert_eq!(sample.keystrokes, sample.default_keystrokes);

    let overrides = vec![KeyBindingOverride {
        action: sample.action.to_string(),
        keystrokes: "test-override".to_string(),
        context: Some(sample.category.id().to_string()),
    }];

    let overridden = editable_bindings(preset, &overrides)
        .into_iter()
        .find(|row| row.action == sample.action && row.category == sample.category)
        .expect("overridden binding should still be listed");

    // With an override the effective keystrokes diverge from the defaults (Source = User).
    assert_ne!(overridden.keystrokes, overridden.default_keystrokes);
    assert_eq!(overridden.keystrokes, vec!["test-override".to_string()]);
    assert_eq!(overridden.default_keystrokes, sample.default_keystrokes);
}

fn preset_keymap(preset: KeymapPreset) -> Keymap {
    let mut bindings = Vec::new();

    for spec in command_specs() {
        let context =
            Rc::new(KeyBindingContextPredicate::parse(&spec.category.predicate()).unwrap());
        for keystrokes in default_keystrokes(spec, preset) {
            bindings.push(
                KeyBinding::load(
                    &keystrokes,
                    (spec.build)(),
                    Some(context.clone()),
                    false,
                    None,
                    &DummyKeyboardMapper,
                )
                .unwrap(),
            );
        }
    }

    let mut keymap = Keymap::default();
    keymap.add_bindings(bindings);
    keymap
}

fn first_action_for(keymap: &Keymap, keystrokes: &str, contexts: &[&str]) -> &'static str {
    let keystrokes = expand_platform_keystroke(keystrokes);
    let keystrokes = keystrokes
        .split_whitespace()
        .map(|keystroke| Keystroke::parse(keystroke).unwrap())
        .collect::<Vec<_>>();
    let contexts = contexts
        .iter()
        .map(|context| KeyContext::parse(context).unwrap())
        .collect::<Vec<_>>();
    let (bindings, pending) = keymap.bindings_for_input(&keystrokes, &contexts);

    assert!(!pending);
    bindings
        .first()
        .expect("expected matching keybinding")
        .action()
        .name()
}

fn action_names_for(keymap: &Keymap, keystrokes: &str, contexts: &[&str]) -> Vec<String> {
    let keystrokes = expand_platform_keystroke(keystrokes);
    let keystrokes = keystrokes
        .split_whitespace()
        .map(|keystroke| Keystroke::parse(keystroke).unwrap())
        .collect::<Vec<_>>();
    let contexts = contexts
        .iter()
        .map(|context| KeyContext::parse(context).unwrap())
        .collect::<Vec<_>>();

    keymap
        .bindings_for_input(&keystrokes, &contexts)
        .0
        .iter()
        .map(|binding| binding.action().name().to_string())
        .collect()
}

#[test]
fn document_undo_redo_defaults() {
    let undo_keystrokes = vec![expand_platform_keystroke("primary-z")];
    let mut redo_keystrokes = vec![expand_platform_keystroke("primary-shift-z")];
    if !cfg!(target_os = "macos") {
        redo_keystrokes.push("ctrl-y".to_string());
    }

    for preset in [KeymapPreset::Modern, KeymapPreset::Vim] {
        let rows = editable_bindings(preset, &[]);
        let document_binding = |action| {
            rows.iter()
                .find(|row| {
                    row.action == action && row.category == KeyBindingCategory::DocumentView
                })
                .expect("document command binding should exist")
        };

        assert_eq!(document_binding("undo").keystrokes, undo_keystrokes);
        assert_eq!(document_binding("redo").keystrokes, redo_keystrokes);

        let keymap = preset_keymap(preset);
        assert_eq!(
            first_action_for(&keymap, "primary-z", &["DocumentView"]),
            Undo::name_for_type()
        );
        assert_eq!(
            first_action_for(&keymap, "primary-shift-z", &["DocumentView"]),
            Redo::name_for_type()
        );
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(
                first_action_for(&keymap, "ctrl-y", &["DocumentView"]),
                Redo::name_for_type()
            );
        }
    }
}

#[test]
fn page_overview_supports_search_and_result_navigation() {
    for preset in [KeymapPreset::Modern, KeymapPreset::Vim] {
        let keymap = preset_keymap(preset);

        assert_eq!(
            first_action_for(&keymap, "/", &["GridOverview"]),
            Search::name_for_type()
        );

        let (next, previous) = match preset {
            KeymapPreset::Modern => ("f3", "shift-f3"),
            KeymapPreset::Vim => ("n", "shift-n"),
        };
        assert_eq!(
            first_action_for(&keymap, next, &["GridOverview"]),
            NextSearchResult::name_for_type()
        );
        assert_eq!(
            first_action_for(&keymap, previous, &["GridOverview"]),
            PrevSearchResult::name_for_type()
        );

        let (search_next, search_previous) = match preset {
            KeymapPreset::Modern => ("primary-g", "primary-shift-g"),
            KeymapPreset::Vim => ("ctrl-g", "ctrl-shift-g"),
        };
        assert_eq!(
            first_action_for(&keymap, search_next, &["GridOverview", "Search"]),
            NextSearchResult::name_for_type()
        );
        assert_eq!(
            first_action_for(&keymap, search_previous, &["GridOverview", "Search"],),
            PrevSearchResult::name_for_type()
        );
    }

    let modern = preset_keymap(KeymapPreset::Modern);
    assert_eq!(
        first_action_for(&modern, "primary-f", &["GridOverview"]),
        Search::name_for_type()
    );
    assert!(
        action_names_for(&modern, "/", &["GridOverview", "Search"]).is_empty(),
        "page-overview shortcuts must not intercept search input"
    );

    let vim = preset_keymap(KeymapPreset::Vim);
    assert!(
        action_names_for(&vim, "n", &["GridOverview", "Search"]).is_empty(),
        "Vim result navigation must not intercept search input"
    );
    assert!(
        action_names_for(&vim, "n", &["GridOverview", "CommandPalette"]).is_empty(),
        "Vim result navigation must not intercept command-palette input"
    );
}

#[test]
fn vim_ctrl_r_still_dispatches_reload() {
    let keymap = preset_keymap(KeymapPreset::Vim);
    assert_eq!(
        first_action_for(&keymap, "ctrl-r", &["DocumentView"]),
        Reload::name_for_type()
    );
}

#[test]
fn new_tab_navigation_uses_native_and_vim_defaults() {
    let modern = preset_keymap(KeymapPreset::Modern);
    assert_eq!(
        first_action_for(&modern, "down", &["NewTab"]),
        CursorDown::name_for_type()
    );
    assert_eq!(
        first_action_for(&modern, "shift-down", &["NewTab"]),
        ExtendSelectionDown::name_for_type()
    );
    assert_eq!(
        first_action_for(&modern, "enter", &["NewTab"]),
        ActivateFocused::name_for_type()
    );
    assert_eq!(
        first_action_for(&modern, "primary-a", &["NewTab"]),
        NewTabSelectAll::name_for_type()
    );

    let vim = preset_keymap(KeymapPreset::Vim);
    assert_eq!(
        first_action_for(&vim, "j", &["NewTab"]),
        CursorDown::name_for_type()
    );
    assert_eq!(
        first_action_for(&vim, "g g", &["NewTab"]),
        CursorFirst::name_for_type()
    );
    assert_eq!(
        first_action_for(&vim, "shift-g", &["NewTab"]),
        CursorLast::name_for_type()
    );
}
