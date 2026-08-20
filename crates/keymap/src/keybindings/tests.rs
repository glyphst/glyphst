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
    assert_eq!(
        KeyBindingCategory::ItemNavigation.label(),
        "Item Navigation"
    );
    let item_navigation = KeyBindingCategory::ItemNavigation.predicate();
    assert!(item_navigation.starts_with("Menu || (NewTab"));
    assert!(item_navigation.contains("!Input"));
    assert!(item_navigation.contains("!NewTabModal"));
    assert_eq!(KeyBindingCategory::DocumentView.label(), "Document Reader");
    let predicate = KeyBindingCategory::DocumentView.predicate();
    assert!(predicate.starts_with("DocumentView && !GridOverview"));
    assert!(predicate.contains("!Search"));
    assert!(predicate.contains("!PathChooser"));

    let predicate = KeyBindingCategory::PageOverview.predicate();
    assert!(predicate.starts_with("GridOverview && !CommandPalette"));
    assert!(predicate.contains("!Search"));
    assert!(predicate.contains("!PathChooser"));
}

#[test]
fn item_navigation_is_editable_and_dispatches_in_menu_and_new_tab_contexts() {
    for preset in [KeymapPreset::Modern, KeymapPreset::Vim] {
        let rows = editable_bindings(preset, &[]);
        let next = rows
            .iter()
            .find(|binding| {
                binding.action == "next-item"
                    && binding.category == KeyBindingCategory::ItemNavigation
            })
            .expect("next-item binding should be editable");
        let previous = rows
            .iter()
            .find(|binding| {
                binding.action == "prev-item"
                    && binding.category == KeyBindingCategory::ItemNavigation
            })
            .expect("prev-item binding should be editable");

        assert_eq!(next.keystrokes, ["down", "ctrl-n"]);
        assert_eq!(next.default_keystrokes, ["down", "ctrl-n"]);
        assert_eq!(previous.keystrokes, ["up", "ctrl-p"]);
        assert_eq!(previous.default_keystrokes, ["up", "ctrl-p"]);

        let keymap = preset_keymap(preset);
        assert_eq!(
            first_action_for(&keymap, "ctrl-n", &["CommandPalette Menu"]),
            menu::SelectNext::name_for_type()
        );
        assert_eq!(
            first_action_for(&keymap, "ctrl-p", &["PathChooser Menu"]),
            menu::SelectPrev::name_for_type()
        );
        assert_eq!(
            first_action_for(&keymap, "ctrl-n", &["NewTab"]),
            menu::SelectNext::name_for_type()
        );
        assert_eq!(
            first_action_for(&keymap, "ctrl-p", &["NewTab"]),
            menu::SelectPrev::name_for_type()
        );
        assert!(action_names_for(&keymap, "ctrl-n", &["NewTab", "Input"]).is_empty());
        assert!(action_names_for(&keymap, "ctrl-p", &["NewTab", "NewTabModal"]).is_empty());
    }
}

