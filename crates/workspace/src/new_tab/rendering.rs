//! New-tab layout, library views, and editor overlays.

use super::*;

impl NewTab {
    pub(super) fn destination_label(
        destination: &ReaderNewTabDestination,
        store: &LibraryStore,
    ) -> String {
        match destination {
            ReaderNewTabDestination::Home => "Home".to_string(),
            ReaderNewTabDestination::Library => "Library".to_string(),
            ReaderNewTabDestination::Favorites => "Favorites".to_string(),
            ReaderNewTabDestination::History => "History".to_string(),
            ReaderNewTabDestination::Tags => "Tags".to_string(),
            ReaderNewTabDestination::Trash => "Trash".to_string(),
            ReaderNewTabDestination::Tag(id) => store
                .library_tags()
                .iter()
                .find(|tag| tag.id == *id)
                .map(|tag| format!("#{}", tag.name))
                .unwrap_or_else(|| "Tag".to_string()),
            ReaderNewTabDestination::SavedView(id) => store
                .library_saved_views()
                .iter()
                .find(|view| view.id == *id)
                .map(|view| view.name.clone())
                .unwrap_or_else(|| "Saved View".to_string()),
        }
    }

    pub(super) fn render_destination_button(
        destination: ReaderNewTabDestination,
        label: String,
        icon: IconName,
        selected: bool,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = format!("new-tab-nav-{destination:?}");
        let debug_label = label.clone();
        let mut button = Button::new(id)
            .debug_selector(move || format!("new-tab-nav-{debug_label}"))
            .ghost()
            .selected(selected)
            .icon(icon)
            .label(label);
        if compact {
            button = button.xsmall().compact();
        } else {
            button = button.small().w_full();
        }
        button
            .on_click(cx.listener(move |this, _, window, cx| {
                this.set_destination(destination.clone(), window, cx);
                cx.stop_propagation();
            }))
            .into_any_element()
    }

