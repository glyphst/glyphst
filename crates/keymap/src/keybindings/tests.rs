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
        assert_eq!(specs[0].category, KeyBindingCategory::PdfView);
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
        let pdf_binding = |action| {
            rows.iter()
                .find(|row| row.action == action && row.category == KeyBindingCategory::PdfView)
                .expect("PDF command binding should exist")
        };

        assert_eq!(
            pdf_binding("rotate-clockwise").keystrokes,
            vec!["r".to_string()]
        );
        assert_eq!(
            pdf_binding("rotate-counterclockwise").keystrokes,
            vec!["shift-r".to_string()]
        );
        assert_eq!(pdf_binding("reload").keystrokes, vec!["ctrl-r".to_string()]);
    }
}

#[test]
fn categories_hide_raw_predicates() {
    assert_eq!(KeyBindingCategory::PdfView.label(), "Document Reader");
    let predicate = KeyBindingCategory::PdfView.predicate();
    assert!(predicate.starts_with("PdfView && !GridOverview"));
    assert!(predicate.contains("!Search"));
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
            .any(|row| row.category == KeyBindingCategory::PdfView)
    );
    assert!(
        rows.iter()
            .any(|row| row.category == KeyBindingCategory::SearchResults)
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

#[test]
fn pdf_undo_redo_defaults() {
    let undo_keystrokes = vec![expand_platform_keystroke("primary-z")];
    let mut redo_keystrokes = vec![expand_platform_keystroke("primary-shift-z")];
    if !cfg!(target_os = "macos") {
        redo_keystrokes.push("ctrl-y".to_string());
    }

    for preset in [KeymapPreset::Modern, KeymapPreset::Vim] {
        let rows = editable_bindings(preset, &[]);
        let pdf_binding = |action| {
            rows.iter()
                .find(|row| row.action == action && row.category == KeyBindingCategory::PdfView)
                .expect("PDF command binding should exist")
        };

        assert_eq!(pdf_binding("undo").keystrokes, undo_keystrokes);
        assert_eq!(pdf_binding("redo").keystrokes, redo_keystrokes);

        let keymap = preset_keymap(preset);
        assert_eq!(
            first_action_for(&keymap, "primary-z", &["PdfView"]),
            Undo::name_for_type()
        );
        assert_eq!(
            first_action_for(&keymap, "primary-shift-z", &["PdfView"]),
            Redo::name_for_type()
        );
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(
                first_action_for(&keymap, "ctrl-y", &["PdfView"]),
                Redo::name_for_type()
            );
        }
    }
}

#[test]
fn vim_ctrl_r_still_dispatches_reload() {
    let keymap = preset_keymap(KeymapPreset::Vim);
    assert_eq!(
        first_action_for(&keymap, "ctrl-r", &["PdfView"]),
        Reload::name_for_type()
    );
}