#[test]
fn item_navigation_overrides_replace_all_defaults() {
    let overrides = [KeyBindingOverride {
        keymap: KeymapPreset::Modern,
        action: "next-item".to_string(),
        keystrokes: "alt-j".to_string(),
        context: Some(KeyBindingCategory::ItemNavigation.id().to_string()),
    }];

    let next = editable_bindings(KeymapPreset::Modern, &overrides)
        .into_iter()
        .find(|binding| binding.action == "next-item")
        .expect("next-item binding should remain editable");
    assert_eq!(next.keystrokes, ["alt-j"]);
    assert_eq!(next.default_keystrokes, ["down", "ctrl-n"]);
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
fn duplicate_tab_uses_the_workspace_shortcut_in_both_presets() {
    let expected = vec![expand_platform_keystroke("primary-shift-d")];

    for preset in [KeymapPreset::Modern, KeymapPreset::Vim] {
        let binding = editable_bindings(preset, &[])
            .into_iter()
            .find(|binding| {
                binding.action == "duplicate-tab"
                    && binding.category == KeyBindingCategory::Workspace
            })
            .expect("duplicate-tab binding should exist");
        assert_eq!(binding.keystrokes, expected);
        assert_eq!(binding.default_keystrokes, expected);

        let keymap = preset_keymap(preset);
        assert_eq!(
            first_action_for(&keymap, "primary-shift-d", &["Workspace"]),
            DuplicateTab::name_for_type()
        );
    }
}

#[test]
fn zen_mode_is_unbound_by_default_and_can_be_user_bound() {
    for preset in [KeymapPreset::Modern, KeymapPreset::Vim] {
        let binding = editable_bindings(preset, &[])
            .into_iter()
            .find(|binding| {
                binding.action == "toggle-zen-mode"
                    && binding.category == KeyBindingCategory::Workspace
            })
            .expect("Zen mode binding should be editable");
        assert!(binding.keystrokes.is_empty());
        assert!(binding.default_keystrokes.is_empty());

        let overrides = [KeyBindingOverride {
            keymap: preset,
            action: "zen-mode".to_string(),
            keystrokes: "ctrl-alt-z".to_string(),
            context: Some(KeyBindingCategory::Workspace.id().to_string()),
        }];
        let overridden = editable_bindings(preset, &overrides)
            .into_iter()
            .find(|binding| {
                binding.action == "toggle-zen-mode"
                    && binding.category == KeyBindingCategory::Workspace
            })
            .expect("overridden Zen mode binding should remain editable");
        assert_eq!(overridden.keystrokes, vec!["ctrl-alt-z".to_string()]);
        assert!(overridden.default_keystrokes.is_empty());
    }
}

#[test]
fn rendering_debug_is_unbound_by_default_and_can_be_user_bound() {
    for preset in [KeymapPreset::Modern, KeymapPreset::Vim] {
        let binding = editable_bindings(preset, &[])
            .into_iter()
            .find(|binding| {
                binding.action == "toggle-rendering-debug"
                    && binding.category == KeyBindingCategory::Workspace
            })
            .expect("rendering debug should be an editable command");
        assert!(binding.keystrokes.is_empty());
        assert!(binding.default_keystrokes.is_empty());

        let overrides = [KeyBindingOverride {
            keymap: preset,
            action: "render-debug".to_string(),
            keystrokes: "ctrl-alt-r".to_string(),
            context: Some(KeyBindingCategory::Workspace.id().to_string()),
        }];
        let overridden = editable_bindings(preset, &overrides)
            .into_iter()
            .find(|binding| binding.action == "toggle-rendering-debug")
            .expect("overridden rendering debug should remain editable");
        assert_eq!(overridden.keystrokes, ["ctrl-alt-r"]);
    }
}

#[test]
fn nested_escape_binding_wins_before_zen_mode() {
    let mut keymap = Keymap::default();
    keymap.add_bindings([
        KeyBinding::load(
            "escape",
            Box::new(ToggleZenMode),
            Some(Rc::new(
                KeyBindingContextPredicate::parse("ZenMode").unwrap(),
            )),
            false,
            None,
            &DummyKeyboardMapper,
        )
        .unwrap(),
        KeyBinding::load(
            "escape",
            Box::new(ToggleGridOverview),
            Some(Rc::new(
                KeyBindingContextPredicate::parse("GridOverview").unwrap(),
            )),
            false,
            None,
            &DummyKeyboardMapper,
        )
        .unwrap(),
    ]);

    assert_eq!(
        first_action_for(&keymap, "escape", &["Workspace ZenMode", "DocumentView"]),
        ToggleZenMode::name_for_type()
    );
    assert_eq!(
        first_action_for(
            &keymap,
            "escape",
            &["Workspace ZenMode", "DocumentView GridOverview"],
        ),
        ToggleGridOverview::name_for_type()
    );
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
        keymap: preset,
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

#[test]
fn overrides_are_isolated_by_preset() {
    let overrides = vec![
        KeyBindingOverride {
            keymap: KeymapPreset::Modern,
            action: "reload".to_string(),
            keystrokes: "ctrl-1".to_string(),
            context: Some(KeyBindingCategory::DocumentView.id().to_string()),
        },
        KeyBindingOverride {
            keymap: KeymapPreset::Vim,
            action: "reload".to_string(),
            keystrokes: "ctrl-2".to_string(),
            context: Some(KeyBindingCategory::DocumentView.id().to_string()),
        },
    ];

    let reload_keys = |preset| {
        editable_bindings(preset, &overrides)
            .into_iter()
            .find(|binding| {
                binding.action == "reload" && binding.category == KeyBindingCategory::DocumentView
            })
            .unwrap()
            .keystrokes
    };
    assert_eq!(
        reload_keys(KeymapPreset::Modern),
        vec!["ctrl-1".to_string()]
    );
    assert_eq!(reload_keys(KeymapPreset::Vim), vec!["ctrl-2".to_string()]);
}

#[test]
fn setting_and_resetting_overrides_only_changes_the_target_preset() {
    let mut settings = Settings::default();
    let category = KeyBindingCategory::DocumentView;

    set_keybinding_overrides(
        &mut settings,
        KeymapPreset::Modern,
        "reload".to_string(),
        category,
        vec!["ctrl-1".to_string(), "ctrl-2".to_string()],
    );
    set_keybinding_overrides(
        &mut settings,
        KeymapPreset::Vim,
        "reload".to_string(),
        category,
        vec!["ctrl-r".to_string()],
    );
    set_keybinding_overrides(
        &mut settings,
        KeymapPreset::Modern,
        "reload".to_string(),
        category,
        Vec::new(),
    );

    let modern = settings
        .keybinding_overrides
        .iter()
        .filter(|binding| binding.keymap == KeymapPreset::Modern)
        .collect::<Vec<_>>();
    assert_eq!(modern.len(), 1);
    assert!(modern[0].keystrokes.is_empty());
    assert_eq!(
        settings
            .keybinding_overrides
            .iter()
            .filter(|binding| binding.keymap == KeymapPreset::Vim)
            .map(|binding| binding.keystrokes.as_str())
            .collect::<Vec<_>>(),
        vec!["ctrl-r"]
    );

    reset_keybinding_overrides(&mut settings, KeymapPreset::Modern, "reload", category);
    assert!(
        settings
            .keybinding_overrides
            .iter()
            .all(|binding| binding.keymap == KeymapPreset::Vim)
    );
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

fn action_names_and_pending_for(
    keymap: &Keymap,
    keystrokes: &str,
    contexts: &[&str],
) -> (Vec<String>, bool) {
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

    (
        bindings
            .iter()
            .map(|binding| binding.action().name().to_string())
            .collect(),
        pending,
    )
}

#[test]
fn close_tab_defaults_do_not_overlap_the_vim_pane_prefix() {
    let modern = preset_keymap(KeymapPreset::Modern);
    let (modern_actions, modern_pending) =
        action_names_and_pending_for(&modern, "primary-w", &["Workspace"]);
    assert_eq!(
        modern_actions.first().map(String::as_str),
        Some(CloseTab::name_for_type())
    );
    assert!(
        !modern_pending,
        "Modern close-tab should dispatch immediately"
    );

    let vim = preset_keymap(KeymapPreset::Vim);
    let (prefix_actions, prefix_pending) =
        action_names_and_pending_for(&vim, "ctrl-w", &["Workspace"]);
    assert!(
        prefix_actions
            .iter()
            .all(|action| action != CloseTab::name_for_type())
    );
    assert!(prefix_pending, "ctrl-w should only start a Vim chord");

    for chord in ["ctrl-w c", "ctrl-w ctrl-c", "ctrl-w q", "ctrl-w ctrl-q"] {
        let (actions, pending) = action_names_and_pending_for(&vim, chord, &["Workspace"]);
        assert_eq!(
            actions.first().map(String::as_str),
            Some(CloseTab::name_for_type())
        );
        assert!(!pending, "{chord} should dispatch immediately");
    }

    #[cfg(target_os = "macos")]
    {
        let (actions, pending) = action_names_and_pending_for(&vim, "cmd-w", &["Workspace"]);
        assert_eq!(
            actions.first().map(String::as_str),
            Some(CloseTab::name_for_type())
        );
        assert!(!pending, "cmd-w should dispatch immediately on macOS");
    }
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
fn two_column_mode_uses_only_the_vim_d_default() {
    let modern = preset_keymap(KeymapPreset::Modern);
    let vim = preset_keymap(KeymapPreset::Vim);

    assert!(
        action_names_for(&modern, "d", &["DocumentView"])
            .iter()
            .all(|name| name != ToggleTwoColumnMode::name_for_type())
    );
    assert_eq!(
        first_action_for(&vim, "d", &["DocumentView"]),
        ToggleTwoColumnMode::name_for_type()
    );

    let modern_binding = editable_bindings(KeymapPreset::Modern, &[])
        .into_iter()
        .find(|binding| binding.action == "toggle-two-column-mode")
        .unwrap();
    let vim_binding = editable_bindings(KeymapPreset::Vim, &[])
        .into_iter()
        .find(|binding| binding.action == "toggle-two-column-mode")
        .unwrap();
    assert!(modern_binding.keystrokes.is_empty());
    assert_eq!(vim_binding.keystrokes, vec!["d".to_string()]);
}

#[test]
fn new_tab_navigation_uses_native_and_vim_defaults() {
    let modern = preset_keymap(KeymapPreset::Modern);
    assert_eq!(
        first_action_for(&modern, "down", &["NewTab"]),
        CursorDown::name_for_type()
    );
    assert_eq!(
        first_action_for(&modern, "up", &["NewTab"]),
        CursorUp::name_for_type()
    );
    assert_eq!(
        first_action_for(&modern, "ctrl-n", &["NewTab"]),
        menu::SelectNext::name_for_type()
    );
    assert_eq!(
        first_action_for(&modern, "ctrl-p", &["NewTab"]),
        menu::SelectPrev::name_for_type()
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
    assert!(
        action_names_for(&vim, "k", &["NewTab", "Input"]).is_empty(),
        "New Tab navigation must not intercept text input"
    );
    assert!(
        action_names_for(&vim, "k", &["NewTab", "NewTabModal"]).is_empty(),
        "New Tab navigation must not run behind a modal"
    );
}