    pub(super) fn primary_destinations() -> [(ReaderNewTabDestination, &'static str, IconName); 6] {
        [
            (ReaderNewTabDestination::Home, "Home", IconName::AppWindow),
            (
                ReaderNewTabDestination::Library,
                "Library",
                IconName::BookOpen,
            ),
            (
                ReaderNewTabDestination::Favorites,
                "Favorites",
                IconName::Circle,
            ),
            (
                ReaderNewTabDestination::History,
                "History",
                IconName::RotateCcw,
            ),
            (ReaderNewTabDestination::Tags, "Tags", IconName::Link2),
            (ReaderNewTabDestination::Trash, "Trash", IconName::Trash2),
        ]
    }

    pub(super) fn home_destinations() -> [(ReaderNewTabDestination, &'static str, IconName); 3] {
        [
            (
                ReaderNewTabDestination::Library,
                "Library",
                IconName::BookOpen,
            ),
            (
                ReaderNewTabDestination::History,
                "History",
                IconName::RotateCcw,
            ),
            (ReaderNewTabDestination::Trash, "Trash", IconName::Trash2),
        ]
    }

    pub(super) fn render_quick_source_selector(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.session.destination.clone();
        let label = Self::destination_label(&selected, cx.global::<LibraryStore>());
        let mut selector = div().relative().child(
            Button::new("new-tab-source-selector")
                .debug_selector(|| "new-tab-source-selector".to_string())
                .small()
                .ghost()
                .label(label)
                .icon(IconName::ChevronDown)
                .selected(self.nav_menu_open)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.nav_menu_open = !this.nav_menu_open;
                    cx.stop_propagation();
                    cx.notify();
                })),
        );
        if self.nav_menu_open {
            let menu = div()
                .id("new-tab-source-menu")
                .debug_selector(|| "new-tab-source-menu".to_string())
                .absolute()
                .top(px(30.0))
                .left_0()
                .w(px(180.0))
                .p_1()
                .flex()
                .flex_col()
                .gap_1()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_lg()
                .occlude()
                .children([
                    Self::render_destination_button(
                        ReaderNewTabDestination::History,
                        "History".to_string(),
                        IconName::RotateCcw,
                        selected == ReaderNewTabDestination::History,
                        false,
                        cx,
                    ),
                    Self::render_destination_button(
                        ReaderNewTabDestination::Library,
                        "Library".to_string(),
                        IconName::BookOpen,
                        selected == ReaderNewTabDestination::Library,
                        false,
                        cx,
                    ),
                ]);
            selector = selector.child(deferred(menu));
        }
        selector.into_any_element()
    }

    pub(super) fn render_library_filter_menu(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.session.destination.clone();
        let tags = cx
            .global::<LibraryStore>()
            .library_tags()
            .iter()
            .map(|tag| (tag.id, tag.name.clone()))
            .collect::<Vec<_>>();
        let saved_views = cx
            .global::<LibraryStore>()
            .library_saved_views()
            .iter()
            .map(|view| (view.id, view.name.clone()))
            .collect::<Vec<_>>();
        let active_filter = !matches!(selected, ReaderNewTabDestination::Library);
        let mut control = div().relative().child(
            Button::new("home-library-filter-menu")
                .debug_selector(|| "home-library-filter-menu".to_string())
                .xsmall()
                .outline()
                .icon(IconName::Link2)
                .label("Filters")
                .selected(active_filter || self.nav_menu_open)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.nav_menu_open = !this.nav_menu_open;
                    cx.stop_propagation();
                    cx.notify();
                })),
        );
        if self.nav_menu_open {
            let mut menu = div()
                .id("home-library-filter-popover")
                .debug_selector(|| "home-library-filter-popover".to_string())
                .absolute()
                .top(px(28.0))
                .right_0()
                .w(px(240.0))
                .max_h(px(420.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_lg()
                .child(Self::render_destination_button(
                    ReaderNewTabDestination::Library,
                    "All Library Items".to_string(),
                    IconName::BookOpen,
                    selected == ReaderNewTabDestination::Library,
                    false,
                    cx,
                ))
                .child(Self::render_destination_button(
                    ReaderNewTabDestination::Favorites,
                    "Favorites".to_string(),
                    IconName::Circle,
                    selected == ReaderNewTabDestination::Favorites,
                    false,
                    cx,
                ));
            if !saved_views.is_empty() {
                menu = menu.child(
                    div()
                        .mt_2()
                        .px_2()
                        .text_size(ui_rems(10.0))
                        .text_color(theme.muted_foreground)
                        .child("SAVED VIEWS"),
                );
                for (id, name) in saved_views {
                    menu = menu.child(Self::render_destination_button(
                        ReaderNewTabDestination::SavedView(id),
                        name,
                        IconName::Table2,
                        selected == ReaderNewTabDestination::SavedView(id),
                        false,
                        cx,
                    ));
                }
            }
            menu = menu.child(Self::render_destination_button(
                ReaderNewTabDestination::Tags,
                "Manage Tags".to_string(),
                IconName::Pencil,
                selected == ReaderNewTabDestination::Tags,
                false,
                cx,
            ));
            if !tags.is_empty() {
                menu = menu.child(
                    div()
                        .mt_2()
                        .px_2()
                        .text_size(ui_rems(10.0))
                        .text_color(theme.muted_foreground)
                        .child("TAGS"),
                );
                for (id, name) in tags {
                    menu = menu.child(Self::render_destination_button(
                        ReaderNewTabDestination::Tag(id),
                        format!("#{name}"),
                        IconName::Link2,
                        selected == ReaderNewTabDestination::Tag(id),
                        false,
                        cx,
                    ));
                }
            }
            control = control.child(menu);
        }
        control.into_any_element()
    }

    pub(super) fn render_navigation(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.session.destination.clone();
        if self.host == LibraryBrowserHost::Home {
            let destinations = Self::home_destinations();
            let mut navigation = slim_navigation_rail("home-navigation-rail", theme);
            for (destination, label, icon) in destinations.iter().take(2) {
                let destination = destination.clone();
                navigation = navigation.child(
                    slim_navigation_button(
                        format!("home-nav-{destination:?}"),
                        *icon,
                        selected == destination,
                        *label,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_destination(destination.clone(), window, cx);
                        cx.stop_propagation();
                    })),
                );
            }
            let (destination, label, icon) = &destinations[2];
            let destination = destination.clone();
            return navigation
                .child(div().flex_1())
                .child(
                    slim_navigation_button(
                        format!("home-nav-{destination:?}"),
                        *icon,
                        selected == destination,
                        *label,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_destination(destination.clone(), window, cx);
                        cx.stop_propagation();
                    })),
                )
                .into_any_element();
        }
        let store = cx.global::<LibraryStore>();
        let tag_destinations = store
            .library_tags()
            .iter()
            .map(|tag| {
                (
                    ReaderNewTabDestination::Tag(tag.id),
                    format!("#{}", tag.name),
                )
            })
            .collect::<Vec<_>>();
        let saved_destinations = store
            .library_saved_views()
            .iter()
            .map(|view| {
                (
                    ReaderNewTabDestination::SavedView(view.id),
                    view.name.clone(),
                )
            })
            .collect::<Vec<_>>();
        let current_label = Self::destination_label(&selected, store);

        if self.layout_class == NewTabLayoutClass::Expanded {
            let mut navigation = div()
                .id("new-tab-navigation-rail")
                .debug_selector(|| "new-tab-navigation-rail".to_string())
                .w(px(NEW_TAB_NAV_WIDTH))
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .border_r(px(1.0))
                .border_color(theme.border)
                .bg(theme.sidebar);
            for (destination, label, icon) in Self::primary_destinations() {
                navigation = navigation.child(Self::render_destination_button(
                    destination.clone(),
                    label.to_string(),
                    icon,
                    selected == destination,
                    false,
                    cx,
                ));
            }
            if !saved_destinations.is_empty() {
                navigation = navigation.child(
                    div()
                        .mt_2()
                        .px_2()
                        .text_size(ui_rems(10.0))
                        .text_color(theme.muted_foreground)
                        .child("SAVED VIEWS"),
                );
                for (destination, label) in saved_destinations {
                    navigation = navigation.child(Self::render_destination_button(
                        destination.clone(),
                        label,
                        IconName::Table2,
                        selected == destination,
                        false,
                        cx,
                    ));
                }
            }
            if !tag_destinations.is_empty() {
                navigation = navigation.child(
                    div()
                        .mt_2()
                        .px_2()
                        .text_size(ui_rems(10.0))
                        .text_color(theme.muted_foreground)
                        .child("TAGS"),
                );
                for (destination, label) in tag_destinations.into_iter().take(12) {
                    navigation = navigation.child(Self::render_destination_button(
                        destination.clone(),
                        label,
                        IconName::Link2,
                        selected == destination,
                        false,
                        cx,
                    ));
                }
            }
            return navigation.into_any_element();
        }

        let mut bar = div()
            .id("new-tab-navigation-top")
            .debug_selector(|| "new-tab-navigation-top".to_string())
            .relative()
            .h(px(40.0))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .border_b(px(1.0))
            .border_color(theme.border)
            .bg(theme.sidebar);
        if self.layout_class == NewTabLayoutClass::Narrow {
            bar = bar
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(ui_rems(13.0))
                        .child(current_label),
                )
                .child(
                    Button::new("new-tab-nav-menu")
                        .xsmall()
                        .compact()
                        .ghost()
                        .icon(IconName::Menu)
                        .label("Browse")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.nav_menu_open = !this.nav_menu_open;
                            cx.stop_propagation();
                            cx.notify();
                        })),
                );
        } else {
            for (destination, label, icon) in Self::primary_destinations() {
                bar = bar.child(Self::render_destination_button(
                    destination.clone(),
                    label.to_string(),
                    icon,
                    selected == destination,
                    true,
                    cx,
                ));
            }
            if !saved_destinations.is_empty() || !tag_destinations.is_empty() {
                bar = bar.child(
                    Button::new("new-tab-nav-more")
                        .xsmall()
                        .compact()
                        .ghost()
                        .icon(IconName::Menu)
                        .label("More")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.nav_menu_open = !this.nav_menu_open;
                            cx.stop_propagation();
                            cx.notify();
                        })),
                );
            }
        }
        if self.nav_menu_open {
            let mut menu = div()
                .id("new-tab-nav-overflow")
                .debug_selector(|| "new-tab-nav-overflow".to_string())
                .absolute()
                .top(px(38.0))
                .right(px(8.0))
                .w(px(220.0))
                .max_h(px(420.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_lg();
            for (destination, label, icon) in Self::primary_destinations() {
                menu = menu.child(Self::render_destination_button(
                    destination.clone(),
                    label.to_string(),
                    icon,
                    selected == destination,
                    false,
                    cx,
                ));
            }
            for (destination, label) in saved_destinations {
                menu = menu.child(Self::render_destination_button(
                    destination.clone(),
                    label,
                    IconName::Table2,
                    selected == destination,
                    false,
                    cx,
                ));
            }
            for (destination, label) in tag_destinations {
                menu = menu.child(Self::render_destination_button(
                    destination.clone(),
                    label,
                    IconName::Link2,
                    selected == destination,
                    false,
                    cx,
                ));
            }
            bar = bar.child(menu);
        }
        bar.into_any_element()
    }

    pub(super) fn tag_result_counts(&self, cx: &Context<Self>) -> (i64, i64) {
        let query = self.search_input.read(cx).value().trim().to_lowercase();
        let tags = cx.global::<LibraryStore>().library_tags();
        let total = tags.len() as i64;
        let visible = tags
            .iter()
            .filter(|tag| query.is_empty() || tag.name.to_lowercase().contains(&query))
            .count() as i64;
        (visible, total)
    }

    pub(super) fn render_tags_content(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let query = self.search_input.read(cx).value().trim().to_lowercase();
        let tags = cx
            .global::<LibraryStore>()
            .library_tags()
            .iter()
            .filter(|tag| query.is_empty() || tag.name.to_lowercase().contains(&query))
            .cloned()
            .collect::<Vec<_>>();
        let relations = cx.global::<LibraryStore>().library_item_tags().to_vec();
        if tags.is_empty() {
            return div()
                .id("new-tab-tags-content")
                .debug_selector(|| "new-tab-tags-content".to_string())
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(self.render_empty_state(self.empty_state(!query.is_empty()), theme, cx))
                .into_any_element();
        }
        div()
            .id("new-tab-tags-content")
            .debug_selector(|| "new-tab-tags-content".to_string())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .children(tags.into_iter().map(|tag| {
                let count = relations
                    .iter()
                    .filter(|relation| relation.tag_id == tag.id)
                    .count();
                let destination = ReaderNewTabDestination::Tag(tag.id);
                let tag_id = tag.id;
                let tag_name = tag.name.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new(("new-tab-tag", tag.id.0 as usize))
                            .small()
                            .outline()
                            .flex_1()
                            .label(format!("#{}  ({count})", tag.name))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_destination(destination.clone(), window, cx);
                                cx.stop_propagation();
                            })),
                    )
                    .child(
                        Button::new(("new-tab-tag-edit", tag_id.0 as usize))
                            .xsmall()
                            .ghost()
                            .icon(IconName::Pencil)
                            .tooltip("Rename, merge, or delete tag")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_tag_editor(tag_id, tag_name.clone(), window, cx);
                                cx.stop_propagation();
                            })),
                    )
            }))
            .into_any_element()
    }

    pub(super) fn render_inspector(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let entry = self.selected_entry()?.clone();
        let item = entry
            .item_id
            .and_then(|id| cx.global::<LibraryStore>().library_item(id).cloned());
        let item_tags = item
            .as_ref()
            .map(|item| {
                cx.global::<LibraryStore>()
                    .tags_for_item(item.id)
                    .into_iter()
                    .map(|tag| (tag.id, tag.name.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let (assigned_collection_id, collection_options) = item
            .as_ref()
            .map(|item| {
                let store = cx.global::<LibraryStore>();
                let assigned = store.item_parent(item.id);
                let mut options = store
                    .library_collections()
                    .iter()
                    .map(|collection| {
                        (
                            collection.id,
                            store.collection_path_label(collection.id),
                            store
                                .collection_ancestors(collection.id)
                                .len()
                                .saturating_sub(1),
                        )
                    })
                    .collect::<Vec<_>>();
                options.sort_by(|left, right| left.1.cmp(&right.1));
                (assigned, options)
            })
            .unwrap_or_default();
        let mut inspector = div()
            .id("new-tab-item-inspector")
            .debug_selector(|| "new-tab-item-inspector".to_string())
            .w(px(NEW_TAB_INSPECTOR_WIDTH))
            .h_full()
            .flex_none()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .border_l(px(1.0))
            .border_color(theme.border)
            .bg(theme.sidebar)
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .text_size(ui_rems(15.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(entry.file_name.clone()),
                    )
                    .child(
                        Button::new("new-tab-inspector-close")
                            .xsmall()
                            .ghost()
                            .icon(IconName::X)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.inspector_open = false;
                                cx.stop_propagation();
                                cx.notify();
                            })),
                    ),
            );
        if let Some(item) = item {
            let creators = item
                .creators
                .iter()
                .map(|creator| creator.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let status = match item.status {
                ReadingStatus::ToRead => "To Read",
                ReadingStatus::Reading => "Reading",
                ReadingStatus::Read => "Read",
            };
            inspector = inspector
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(if creators.is_empty() {
                            "Unknown creator".to_string()
                        } else {
                            creators
                        }),
                )
                .when_some(item.year, |this, year| this.child(format!("Year: {year}")))
                .when(!item.publication.is_empty(), |this| {
                    this.child(format!("Publication: {}", item.publication))
                })
                .when(!item_tags.is_empty(), |this| {
                    this.child(div().flex().flex_wrap().gap_1().children(
                        item_tags.clone().into_iter().map(|(tag_id, name)| {
                            Button::new(("new-tab-inspector-tag", tag_id.0 as usize))
                                .xsmall()
                                .outline()
                                .label(format!("#{name} ×"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_tag_from_selected(tag_id, cx);
                                    cx.stop_propagation();
                                }))
                        }),
                    ))
                })
                .when(entry.missing, |this| {
                    this.child(
                        div()
                            .text_color(theme.danger)
                            .child("Linked file is missing. Use Locate/Relink from item actions."),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .child(
                            Button::new("new-tab-inspector-favorite")
                                .xsmall()
                                .outline()
                                .label(if item.favorite {
                                    "Unfavorite"
                                } else {
                                    "Favorite"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_selected_favorite(cx);
                                    cx.stop_propagation();
                                })),
                        )
                        .child(
                            Button::new("new-tab-inspector-status")
                                .xsmall()
                                .outline()
                                .label(status)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cycle_selected_status(cx);
                                    cx.stop_propagation();
                                })),
                        )
                        .child(
                            Button::new("new-tab-inspector-edit-metadata")
                                .xsmall()
                                .outline()
                                .icon(IconName::Pencil)
                                .label("Edit Metadata")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_metadata_editor(window, cx);
                                    cx.stop_propagation();
                                })),
                        )
                        .child(
                            Button::new("new-tab-inspector-locate")
                                .xsmall()
                                .outline()
                                .icon(IconName::LocateFixed)
                                .label(if entry.missing { "Locate" } else { "Relink" })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.locate_selected_attachment(window, cx);
                                    cx.stop_propagation();
                                })),
                        ),
                )
                .child(
                    div()
                        .mt_1()
                        .text_size(ui_rems(11.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .child("FOLDER"),
                )
                .child(
                    div()
                        .id("new-tab-inspector-library-root")
                        .h(px(30.0))
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(cx.theme().radius)
                        .hover(move |style| style.bg(theme.list_hover))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.set_selected_collection(None, cx);
                                cx.stop_propagation();
                            }),
                        )
                        .child(
                            Checkbox::new("new-tab-inspector-library-root-check")
                                .xsmall()
                                .checked(assigned_collection_id.is_none())
                                .tab_stop(false),
                        )
                        .child("Library root"),
                )
                .children(collection_options.into_iter().map(|(id, path, depth)| {
                    let checked = assigned_collection_id == Some(id);
                    div()
                        .id(("new-tab-inspector-folder", id.0 as usize))
                        .debug_selector(|| "new-tab-inspector-folder".to_string())
                        .h(px(30.0))
                        .w_full()
                        .pl(px(depth as f32 * 12.0))
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(cx.theme().radius)
                        .hover(move |style| style.bg(theme.list_hover))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.set_selected_collection(Some(id), cx);
                                cx.stop_propagation();
                            }),
                        )
                        .child(
                            Checkbox::new(("new-tab-inspector-folder-check", id.0 as usize))
                                .xsmall()
                                .checked(checked)
                                .tab_stop(false),
                        )
                        .child(div().min_w_0().truncate().child(path))
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h(px(30.0))
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.background)
                                .child(
                                    Input::new(&self.editors.tag_input)
                                        .small()
                                        .appearance(false),
                                ),
                        )
                        .child(
                            Button::new("new-tab-inspector-add-tag")
                                .xsmall()
                                .primary()
                                .icon(IconName::Plus)
                                .label("Tag")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_tag_to_selected(window, cx);
                                    cx.stop_propagation();
                                })),
                        ),
                )
                .child(
                    div()
                        .mt_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child("Research note")
                        .child(
                            Button::new("new-tab-note-expand")
                                .xsmall()
                                .ghost()
                                .icon(IconName::ArrowUpRight)
                                .label("Expand")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.editors.note_expanded = true;
                                    let input = this.editors.work_note_input.clone();
                                    window.defer(cx, move |window, cx| {
                                        input.update(cx, |input, cx| input.focus(window, cx));
                                    });
                                    cx.stop_propagation();
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .min_h(px(180.0))
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.background)
                        .child(Textarea::new(&self.editors.work_note_input).appearance(false)),
                )
                .when(item.trashed_at.is_none(), |this| {
                    this.child(
                        Button::new("new-tab-inspector-trash")
                            .small()
                            .danger()
                            .icon(IconName::Trash2)
                            .label("Move to Trash")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.trash_selected_item(cx);
                                cx.stop_propagation();
                            })),
                    )
                });
        } else {
            inspector = inspector
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child("This file is in History but not in your library."),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_size(ui_rems(11.0))
                        .child(format!("Path: {}", entry.path.display()))
                        .child(format!("Format: {}", entry.format_size))
                        .child(format!("Progress: {}", entry.progress))
                        .child(format!("Last opened: {}", entry.last_opened)),
                )
                .child(
                    Button::new("new-tab-add-selected-to-library")
                        .small()
                        .primary()
                        .icon(IconName::Plus)
                        .label("Add to Library")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.add_selected_history_item(cx);
                            cx.stop_propagation();
                        })),
                );
        }
        Some(inspector.into_any_element())
    }

    pub(super) fn cancel_topmost_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let cancelled = if self.editors.folder.is_some() {
            self.editors.folder = None;
            true
        } else if self.editors.tag_id.is_some() {
            self.editors.tag_id = None;
            true
        } else if self.editors.saved_view_open {
            self.editors.saved_view_open = false;
            true
        } else if self.editors.metadata_open {
            self.editors.metadata_open = false;
            true
        } else if self.editors.note_expanded {
            self.editors.note_expanded = false;
            true
        } else {
            false
        };
        if cancelled {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
        cancelled
    }

    pub(super) fn render_note_overlay(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("new-tab-note-overlay")
            .debug_selector(|| "new-tab-note-overlay".to_string())
            .key_context("NewTabModal Dialog")
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .p_6()
            .bg(theme.overlay)
            .child(
                div()
                    .id("new-tab-metadata-card")
                    .size_full()
                    .max_w(px(900.0))
                    .mx_auto()
                    .flex()
                    .flex_col()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .shadow_lg()
                    .child(
                        div()
                            .h(px(44.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_3()
                            .border_b(px(1.0))
                            .border_color(theme.border)
                            .child("Research note (Markdown)")
                            .child(
                                Button::new("new-tab-note-overlay-close")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::X)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.cancel_topmost_editor(window, cx);
                                        cx.stop_propagation();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .p_3()
                            .child(Textarea::new(&self.editors.work_note_input).appearance(false)),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_metadata_overlay(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let field = |label: &'static str, input: &Entity<InputState>| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(ui_rems(11.0))
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .min_h(px(32.0))
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.background)
                        .child(Input::new(input).small().appearance(false)),
                )
        };
        div()
            .id("new-tab-metadata-overlay")
            .debug_selector(|| "new-tab-metadata-overlay".to_string())
            .key_context("NewTabModal Dialog")
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .p_6()
            .bg(theme.overlay)
            .child(
                div()
                    .id("new-tab-metadata-card")
                    .size_full()
                    .max_w(px(760.0))
                    .mx_auto()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_4()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_lg()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(ui_rems(16.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Edit academic metadata"),
                            )
                            .child(
                                Button::new("new-tab-metadata-close")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::X)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.cancel_topmost_editor(window, cx);
                                        cx.stop_propagation();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(ui_rems(11.0))
                                    .text_color(theme.muted_foreground)
                                    .child("Item type"),
                            )
                            .child(
                                div().flex().gap_1().children(
                                    [
                                        (LibraryItemKind::Paper, "Paper"),
                                        (LibraryItemKind::Book, "Book"),
                                        (LibraryItemKind::Other, "Other"),
                                    ]
                                    .into_iter()
                                    .enumerate()
                                    .map(
                                        |(index, (kind, label))| {
                                            Button::new(("new-tab-metadata-kind", index))
                                                .small()
                                                .outline()
                                                .selected(self.editors.metadata_kind == kind)
                                                .label(label)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.editors.metadata_kind = kind;
                                                    cx.stop_propagation();
                                                    cx.notify();
                                                }))
                                        },
                                    ),
                                ),
                            ),
                    )
                    .child(field("Title", &self.editors.metadata_title_input))
                    .child(field("Creators", &self.editors.metadata_creators_input))
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .child(field("Year", &self.editors.metadata_year_input)),
                            )
                            .child(
                                div().flex_1().child(field(
                                    "Language",
                                    &self.editors.metadata_language_input,
                                )),
                            ),
                    )
                    .child(field(
                        "Publication or venue",
                        &self.editors.metadata_publication_input,
                    ))
                    .child(field("Publisher", &self.editors.metadata_publisher_input))
                    .child(field("DOI", &self.editors.metadata_doi_input))
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .child(field("ISBN", &self.editors.metadata_isbn_input)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .child(field("arXiv", &self.editors.metadata_arxiv_input)),
                            ),
                    )
                    .child(field("Subject", &self.editors.metadata_subject_input))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(ui_rems(11.0))
                                    .text_color(theme.muted_foreground)
                                    .child("Abstract or summary"),
                            )
                            .child(
                                div()
                                    .min_h(px(128.0))
                                    .rounded(cx.theme().radius)
                                    .border_1()
                                    .border_color(theme.border)
                                    .bg(theme.background)
                                    .child(
                                        Textarea::new(&self.editors.metadata_abstract_input)
                                            .appearance(false),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("new-tab-metadata-cancel")
                                    .small()
                                    .outline()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.cancel_topmost_editor(window, cx);
                                        cx.stop_propagation();
                                    })),
                            )
                            .child(
                                Button::new("new-tab-metadata-save")
                                    .small()
                                    .primary()
                                    .label("Save")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save_metadata_editor(cx);
                                        cx.stop_propagation();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_saved_view_overlay(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("new-tab-saved-view-overlay")
            .debug_selector(|| "new-tab-saved-view-overlay".to_string())
            .key_context("NewTabModal Dialog")
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .p_6()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.overlay)
            .child(
                div()
                    .w_full()
                    .max_w(px(420.0))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_lg()
                    .child(
                        div()
                            .text_size(ui_rems(16.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Save current library view"),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child("The current destination, search, sort, and layout will be reusable from the navigation panel."),
                    )
                    .child(
                        div()
                            .h(px(34.0))
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .child(
                                Input::new(&self.editors.saved_view_name_input)
                                    .small()
                                    .appearance(false),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("new-tab-saved-view-cancel")
                                    .small()
                                    .outline()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.cancel_topmost_editor(window, cx);
                                        cx.stop_propagation();
                                    })),
                            )
                            .child(
                                Button::new("new-tab-saved-view-save")
                                    .small()
                                    .primary()
                                    .label("Save View")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_current_view(window, cx);
                                        cx.stop_propagation();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_tag_editor_overlay(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("new-tab-tag-editor-overlay")
            .debug_selector(|| "new-tab-tag-editor-overlay".to_string())
            .key_context("NewTabModal Dialog")
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .p_6()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.overlay)
            .child(
                div()
                    .w_full()
                    .max_w(px(420.0))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_lg()
                    .child(
                        div()
                            .text_size(ui_rems(16.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Edit tag"),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child("Renaming to an existing tag merges their assignments."),
                    )
                    .child(
                        div()
                            .h(px(34.0))
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .child(
                                Input::new(&self.editors.tag_name_input)
                                    .small()
                                    .appearance(false),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                Button::new("new-tab-tag-editor-delete")
                                    .small()
                                    .danger()
                                    .icon(IconName::Trash2)
                                    .label("Delete Tag")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.delete_edited_tag(window, cx);
                                        cx.stop_propagation();
                                    })),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        Button::new("new-tab-tag-editor-cancel")
                                            .small()
                                            .outline()
                                            .label("Cancel")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.cancel_topmost_editor(window, cx);
                                                cx.stop_propagation();
                                            })),
                                    )
                                    .child(
                                        Button::new("new-tab-tag-editor-save")
                                            .small()
                                            .primary()
                                            .label("Save")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.save_tag_editor(cx);
                                                cx.stop_propagation();
                                            })),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_toolbar_button(
        label: String,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new(label.clone())
            .small()
            .outline()
            .label(label)
            .on_click(cx.listener(move |this, _, window, cx| {
                handler(this, window, cx);
                cx.stop_propagation();
            }))
    }

    pub(super) fn render_destination_heading(
        &self,
        label: String,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_root_target = self.host == LibraryBrowserHost::Home && self.is_library_tree();
        let highlighted = self.tree.drop_target == Some(LibraryDropTarget::Root);
        let mut heading = div()
            .id("library-destination-heading")
            .px_1()
            .rounded(px(3.0))
            .bg(if highlighted {
                theme.drop_target
            } else {
                transparent_black()
            })
            .child(label);
        if is_root_target {
            heading = heading
                .on_drag_move::<DraggedLibrarySelection>(cx.listener(
                    |this, event: &DragMoveEvent<DraggedLibrarySelection>, _, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        let dragged = event.drag(cx).clone();
                        this.set_library_root_drag_target(&dragged, active, cx);
                    },
                ))
                .on_drop(
                    cx.listener(|this, dragged: &DraggedLibrarySelection, _, cx| {
                        this.drop_library_selection(dragged, None, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_drag_move::<ExternalPaths>(cx.listener(
                    |this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        this.set_external_library_root_drag_target(active, cx);
                    },
                ))
                .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                    this.drop_external_library_paths(paths, None, cx);
                    cx.stop_propagation();
                }));
            let view = cx.entity();
            return heading
                .context_menu(move |menu, _, _| {
                    Self::collection_context_menu(menu, None, view.clone())
                })
                .into_any_element();
        }
        heading.into_any_element()
    }

    pub(super) fn render_view_button(
        mode: NewTabViewMode,
        current_mode: NewTabViewMode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (id, icon, label) = match mode {
            NewTabViewMode::Grid => (
                "new-tab-view-grid",
                IconName::LayoutGrid,
                crate::fl!("new-tab-view-grid"),
            ),
            NewTabViewMode::Grouped => (
                "new-tab-view-grouped",
                IconName::LayoutList,
                crate::fl!("new-tab-view-grouped"),
            ),
            NewTabViewMode::Table => (
                "new-tab-view-tree",
                IconName::ListTree,
                crate::fl!("new-tab-view-tree"),
            ),
        };

        Button::new(id)
            .debug_selector(move || id.to_string())
            .xsmall()
            .compact()
            .ghost()
            .selected(mode == current_mode)
            .icon(icon)
            .tooltip(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_view_mode(mode, cx);
                cx.stop_propagation();
            }))
    }

    pub(super) fn visible_child_collections(
        &self,
        cx: &Context<Self>,
    ) -> Vec<(LibraryCollectionId, SharedString)> {
        if self.session.destination != ReaderNewTabDestination::Library
            || !self.search_input.read(cx).value().trim().is_empty()
        {
            return Vec::new();
        }
        cx.global::<LibraryStore>()
            .child_collections(self.tree.current_collection)
            .into_iter()
            .map(|collection| (collection.id, collection.name.clone().into()))
            .collect()
    }

    pub(super) fn collection_context_menu(
        menu: PopupMenu,
        id: Option<LibraryCollectionId>,
        view: Entity<Self>,
    ) -> PopupMenu {
        let new_folder_view = view.clone();
        let mut menu = menu.item(
            PopupMenuItem::new(if id.is_some() {
                "New Subfolder"
            } else {
                "New Folder"
            })
            .icon(IconName::Plus)
            .on_click(move |_, window, cx| {
                new_folder_view.update(cx, |this, cx| {
                    this.open_new_folder_editor(id, window, cx);
                });
            }),
        );
        if let Some(id) = id {
            let rename_view = view.clone();
            let delete_view = view;
            menu = menu
                .item(
                    PopupMenuItem::new("Rename Folder")
                        .icon(IconName::Pencil)
                        .on_click(move |_, window, cx| {
                            rename_view.update(cx, |this, cx| {
                                this.open_rename_folder_editor(id, window, cx);
                            });
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Delete Folder")
                        .icon(IconName::Trash2)
                        .on_click(move |_, window, cx| {
                            delete_view.update(cx, |this, cx| {
                                this.request_delete_folder(id, window, cx);
                            });
                        }),
                );
        }
        menu
    }

    pub(super) fn render_collection_item(
        &self,
        id: LibraryCollectionId,
        name: SharedString,
        grouped: bool,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let management = self.host == LibraryBrowserHost::Home;
        let enter_id = id;
        let drag_name = name.clone();
        let mut item = div()
            .id(("library-folder-item", id.0 as usize))
            .debug_selector(|| "library-folder-item".to_string())
            .relative()
            .flex_none()
            .max_w_full()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .when(grouped, |item| item.h(px(40.0)).w_auto())
            .when(!grouped, |item| {
                item.w(px(CARD_WIDTH))
                    .h(px(92.0))
                    .flex_col()
                    .justify_center()
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.enter_collection(enter_id, cx);
                cx.stop_propagation();
            }));
        if management {
            let dragged = DraggedLibrarySelection {
                active: LibraryNodeId::Collection(id),
                marked: Arc::from([LibraryNodeId::Collection(id)]),
                label: drag_name,
            };
            item = item
                .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                .drag_over::<DraggedLibrarySelection>(move |style, _, _, _| {
                    style.border_color(theme.primary).bg(theme.list_hover)
                })
                .on_drop(
                    cx.listener(move |this, dragged: &DraggedLibrarySelection, _, cx| {
                        this.drop_library_selection(dragged, Some(id), cx);
                    }),
                );
        }

        let label = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Icon::new(IconName::FolderOpen)
                    .size(ui_rems(if grouped { 16.0 } else { 22.0 }))
                    .text_color(theme.muted_foreground),
            )
            .child(div().min_w_0().truncate().child(name));
        let item = item.child(label);
        if management {
            let view = cx.entity();
            return item
                .context_menu(move |menu, _, _| {
                    Self::collection_context_menu(menu, Some(id), view.clone())
                })
                .into_any_element();
        }
        item.into_any_element()
    }

    pub(super) fn render_collection_breadcrumb(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.session.destination != ReaderNewTabDestination::Library
            || !self.search_input.read(cx).value().trim().is_empty()
            || self.session.view_mode == LibraryViewMode::Table
        {
            return None;
        }
        let mut crumbs = vec![(None, "Library".to_string())];
        if let Some(id) = self.tree.current_collection {
            crumbs.extend(
                cx.global::<LibraryStore>()
                    .collection_ancestors(id)
                    .into_iter()
                    .map(|collection| (Some(collection.id), collection.name.clone())),
            );
        }
        let management = self.host == LibraryBrowserHost::Home;
        let mut breadcrumb = div()
            .id("library-folder-breadcrumb")
            .debug_selector(|| "library-folder-breadcrumb".to_string())
            .h(px(34.0))
            .flex_none()
            .w_full()
            .flex()
            .items_center()
            .gap_0p5()
            .px_4()
            .border_b(px(1.0))
            .border_color(theme.border);
        for (ix, (id, label)) in crumbs.into_iter().enumerate() {
            if ix > 0 {
                breadcrumb = breadcrumb.child(
                    Icon::new(IconName::ChevronRight)
                        .size(ui_rems(12.0))
                        .text_color(theme.muted_foreground),
                );
            }
            let mut crumb = Button::new(("library-folder-crumb", ix))
                .xsmall()
                .ghost()
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.enter_collection_parent(id, cx);
                    cx.stop_propagation();
                }));
            if management {
                crumb = crumb
                    .drag_over::<DraggedLibrarySelection>(move |style, _, _, _| {
                        style.bg(theme.list_hover)
                    })
                    .on_drop(
                        cx.listener(move |this, dragged: &DraggedLibrarySelection, _, cx| {
                            this.drop_library_selection(dragged, id, cx);
                        }),
                    );
            }
            breadcrumb = breadcrumb.child(crumb);
        }
        Some(breadcrumb.into_any_element())
    }

    pub(super) fn render_folder_editor_overlay(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let editor = self.editors.folder?;
        let title = if editor.collection_id.is_some() {
            "Rename Folder"
        } else {
            "New Folder"
        };
        Some(
            div()
                .id("library-folder-editor-overlay")
                .debug_selector(|| "library-folder-editor-overlay".to_string())
                .key_context("NewTabModal Dialog")
                .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(transparent_black().opacity(0.35))
                .child(
                    div()
                        .w(px(360.0))
                        .max_w_full()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .rounded(cx.theme().radius_lg)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .shadow_lg()
                        .child(div().text_size(ui_rems(14.0)).child(title))
                        .child(Input::new(&self.editors.folder_name_input).small())
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("library-folder-editor-cancel")
                                        .small()
                                        .outline()
                                        .label("Cancel")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.cancel_topmost_editor(window, cx);
                                            cx.stop_propagation();
                                        })),
                                )
                                .child(
                                    Button::new("library-folder-editor-save")
                                        .small()
                                        .primary()
                                        .label("Save")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.save_folder_editor(cx);
                                            cx.stop_propagation();
                                        })),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn item_state(&self, item_ix: usize) -> NewTabItemState {
        let key = self.item_key(item_ix);
        let selected = key.as_ref().is_some_and(|key| match key {
            NewTabItemKey::OpenFile | NewTabItemKey::Collection(_) => false,
            NewTabItemKey::Document(path) => self.selection.contains(path),
        });
        NewTabItemState {
            item_ix,
            focused: key.as_ref() == Some(&self.focused_item),
            selecting: self.selecting,
            selected,
        }
    }

    pub(super) fn drag_ids_for_entry(&self, entry: &NewTabEntry) -> Vec<DocumentId> {
        if self.host != LibraryBrowserHost::Home {
            return Vec::new();
        }
        if self.selection.contains(&entry.path) {
            let ids = self
                .entries
                .iter()
                .filter(|candidate| self.selection.contains(&candidate.path))
                .filter_map(|candidate| candidate.item_id)
                .collect::<HashSet<_>>();
            if !ids.is_empty() {
                return ids.into_iter().collect();
            }
        }
        entry.item_id.into_iter().collect()
    }

    pub(super) fn render_document_card(
        entry: &NewTabEntry,
        state: NewTabCardState,
        drag_ids: Vec<DocumentId>,
        theme: ThemeColor,
        page_color: u32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let NewTabCardState { thumbnail, item } = state;
        let item_ix = item.item_ix;
        let path = entry.path.clone();
        let file_name = entry.file_name.clone();
        let directory = entry.directory.clone();
        let last_read = entry.last_read.clone();
        let border_color = if item.focused {
            theme.ring
        } else if item.selected {
            theme.primary
        } else {
            theme.border
        };
        let card_bg = if item.selected {
            theme.primary.opacity(0x22 as f32 / 255.0)
        } else {
            theme.popover
        };

        let draggable = !drag_ids.is_empty();
        let dragged = DraggedLibrarySelection {
            active: LibraryNodeId::Item(entry.item_id.unwrap_or(DocumentId(0))),
            marked: drag_ids
                .into_iter()
                .map(LibraryNodeId::Item)
                .collect::<Vec<_>>()
                .into(),
            label: file_name.clone(),
        };
        div()
            .id(("new-tab-card", item_ix))
            .debug_selector(move || format!("new-tab-card-{item_ix}"))
            .flex()
            .flex_none()
            .flex_col()
            .items_center()
            .relative()
            .w(px(CARD_WIDTH))
            .h(px(CARD_HEIGHT))
            .p_3()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(border_color)
            .bg(card_bg)
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.handle_document_pointer(
                        path.clone(),
                        event.modifiers,
                        event.click_count,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .when(draggable, |card| {
                card.on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            })
            .when(item.selecting, |card| {
                card.child(
                    Checkbox::new(("new-tab-card-selection", item.item_ix))
                        .xsmall()
                        .checked(item.selected)
                        .tab_stop(false)
                        .absolute()
                        .top(px(6.0))
                        .right(px(6.0))
                        .p_1()
                        .bg(theme.popover.opacity(0xe8 as f32 / 255.0))
                        .rounded(cx.theme().radius),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(THUMBNAIL_MAX_WIDTH))
                    .h(px(THUMBNAIL_MAX_HEIGHT))
                    .child(Self::render_thumbnail(thumbnail, theme, page_color)),
            )
            .child(
                div()
                    .mt_3()
                    .w_full()
                    .truncate()
                    .text_size(ui_rems(13.0))
                    .child(file_name),
            )
            .child(
                div()
                    .mt_1()
                    .w_full()
                    .truncate()
                    .text_size(ui_rems(11.0))
                    .text_color(theme.muted_foreground)
                    .child(directory),
            )
            .child(
                div()
                    .mt_1()
                    .w_full()
                    .truncate()
                    .text_size(ui_rems(11.0))
                    .text_color(theme.muted_foreground)
                    .child(last_read),
            )
    }

    pub(super) fn render_tree_document_row(
        entry: &NewTabEntry,
        state: NewTabItemState,
        drag_ids: Vec<DocumentId>,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = entry.path.clone();
        let file_name = entry.file_name.clone();
        let icon = entry.icon();

        let draggable = !drag_ids.is_empty();
        let dragged = DraggedLibrarySelection {
            active: LibraryNodeId::Item(entry.item_id.unwrap_or(DocumentId(0))),
            marked: drag_ids
                .into_iter()
                .map(LibraryNodeId::Item)
                .collect::<Vec<_>>()
                .into(),
            label: file_name.clone(),
        };
        let mut row = div()
            .id(("new-tab-tree-document-row", state.item_ix))
            .debug_selector(|| "new-tab-tree-document-row".to_string())
            .size_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(if state.focused {
                theme.ring
            } else if state.selected {
                theme.primary
            } else {
                transparent_black()
            })
            .bg(if state.selected {
                theme.primary.opacity(0x18 as f32 / 255.0)
            } else {
                transparent_black()
            })
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.handle_document_pointer(
                        path.clone(),
                        event.modifiers,
                        event.click_count,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .when(draggable, |row| {
                row.on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            });

        if state.selecting {
            row = row.child(
                Checkbox::new(("new-tab-row-selection", state.item_ix))
                    .xsmall()
                    .checked(state.selected)
                    .tab_stop(false),
            );
        }

        row = row
            .child(
                Icon::new(icon)
                    .size(ui_rems(16.0))
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .debug_selector(|| "new-tab-tree-document-name".to_string())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(file_name),
            );

        div()
            .debug_selector(|| "new-tab-tree-row-slot".to_string())
            .h(px(LIST_ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .px_1()
            .py_0p5()
            .child(row)
            .into_any_element()
    }

    pub(super) fn render_document_span(
        entry: &NewTabEntry,
        state: NewTabItemState,
        drag_ids: Vec<DocumentId>,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = entry.path.clone();
        let draggable = !drag_ids.is_empty();
        let dragged = DraggedLibrarySelection {
            active: LibraryNodeId::Item(entry.item_id.unwrap_or(DocumentId(0))),
            marked: drag_ids
                .into_iter()
                .map(LibraryNodeId::Item)
                .collect::<Vec<_>>()
                .into(),
            label: entry.file_name.clone(),
        };
        let mut span = div()
            .id(("new-tab-grouped-item", state.item_ix))
            .debug_selector(|| "new-tab-grouped-item".to_string())
            .h(px(40.0))
            .w_auto()
            .max_w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(if state.focused {
                theme.ring
            } else if state.selected {
                theme.primary
            } else {
                theme.border
            })
            .bg(if state.selected {
                theme.primary.opacity(0x18 as f32 / 255.0)
            } else {
                theme.popover
            })
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.handle_document_pointer(
                        path.clone(),
                        event.modifiers,
                        event.click_count,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .when(draggable, |span| {
                span.on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            });
        if state.selecting {
            span = span.child(
                Checkbox::new(("new-tab-grouped-selection", state.item_ix))
                    .xsmall()
                    .checked(state.selected)
                    .tab_stop(false),
            );
        }
        span.child(
            Icon::new(entry.icon())
                .size(ui_rems(16.0))
                .text_color(if entry.missing {
                    theme.danger
                } else {
                    theme.muted_foreground
                }),
        )
        .child(div().min_w_0().truncate().child(entry.file_name.clone()))
        .into_any_element()
    }

    pub(super) fn grouped_time_label(timestamp: Option<u64>, unknown: &'static str) -> String {
        let Some(timestamp) = timestamp
            .and_then(|timestamp| i64::try_from(timestamp).ok())
            .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0))
        else {
            return unknown.to_string();
        };
        let date = timestamp.with_timezone(&Local).date_naive();
        let today = Local::now().date_naive();
        if date == today {
            "Today".to_string()
        } else if date.iso_week() == today.iso_week() {
            "This Week".to_string()
        } else if date.year() == today.year() && date.month() == today.month() {
            "This Month".to_string()
        } else {
            "Earlier".to_string()
        }
    }

    pub(super) fn grouped_alpha_label(value: &str, empty: &'static str) -> String {
        let value = value.trim();
        if value.is_empty() {
            return empty.to_string();
        }
        value
            .chars()
            .next()
            .filter(|character| character.is_alphabetic())
            .map(|character| character.to_uppercase().collect::<String>())
            .unwrap_or_else(|| "#".to_string())
    }

    pub(super) fn grouped_sections(&self) -> Vec<(String, Vec<usize>)> {
        let mut groups = HashMap::<String, Vec<usize>>::new();
        for (item_ix, entry_ix) in self.filtered_entries.iter().copied().enumerate() {
            let entry = &self.entries[entry_ix];
            let label = match self.session.sort.field {
                LibrarySortField::DateAdded => {
                    Self::grouped_time_label(entry.date_added, "Unknown")
                }
                LibrarySortField::LastOpened => {
                    Self::grouped_time_label(entry.last_opened_timestamp, "Never Opened")
                }
                LibrarySortField::Year => entry
                    .year
                    .map(|year| year.to_string())
                    .unwrap_or_else(|| "Unknown".to_string()),
                LibrarySortField::Title => Self::grouped_alpha_label(&entry.title_sort, "#"),
                LibrarySortField::Creator => {
                    Self::grouped_alpha_label(&entry.creator_sort, "Unknown")
                }
            };
            groups.entry(label).or_default().push(item_ix);
        }

        let direction = self.session.sort.direction;
        let field = self.session.sort.field;
        let mut labels = groups.keys().cloned().collect::<Vec<_>>();
        labels.sort_by(|left, right| {
            let ordering = match field {
                LibrarySortField::DateAdded | LibrarySortField::LastOpened => {
                    let rank = |label: &str| match (direction, label) {
                        (_, "Never Opened" | "Unknown") => 4,
                        (LibrarySortDirection::Descending, "Today") => 0,
                        (LibrarySortDirection::Descending, "This Week") => 1,
                        (LibrarySortDirection::Descending, "This Month") => 2,
                        (LibrarySortDirection::Descending, "Earlier") => 3,
                        (LibrarySortDirection::Ascending, "Earlier") => 0,
                        (LibrarySortDirection::Ascending, "This Month") => 1,
                        (LibrarySortDirection::Ascending, "This Week") => 2,
                        (LibrarySortDirection::Ascending, "Today") => 3,
                        _ => 4,
                    };
                    rank(left).cmp(&rank(right))
                }
                LibrarySortField::Year => {
                    if left == "Unknown" {
                        std::cmp::Ordering::Greater
                    } else if right == "Unknown" {
                        std::cmp::Ordering::Less
                    } else {
                        let ordering = left
                            .parse::<i32>()
                            .unwrap_or_default()
                            .cmp(&right.parse::<i32>().unwrap_or_default());
                        if direction == LibrarySortDirection::Ascending {
                            ordering
                        } else {
                            ordering.reverse()
                        }
                    }
                }
                LibrarySortField::Title | LibrarySortField::Creator => {
                    let left_last = left == "#" || left == "Unknown";
                    let right_last = right == "#" || right == "Unknown";
                    match (left_last, right_last) {
                        (true, false) => std::cmp::Ordering::Greater,
                        (false, true) => std::cmp::Ordering::Less,
                        _ => {
                            let ordering = left.cmp(right);
                            if direction == LibrarySortDirection::Ascending {
                                ordering
                            } else {
                                ordering.reverse()
                            }
                        }
                    }
                }
            };
            ordering.then_with(|| left.cmp(right))
        });
        labels
            .into_iter()
            .map(|label| {
                let items = groups.remove(&label).unwrap_or_default();
                (label, items)
            })
            .collect()
    }

    pub(super) fn render_empty_state(
        &self,
        state: BrowserEmptyState,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let BrowserEmptyState {
            title,
            description,
            icon,
            action,
        } = state;
        let highlighted = matches!(action, BrowserEmptyAction::AddFiles(_))
            && self.tree.drop_target == Some(LibraryDropTarget::Root);
        let mut empty = div()
            .id("new-tab-empty-state")
            .debug_selector(|| "new-tab-empty-state".to_string())
            .flex_1()
            .min_h(px(180.0))
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .p_6()
            .rounded(cx.theme().radius)
            .bg(if highlighted {
                theme.drop_target
            } else {
                transparent_black()
            })
            .child(
                Icon::new(icon)
                    .size(ui_rems(28.0))
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .text_size(ui_rems(15.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(title),
            )
            .child(
                div()
                    .max_w(px(460.0))
                    .text_center()
                    .text_size(ui_rems(11.0))
                    .text_color(theme.muted_foreground)
                    .child(description),
            );

        match action {
            BrowserEmptyAction::None => {}
            BrowserEmptyAction::AddFiles(target_collection) => {
                empty = empty.child(
                    Button::new("library-empty-add-files")
                        .debug_selector(|| "library-empty-add-files".to_string())
                        .small()
                        .primary()
                        .icon(IconName::Plus)
                        .label(crate::fl!("new-tab-add-files"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.add_files_to(target_collection, window, cx);
                            cx.stop_propagation();
                        })),
                );
            }
            BrowserEmptyAction::OpenFile => {
                empty = empty.child(
                    Button::new("new-tab-empty-open-file")
                        .debug_selector(|| "new-tab-empty-open-file".to_string())
                        .small()
                        .primary()
                        .icon(IconName::FolderOpen)
                        .label(crate::fl!("new-tab-open-file"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.open_file(cx);
                            cx.stop_propagation();
                        })),
                );
            }
        }

        if let BrowserEmptyAction::AddFiles(target_collection) = action {
            empty = empty
                .on_drag_move::<ExternalPaths>(cx.listener(
                    |this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                        this.set_external_library_root_drag_target(
                            event.bounds.contains(&event.event.position),
                            cx,
                        );
                    },
                ))
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                    this.drop_external_library_paths(paths, target_collection, cx);
                    cx.stop_propagation();
                }))
                .on_mouse_exit(cx.listener(|this, _: &MouseExitEvent, _, cx| {
                    this.set_external_library_root_drag_target(false, cx);
                }));
        }

        empty.into_any_element()
    }

    pub(super) fn render_grid(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cols = self.cols.max(1);
        let item_count = self.item_count();
        let item_rows = item_count.div_ceil(cols);
        let filtering = !self.search_input.read(cx).value().trim().is_empty();
        let grid_width = if self.browser_viewport_width > 0.0 {
            self.browser_viewport_width
        } else {
            self.viewport_width
        };
        let horizontal_gap = Self::horizontal_gap(grid_width, cols);
        let scroll = self.scroll.clone();
        let breadcrumb = self.render_collection_breadcrumb(cx.theme().colors, cx);
        let folders = self.visible_child_collections(cx);
        let has_folders = !folders.is_empty();
        let folder_grid = (!folders.is_empty()).then(|| {
            let mut grid = div()
                .id("library-folder-grid")
                .debug_selector(|| "library-folder-grid".to_string())
                .flex_none()
                .max_h(px(220.0))
                .overflow_y_scroll()
                .flex()
                .flex_wrap()
                .items_start()
                .gap_3()
                .px(px(GRID_PADDING_X))
                .py_3();
            for (id, name) in folders {
                grid =
                    grid.child(self.render_collection_item(id, name, false, cx.theme().colors, cx));
            }
            grid.into_any_element()
        });

        if item_count == 0 {
            let show_empty = filtering || !has_folders || self.entries.is_empty();
            let mut content = div()
                .id("new-tab-content")
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .flex()
                .flex_col()
                .children(breadcrumb)
                .children(folder_grid);
            if show_empty {
                content = content.child(self.render_empty_state(
                    self.empty_state(filtering),
                    cx.theme().colors,
                    cx,
                ));
            }
            return content.into_any_element();
        }

        div()
            .id("new-tab-content")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .children(breadcrumb)
            .children(folder_grid)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        uniform_list(
                            "new-tab-grid",
                            item_rows,
                            cx.processor(move |this, visible_range: Range<usize>, _window, cx| {
                                let page_color = cx.global::<SettingsStore>().resolved_theme.page;
                                let theme = cx.theme().colors;
                                let prewarm_range = visible_range.start.saturating_sub(1)
                                    ..(visible_range.end + 1).min(item_rows);
                                let visible_paths = prewarm_range
                                    .flat_map(|row_ix| {
                                        let first_item_ix = row_ix * cols;
                                        first_item_ix..first_item_ix + cols
                                    })
                                    .filter_map(|item_ix| {
                                        this.entry_for_item(item_ix).map(|entry| entry.path.clone())
                                    })
                                    .collect::<Vec<_>>();

                                for path in visible_paths {
                                    this.start_thumbnail_render(path, cx);
                                }

                                visible_range
                                    .map(|row_ix| {
                                        let mut row = div()
                                            .flex()
                                            .flex_row()
                                            .items_start()
                                            .justify_center()
                                            .gap(px(horizontal_gap))
                                            .w_full()
                                            .h(px(GRID_ROW_HEIGHT))
                                            .px(px(GRID_PADDING_X))
                                            .py_2();

                                        for col_ix in 0..cols {
                                            let item_ix = row_ix * cols + col_ix;
                                            if item_ix < item_count {
                                                let entry = this
                                                    .entry_for_item(item_ix)
                                                    .expect("document item must have an entry")
                                                    .clone();
                                                let thumbnail = this
                                                    .thumbnails
                                                    .get(&entry.path)
                                                    .cloned()
                                                    .unwrap_or(ThumbnailState::Loading);
                                                row = row.child(Self::render_document_card(
                                                    &entry,
                                                    NewTabCardState {
                                                        thumbnail,
                                                        item: this.item_state(item_ix),
                                                    },
                                                    this.drag_ids_for_entry(&entry),
                                                    theme,
                                                    page_color,
                                                    cx,
                                                ));
                                            } else {
                                                row = row.child(
                                                    div()
                                                        .flex_none()
                                                        .w(px(CARD_WIDTH))
                                                        .h(px(CARD_HEIGHT)),
                                                );
                                            }
                                        }

                                        row.into_any_element()
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&scroll)
                        .size_full(),
                    )
                    .vertical_scrollbar_for_with_id(&scroll, "new-tab-scrollbar", window, cx),
            )
            .into_any_element()
    }

    pub(super) fn render_list(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let filtering = !self.search_input.read(cx).value().trim().is_empty();
        let sections = self.grouped_sections();
        let theme = cx.theme().colors;
        let _ = window;
        let breadcrumb = self.render_collection_breadcrumb(theme, cx);
        let folders = self.visible_child_collections(cx);
        let sections_empty = sections.is_empty();
        let show_empty_below_folders = sections_empty && self.entries.is_empty();

        if sections_empty && folders.is_empty() {
            return div()
                .id("new-tab-content")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .children(breadcrumb)
                .child(self.render_empty_state(self.empty_state(filtering), theme, cx))
                .into_any_element();
        }

        let mut content = div()
            .id("new-tab-grouped-list")
            .debug_selector(|| "new-tab-grouped-list".to_string())
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .p_4();
        if !folders.is_empty() {
            let mut folder_items = div()
                .id("library-folder-spans")
                .debug_selector(|| "library-folder-spans".to_string())
                .w_full()
                .flex()
                .flex_wrap()
                .items_start()
                .gap_2();
            for (id, name) in folders {
                folder_items =
                    folder_items.child(self.render_collection_item(id, name, true, theme, cx));
            }
            content = content.child(folder_items);
        }
        if show_empty_below_folders {
            content = content.child(self.render_empty_state(self.empty_state(false), theme, cx));
        }
        for (section_ix, (label, item_indices)) in sections.into_iter().enumerate() {
            let mut items = div()
                .id(("new-tab-grouped-section-items", section_ix))
                .w_full()
                .flex()
                .flex_wrap()
                .items_start()
                .gap_2();
            for item_ix in item_indices {
                let entry = self
                    .entry_for_item(item_ix)
                    .expect("grouped document item must have an entry")
                    .clone();
                items = items.child(Self::render_document_span(
                    &entry,
                    self.item_state(item_ix),
                    self.drag_ids_for_entry(&entry),
                    theme,
                    cx,
                ));
            }
            content = content.child(
                div()
                    .id(("new-tab-grouped-section", section_ix))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child(label),
                    )
                    .child(items),
            );
        }

        div()
            .id("new-tab-content")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .children(breadcrumb)
            .child(content)
            .into_any_element()
    }

    pub(super) fn render_library_tree_document_row(
        &self,
        entry_ix: usize,
        depth: usize,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(entry) = self.entries.get(entry_ix) else {
            return div().h(px(LIST_ROW_HEIGHT)).into_any_element();
        };
        let Some(item_id) = entry.item_id else {
            return div().h(px(LIST_ROW_HEIGHT)).into_any_element();
        };
        let node = LibraryNodeId::Item(item_id);
        let label = entry.file_name.clone();
        let icon = entry.icon();
        let selected = self.tree.active == Some(node) || self.tree.marked.contains(&node);
        let drop_highlighted = self.library_node_is_drop_highlighted(node, cx);
        let drag_dimmed = cx.has_active_drag() && self.tree.dragging_nodes.contains(&node);
        let dragged = self.dragged_library_selection(node, label.clone());
        let mut row = div()
            .id(("library-document-tree-row", item_id.0 as usize))
            .debug_selector(move || format!("library-document-tree-row-{}", item_id.0))
            .h(px(LIST_ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .pr_3()
            .pl(px(34.0 + depth as f32 * 20.0))
            .border_b(px(1.0))
            .border_color(theme.border.opacity(0x70 as f32 / 255.0))
            .opacity(if drag_dimmed { 0.45 } else { 1.0 })
            .bg(if drop_highlighted {
                theme.drop_target
            } else if selected {
                theme.list_active
            } else {
                transparent_black()
            })
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if !event.standard_click() {
                    return;
                }
                this.handle_tree_pointer(node, event.modifiers(), event.click_count(), cx);
                cx.stop_propagation();
            }));
        if self.host == LibraryBrowserHost::Home {
            let view = cx.entity();
            row = row
                .on_drag(dragged, move |dragged, _, _, cx| {
                    view.update(cx, |this, cx| this.begin_library_drag(dragged, cx));
                    cx.new(|_| dragged.clone())
                })
                .on_drag_move::<DraggedLibrarySelection>(cx.listener(
                    move |this, event: &DragMoveEvent<DraggedLibrarySelection>, window, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        let dragged = event.drag(cx).clone();
                        this.set_library_drag_target(node, &dragged, active, window, cx);
                    },
                ))
                .on_drop(
                    cx.listener(move |this, dragged: &DraggedLibrarySelection, _, cx| {
                        let parent = cx.global::<LibraryStore>().item_parent(item_id);
                        this.drop_library_selection(dragged, parent, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_drag_move::<ExternalPaths>(cx.listener(
                    move |this, event: &DragMoveEvent<ExternalPaths>, window, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        this.set_external_library_drag_target(node, active, window, cx);
                    },
                ))
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                    let parent = cx.global::<LibraryStore>().item_parent(item_id);
                    this.drop_external_library_paths(paths, parent, cx);
                    cx.stop_propagation();
                }));
        }
        row.child(
            Icon::new(icon)
                .size(ui_rems(16.0))
                .text_color(theme.muted_foreground),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(label),
        )
        .into_any_element()
    }

    pub(super) fn render_collection_tree_row(
        &self,
        id: LibraryCollectionId,
        depth: usize,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let management = self.host == LibraryBrowserHost::Home;
        let Some(collection) = cx.global::<LibraryStore>().library_collection(id) else {
            return div().h(px(LIST_ROW_HEIGHT)).into_any_element();
        };
        let name = collection.name.clone();
        let expanded = self.tree.expanded_collections.contains(&id);
        let node = LibraryNodeId::Collection(id);
        let selected = self.tree.active == Some(node) || self.tree.marked.contains(&node);
        let drop_highlighted = self.library_node_is_drop_highlighted(node, cx);
        let drag_dimmed = cx.has_active_drag() && self.tree.dragging_nodes.contains(&node);
        let mut row = div()
            .id(("library-folder-tree-row", id.0 as usize))
            .debug_selector(move || format!("library-folder-tree-row-{}", id.0))
            .h(px(LIST_ROW_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .pr_3()
            .pl(px(14.0 + depth as f32 * 20.0))
            .border_b(px(1.0))
            .border_color(theme.border.opacity(0x70 as f32 / 255.0))
            .opacity(if drag_dimmed { 0.45 } else { 1.0 })
            .bg(if drop_highlighted {
                theme.drop_target
            } else if selected {
                theme.list_active
            } else {
                transparent_black()
            })
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if !event.standard_click() {
                    return;
                }
                this.handle_tree_pointer(node, event.modifiers(), event.click_count(), cx);
                cx.stop_propagation();
            }));
        if management {
            let dragged = self.dragged_library_selection(node, name.clone().into());
            let view = cx.entity();
            row = row
                .on_drag(dragged, move |dragged, _, _, cx| {
                    view.update(cx, |this, cx| this.begin_library_drag(dragged, cx));
                    cx.new(|_| dragged.clone())
                })
                .on_drag_move::<DraggedLibrarySelection>(cx.listener(
                    move |this, event: &DragMoveEvent<DraggedLibrarySelection>, window, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        let dragged = event.drag(cx).clone();
                        this.set_library_drag_target(node, &dragged, active, window, cx);
                    },
                ))
                .on_drop(
                    cx.listener(move |this, dragged: &DraggedLibrarySelection, _, cx| {
                        this.drop_library_selection(dragged, Some(id), cx);
                        cx.stop_propagation();
                    }),
                )
                .on_drag_move::<ExternalPaths>(cx.listener(
                    move |this, event: &DragMoveEvent<ExternalPaths>, window, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        this.set_external_library_drag_target(node, active, window, cx);
                    },
                ))
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                    this.drop_external_library_paths(paths, Some(id), cx);
                    cx.stop_propagation();
                }));
        }

        row = row
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(ui_rems(12.0))
                .text_color(theme.muted_foreground),
            )
            .child(
                Icon::new(IconName::FolderOpen)
                    .size(ui_rems(16.0))
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(name),
            );
        if management {
            let view = cx.entity();
            return row
                .context_menu(move |menu, _, _| {
                    Self::collection_context_menu(menu, Some(id), view.clone())
                })
                .into_any_element();
        }
        row.into_any_element()
    }

    pub(super) fn render_tree_root_area(
        &self,
        theme: ThemeColor,
        empty_state: Option<BrowserEmptyState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let highlighted = self.tree.drop_target == Some(LibraryDropTarget::Root);
        let mut area = div()
            .id("library-tree-root-area")
            .debug_selector(|| "library-tree-root-area".to_string())
            .flex_grow_1()
            .min_h_0()
            .w_full()
            .bg(if highlighted {
                theme.drop_target
            } else {
                transparent_black()
            })
            .children(empty_state.map(|state| self.render_empty_state(state, theme, cx)));
        if self.host == LibraryBrowserHost::Home {
            area = area
                .on_drag_move::<DraggedLibrarySelection>(cx.listener(
                    move |this, event: &DragMoveEvent<DraggedLibrarySelection>, _, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        let dragged = event.drag(cx).clone();
                        this.set_library_root_drag_target(&dragged, active, cx);
                    },
                ))
                .on_drop(
                    cx.listener(move |this, dragged: &DraggedLibrarySelection, _, cx| {
                        this.drop_library_selection(dragged, None, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_drag_move::<ExternalPaths>(cx.listener(
                    move |this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        this.set_external_library_root_drag_target(active, cx);
                    },
                ))
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                    this.drop_external_library_paths(paths, None, cx);
                    cx.stop_propagation();
                }));
            let view = cx.entity();
            return area
                .context_menu(move |menu, _, _| {
                    Self::collection_context_menu(menu, None, view.clone())
                })
                .into_any_element();
        }
        area.into_any_element()
    }

    pub(super) fn render_tree(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !cx.has_active_drag() {
            self.tree.dragging_nodes.clear();
        }
        let filtering = !self.search_input.read(cx).value().trim().is_empty();
        let tree_mode = self.session.destination == ReaderNewTabDestination::Library;
        let item_count = if tree_mode {
            self.tree.rows.len()
        } else {
            self.item_count()
        };
        if !tree_mode && item_count == 0 {
            return div()
                .id("new-tab-content")
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(self.render_empty_state(self.empty_state(filtering), cx.theme().colors, cx))
                .into_any_element();
        }
        let root_empty_state = tree_mode
            .then(|| {
                if filtering && item_count == 0 {
                    Some(self.empty_state(true))
                } else if !filtering && self.entries.is_empty() {
                    Some(self.empty_state(false))
                } else {
                    None
                }
            })
            .flatten();
        let scroll = self.scroll.clone();
        let list = uniform_list(
            "new-tab-tree-list",
            item_count,
            cx.processor(move |this, visible_range: Range<usize>, _, cx| {
                let theme = cx.theme().colors;
                visible_range
                    .map(|item_ix| {
                        if tree_mode {
                            match this.tree.rows[item_ix].clone() {
                                LibraryTreeRow::Collection { id, depth } => {
                                    this.render_collection_tree_row(id, depth, theme, cx)
                                }
                                LibraryTreeRow::Document { entry_ix, depth } => this
                                    .render_library_tree_document_row(entry_ix, depth, theme, cx),
                            }
                        } else {
                            let entry = this
                                .entry_for_item(item_ix)
                                .expect("document item must have an entry")
                                .clone();
                            Self::render_tree_document_row(
                                &entry,
                                this.item_state(item_ix),
                                this.drag_ids_for_entry(&entry),
                                theme,
                                cx,
                            )
                        }
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&scroll)
        .when(tree_mode, |list| {
            list.with_sizing_behavior(ListSizingBehavior::Infer)
        })
        .when(!tree_mode, |list| list.size_full());
        let root_area =
            tree_mode.then(|| self.render_tree_root_area(cx.theme().colors, root_empty_state, cx));

        div()
            .id("new-tab-content")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("new-tab-tree-viewport")
                    .debug_selector(|| "new-tab-tree-viewport".to_string())
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .when(tree_mode && self.host == LibraryBrowserHost::Home, |view| {
                        view.on_drag_move::<DraggedLibrarySelection>(
                            cx.listener(Self::handle_library_drag_move::<DraggedLibrarySelection>),
                        )
                        .on_drag_move::<ExternalPaths>(
                            cx.listener(Self::handle_library_drag_move::<ExternalPaths>),
                        )
                        .on_mouse_exit(cx.listener(
                            |this, _: &MouseExitEvent, _, cx| this.clear_library_drop_feedback(cx),
                        ))
                    })
                    .child(list)
                    .children(root_area)
                    .vertical_scrollbar_for_with_id(&scroll, "new-tab-scrollbar", window, cx),
            )
            .into_any_element()
    }
}

pub(super) fn format_file_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    if bytes >= GIB {
        format!("{} GiB", format_user_float(bytes as f64 / GIB as f64))
    } else if bytes >= MIB {
        format!("{} MiB", format_user_float(bytes as f64 / MIB as f64))
    } else if bytes >= KIB {
        format!("{} KiB", format_user_float(bytes as f64 / KIB as f64))
    } else {
        format!("{bytes} B")
    }
}

impl Render for NewTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if cx.global::<LibraryStore>().catalog_revision() != self.observed_catalog_revision {
            self.refresh_entries(cx);
        }
        self.sync_work_note_input(window, cx);
        let theme = cx.theme().colors;
        let view_mode = self.view_mode(cx);
        let measure_view = cx.entity();

        let measure = canvas(
            {
                let measure_view = measure_view.clone();
                move |bounds, _window, cx| {
                    let width = bounds.size.width.as_f32();
                    let height = bounds.size.height.as_f32();
                    measure_view.update(cx, |this, cx| this.update_viewport(width, height, cx));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let content = match &self.session.destination {
            ReaderNewTabDestination::Tags => self.render_tags_content(theme, cx),
            _ => match view_mode {
                NewTabViewMode::Grid => self.render_grid(window, cx),
                NewTabViewMode::Grouped => self.render_list(window, cx),
                NewTabViewMode::Table => self.render_tree(window, cx),
            },
        };
        let management_host = self.host == LibraryBrowserHost::Home;
        let wide_inspector = management_host && self.viewport_width >= 960.0;
        let inspector = (wide_inspector && self.inspector_open)
            .then(|| self.render_inspector(theme, cx))
            .flatten();
        let inspector_overlay = (management_host && !wide_inspector && self.inspector_open)
            .then(|| self.render_inspector(theme, cx))
            .flatten()
            .map(|inspector| {
                div()
                    .id("new-tab-item-inspector-overlay")
                    .absolute()
                    .top(px(NEW_TAB_HEADER_HEIGHT))
                    .right_0()
                    .bottom_0()
                    .w(px(NEW_TAB_INSPECTOR_WIDTH))
                    .shadow_lg()
                    .child(inspector)
                    .into_any_element()
            });
        let browser_measure = canvas(
            {
                let measure_view = measure_view.clone();
                move |bounds, _window, cx| {
                    measure_view.update(cx, |this, cx| {
                        this.update_browser_viewport(bounds.size.width.as_f32(), cx);
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let browser_content = div()
            .id("new-tab-browser-viewport")
            .debug_selector(|| "new-tab-browser-viewport".to_string())
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(browser_measure)
            .child(content);
        let content = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .child(browser_content)
            .children(inspector);

        let tree_selection = self.effective_tree_selection();
        let selecting = if self.is_library_tree() {
            self.selecting || !self.tree.marked.is_empty()
        } else {
            self.selecting
        };
        let has_selection = if self.is_library_tree() {
            !tree_selection.is_empty()
        } else {
            !self.selection.is_empty()
        };
        let selected_count = if self.is_library_tree() {
            tree_selection.len() as i64
        } else {
            self.selection.len() as i64
        };
        let (filtered_count, total_count) =
            if self.session.destination == ReaderNewTabDestination::Tags {
                self.tag_result_counts(cx)
            } else {
                (
                    self.filtered_entries.len() as i64,
                    self.entries.len() as i64,
                )
            };
        let library_error = cx
            .global::<LibraryStore>()
            .startup_error()
            .map(|error| format!("Library unavailable. Changes will not be saved: {error}"));
        let import_progress = self.import_progress.clone();
        let import_message = self.import_message.clone();
        let destination_title =
            Self::destination_label(&self.session.destination, cx.global::<LibraryStore>());
        let in_trash = matches!(self.session.destination, ReaderNewTabDestination::Trash);
        let in_history = matches!(self.session.destination, ReaderNewTabDestination::History);
        // Retain the persisted advanced presentation state for compatibility,
        // but keep its controls out of the current clean browser UI.
        let can_save_view = false;
        let is_saved_view = false;
        let can_sort = false;
        let library_management = false;
        let sort_label = Self::sort_field_label(self.session.sort.field);
        let sort_ascending = self.session.sort.direction == LibrarySortDirection::Ascending;
        let kind_filter_label = "All Types";
        let status_filter_label = "All Statuses";
        let header =
            div()
                .flex()
                .flex_col()
                .flex_none()
                .h(px(NEW_TAB_HEADER_HEIGHT))
                .gap_1()
                .px_4()
                .py_2()
                .border_b(px(1.0))
                .border_color(theme.border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(ui_rems(14.0))
                                .child(if management_host {
                                    self.render_destination_heading(destination_title, theme, cx)
                                } else {
                                    self.render_quick_source_selector(theme, cx)
                                })
                                .child(
                                    div()
                                        .text_size(ui_rems(11.0))
                                        .text_color(theme.muted_foreground)
                                        .child(crate::fl!(
                                            "new-tab-results-count",
                                            visible = filtered_count,
                                            total = total_count
                                        )),
                                )
                                .when(selecting, |this| {
                                    this.child(
                                        div()
                                            .text_size(ui_rems(11.0))
                                            .text_color(theme.muted_foreground)
                                            .child(crate::fl!(
                                                "selected-count",
                                                count = selected_count
                                            )),
                                    )
                                }),
                        )
                        .child({
                            let mut actions = div().flex().items_center().gap_1();
                            if let Some(progress) = import_progress {
                                let label = if progress.total == 0 {
                                    "Scanning…".to_string()
                                } else {
                                    format!(
                                        "Importing {} of {}",
                                        progress.completed.min(progress.total),
                                        progress.total
                                    )
                                };
                                actions = actions
                                    .child(
                                        div()
                                            .text_size(ui_rems(11.0))
                                            .text_color(theme.muted_foreground)
                                            .child(label),
                                    )
                                    .child(
                                        Button::new("new-tab-cancel-import")
                                            .xsmall()
                                            .outline()
                                            .label("Cancel")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.cancel_library_import(cx);
                                                cx.stop_propagation();
                                            })),
                                    );
                            }
                            actions = actions.child(
                                Button::new("new-tab-open-file")
                                    .debug_selector(|| "new-tab-open-file".to_string())
                                    .small()
                                    .outline()
                                    .icon(IconName::FolderOpen)
                                    .label(crate::fl!("new-tab-open-file"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.open_file(cx);
                                        cx.stop_propagation();
                                    })),
                            );
                            if management_host {
                                actions = actions.child(
                                    Button::new("new-tab-add-files")
                                        .debug_selector(|| "new-tab-add-files".to_string())
                                        .small()
                                        .primary()
                                        .icon(IconName::Plus)
                                        .label(crate::fl!("new-tab-add-files"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.add_files(window, cx);
                                            cx.stop_propagation();
                                        })),
                                );
                            }
                            if management_host
                                && self.session.destination == ReaderNewTabDestination::Library
                            {
                                let parent_id = if view_mode == NewTabViewMode::Table {
                                    self.library_import_target(cx)
                                } else {
                                    self.tree.current_collection
                                };
                                actions = actions.child(
                                    Button::new("home-library-new-folder")
                                        .debug_selector(|| "home-library-new-folder".to_string())
                                        .small()
                                        .outline()
                                        .icon(IconName::Plus)
                                        .label("New Folder")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_new_folder_editor(parent_id, window, cx);
                                            cx.stop_propagation();
                                        })),
                                );
                            }
                            actions = actions
                                .child(Self::render_view_button(
                                    NewTabViewMode::Grid,
                                    view_mode,
                                    cx,
                                ))
                                .child(Self::render_view_button(
                                    NewTabViewMode::Table,
                                    view_mode,
                                    cx,
                                ))
                                .child(Self::render_view_button(
                                    NewTabViewMode::Grouped,
                                    view_mode,
                                    cx,
                                ));
                            if management_host && has_selection && self.viewport_width < 960.0 {
                                actions = actions.child(Self::render_toolbar_button(
                                    "Details".to_string(),
                                    |this, _, cx| {
                                        this.inspector_open = true;
                                        cx.notify();
                                    },
                                    cx,
                                ));
                            }
                            if management_host && selecting && has_selection {
                                if in_trash {
                                    actions = actions.child(Self::render_toolbar_button(
                                        "Restore".to_string(),
                                        |this, _, cx| this.restore_selected_items(cx),
                                        cx,
                                    ));
                                }
                                actions = actions
                                    .child(Self::render_toolbar_button(
                                        if in_trash {
                                            "Delete Permanently".to_string()
                                        } else if in_history {
                                            crate::fl!("new-tab-remove-from-recents")
                                        } else {
                                            "Move to Trash".to_string()
                                        },
                                        |this, window, cx| this.delete_selected(window, cx),
                                        cx,
                                    ))
                                    .child(Self::render_toolbar_button(
                                        crate::fl!("cancel-selection"),
                                        |this, _, cx| this.cancel_select(cx),
                                        cx,
                                    ));
                            } else if management_host && selecting {
                                actions = actions.child(Self::render_toolbar_button(
                                    crate::fl!("cancel"),
                                    |this, _, cx| this.cancel_select(cx),
                                    cx,
                                ));
                            } else if management_host {
                                actions = actions.child(Self::render_toolbar_button(
                                    crate::fl!("select"),
                                    |this, _, cx| this.enter_select(cx),
                                    cx,
                                ));
                            }
                            actions
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("new-tab-search")
                                .debug_selector(|| "new-tab-search".to_string())
                                .flex()
                                .flex_1()
                                .min_w_0()
                                .h(px(30.0))
                                .items_center()
                                .gap_1()
                                .px_2()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.background)
                                .child(
                                    Icon::new(IconName::Search)
                                        .size(ui_rems(14.0))
                                        .text_color(theme.muted_foreground),
                                )
                                .child(div().flex_1().min_w_0().child(
                                    Input::new(&self.search_input).small().appearance(false),
                                )),
                        )
                        .when(false, |search_row| {
                            search_row.child({
                                let mut toolbar = div().flex().flex_none().items_center().gap_1();
                                if library_management {
                                    toolbar = toolbar
                                        .child(self.render_library_filter_menu(theme, cx))
                                        .child(
                                            Button::new("home-library-kind-filter")
                                                .xsmall()
                                                .outline()
                                                .label(kind_filter_label)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.cycle_kind_filter(cx);
                                                    cx.stop_propagation();
                                                })),
                                        )
                                        .child(
                                            Button::new("home-library-status-filter")
                                                .xsmall()
                                                .outline()
                                                .label(status_filter_label)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.cycle_status_filter(cx);
                                                    cx.stop_propagation();
                                                })),
                                        );
                                }
                                if can_sort {
                                    toolbar = toolbar
                                        .child(
                                            Button::new("new-tab-sort-field")
                                                .xsmall()
                                                .outline()
                                                .label(sort_label)
                                                .tooltip("Change sort field")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.cycle_sort_field(cx);
                                                    cx.stop_propagation();
                                                })),
                                        )
                                        .child(
                                            Button::new("new-tab-sort-direction")
                                                .xsmall()
                                                .ghost()
                                                .icon(if sort_ascending {
                                                    IconName::ChevronUp
                                                } else {
                                                    IconName::ChevronDown
                                                })
                                                .tooltip(if sort_ascending {
                                                    "Ascending"
                                                } else {
                                                    "Descending"
                                                })
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.toggle_sort_direction(cx);
                                                    cx.stop_propagation();
                                                })),
                                        );
                                }
                                toolbar = toolbar
                                    .child(Self::render_view_button(
                                        NewTabViewMode::Grid,
                                        view_mode,
                                        cx,
                                    ))
                                    .child(Self::render_view_button(
                                        NewTabViewMode::Table,
                                        view_mode,
                                        cx,
                                    ))
                                    .child(Self::render_view_button(
                                        NewTabViewMode::Grouped,
                                        view_mode,
                                        cx,
                                    ));
                                if can_save_view {
                                    toolbar = toolbar.child(
                                        Button::new("new-tab-save-current-view")
                                            .xsmall()
                                            .ghost()
                                            .icon(IconName::Table2)
                                            .tooltip("Save current view")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.open_saved_view_editor(window, cx);
                                                cx.stop_propagation();
                                            })),
                                    );
                                }
                                if is_saved_view {
                                    toolbar = toolbar.child(
                                        Button::new("new-tab-delete-current-view")
                                            .xsmall()
                                            .ghost()
                                            .icon(IconName::Trash2)
                                            .tooltip("Delete saved view")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.delete_current_saved_view(window, cx);
                                                cx.stop_propagation();
                                            })),
                                    );
                                }
                                toolbar = toolbar
                                    .child(div().mx_1().h(px(18.0)).w(px(1.0)).bg(theme.border));
                                if management_host && has_selection && self.viewport_width < 960.0 {
                                    toolbar = toolbar.child(Self::render_toolbar_button(
                                        "Details".to_string(),
                                        |this, _, cx| {
                                            this.inspector_open = true;
                                            cx.notify();
                                        },
                                        cx,
                                    ));
                                }
                                if management_host && selecting && has_selection {
                                    if in_trash {
                                        toolbar = toolbar.child(Self::render_toolbar_button(
                                            "Restore".to_string(),
                                            |this, _, cx| this.restore_selected_items(cx),
                                            cx,
                                        ));
                                    }
                                    toolbar = toolbar
                                        .child(Self::render_toolbar_button(
                                            if in_trash {
                                                "Delete Permanently".to_string()
                                            } else if in_history {
                                                crate::fl!("new-tab-remove-from-recents")
                                            } else {
                                                "Move to Trash".to_string()
                                            },
                                            |this, window, cx| this.delete_selected(window, cx),
                                            cx,
                                        ))
                                        .child(Self::render_toolbar_button(
                                            crate::fl!("cancel-selection"),
                                            |this, _, cx| this.cancel_select(cx),
                                            cx,
                                        ));
                                } else if management_host && selecting {
                                    toolbar = toolbar.child(Self::render_toolbar_button(
                                        crate::fl!("cancel"),
                                        |this, _, cx| this.cancel_select(cx),
                                        cx,
                                    ));
                                } else if management_host {
                                    toolbar = toolbar.child(Self::render_toolbar_button(
                                        crate::fl!("select"),
                                        |this, _, cx| this.enter_select(cx),
                                        cx,
                                    ));
                                }
                                toolbar
                            })
                        }),
                );

        let banners = library_error
            .map(SharedString::from)
            .into_iter()
            .chain(import_message)
            .map(|message| {
                div()
                    .mx_3()
                    .mt_3()
                    .p_2()
                    .rounded(px(4.0))
                    .bg(theme.list_hover)
                    .text_color(theme.primary)
                    .child(message)
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let mut main_column = div().flex_1().min_w_0().min_h_0().flex().flex_col();
        main_column = main_column.children(banners).child(header).child(content);
        let workspace_content = if management_host {
            div()
                .size_full()
                .min_w_0()
                .min_h_0()
                .flex()
                .gap(px(SURFACE_GAP))
                .px(px(SURFACE_INSET))
                .pb(px(SURFACE_INSET))
                .child(self.render_navigation(theme, cx))
                .child(main_column)
                .into_any_element()
        } else {
            main_column.into_any_element()
        };
        let note_overlay = (management_host && self.editors.note_expanded)
            .then(|| self.render_note_overlay(theme, cx));
        let metadata_overlay = (management_host && self.editors.metadata_open)
            .then(|| self.render_metadata_overlay(theme, cx));
        let saved_view_overlay = (management_host && self.editors.saved_view_open)
            .then(|| self.render_saved_view_overlay(theme, cx));
        let tag_editor_overlay = (management_host && self.editors.tag_id.is_some())
            .then(|| self.render_tag_editor_overlay(theme, cx));
        let folder_editor_overlay = management_host
            .then(|| self.render_folder_editor_overlay(theme, cx))
            .flatten();
        let editor_overlay_open = management_host
            && (self.editors.note_expanded
                || self.editors.metadata_open
                || self.editors.saved_view_open
                || self.editors.tag_id.is_some()
                || self.editors.folder.is_some());
        let external_drop_overlay = (management_host && self.tree.drop_target.is_none())
            .then(|| {
                self.external_document_drop_feedback
                    .map(|feedback| render_external_document_drop_overlay(feedback, theme))
            })
            .flatten();

        div()
            .id(if management_host {
                "home-view"
            } else {
                "new-tab"
            })
            .debug_selector(move || {
                if management_host {
                    "home-view".to_string()
                } else {
                    "new-tab".to_string()
                }
            })
            .relative()
            .size_full()
            .key_context(if editor_overlay_open {
                "NewTab Dialog"
            } else {
                "NewTab"
            })
            .track_focus(&self.focus_handle)
            .when(management_host, |this| {
                this.on_drag_move::<ExternalPaths>(cx.listener(Self::track_external_document_drag))
            })
            .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                if this.cancel_topmost_editor(window, cx) {
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ScrollLineDown, _, cx| {
                let mode = this.view_mode(cx);
                this.scroll_by(px(Self::row_height(mode)), cx);
            }))
            .on_action(cx.listener(|this, _: &ScrollLineUp, _, cx| {
                let mode = this.view_mode(cx);
                this.scroll_by(-px(Self::row_height(mode)), cx);
            }))
            .on_action(cx.listener(|this, _: &NextHalfPage, _, cx| {
                let viewport_height = this.scroll.0.borrow().base_handle.bounds().size.height;
                this.scroll_by(viewport_height * 0.5, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousHalfPage, _, cx| {
                let viewport_height = this.scroll.0.borrow().base_handle.bounds().size.height;
                this.scroll_by(-(viewport_height * 0.5), cx);
            }))
            .on_action(cx.listener(|this, _: &EnterSelect, _, cx| {
                this.enter_select(cx);
            }))
            .on_action(cx.listener(|this, _: &CancelSelect, _, cx| {
                this.cancel_select(cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteSelected, window, cx| {
                this.delete_selected(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.move_linear_cursor(true, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| {
                this.move_linear_cursor(false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorUp, _, cx| {
                this.move_cursor(CursorMove::Up, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorDown, _, cx| {
                this.move_cursor(CursorMove::Down, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorLeft, _, cx| {
                this.move_cursor(CursorMove::Left, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorRight, _, cx| {
                this.move_cursor(CursorMove::Right, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorFirst, _, cx| {
                this.move_cursor(CursorMove::First, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorLast, _, cx| {
                this.move_cursor(CursorMove::Last, false, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionUp, _, cx| {
                this.move_cursor(CursorMove::Up, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionDown, _, cx| {
                this.move_cursor(CursorMove::Down, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionLeft, _, cx| {
                this.move_cursor(CursorMove::Left, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionRight, _, cx| {
                this.move_cursor(CursorMove::Right, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionFirst, _, cx| {
                this.move_cursor(CursorMove::First, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionLast, _, cx| {
                this.move_cursor(CursorMove::Last, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivateFocused, _, cx| {
                this.activate_focused(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFocused, _, cx| {
                this.toggle_focused(cx);
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.select_all(cx);
            }))
            .flex()
            .flex_col()
            .bg(theme.tab_active)
            .text_size(ui_rems(12.0))
            .text_color(theme.foreground)
            .child(measure)
            .child(workspace_content)
            .children(inspector_overlay)
            .children(note_overlay)
            .children(metadata_overlay)
            .children(saved_view_overlay)
            .children(tag_editor_overlay)
            .children(folder_editor_overlay)
            .children(external_drop_overlay)
    }
}
