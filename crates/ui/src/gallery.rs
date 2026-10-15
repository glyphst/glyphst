//! Frame 01 Components of `design/main.pen`, drawn from the components:
//! the place to compare the code with the design (`glyphst-reader
//! --gallery --screenshot PATH`).

use egui::{Rect, Ui, vec2};

use crate::{
    badge::{Badge, Chip},
    button::{self, Button},
    chat::{self, StepState, TimelineStep},
    cover::DocCover,
    field::{SearchButton, TextField},
    key::Key,
    menu,
    meter::Meter,
    motif, overlay,
    pane::{self, Action},
    row::{ListRow, TreeRow},
    segmented,
    slider::Slider,
    status::{self, Segment, Side},
    tab::{self, DocTab},
    theme::Tokens,
    thumb::PageThumb,
    toggle::{Checkbox, Switch},
};

/// State the gallery's controls change.
pub struct Gallery {
    pub checked: bool,
    pub switch_on: bool,
    pub switch_off: bool,
    pub slider: f32,
    pub theme: u8,
    pub search: String,
    pub case: bool,
    pub regex: bool,
}

impl Default for Gallery {
    fn default() -> Self {
        Self {
            checked: true,
            switch_on: true,
            switch_off: false,
            slider: 0.6,
            theme: 2,
            search: String::new(),
            case: false,
            regex: false,
        }
    }
}

impl Gallery {
    pub fn show(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        ui.painter().rect_filled(ui.max_rect(), 0, t.bg);
        egui::ScrollArea::both().show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::same(48))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width().min(1420.0));
                    ui.spacing_mut().item_spacing = vec2(16.0, 16.0);
                    self.controls(ui);
                    ui.add_space(24.0);
                    self.chrome(ui);
                    ui.add_space(24.0);
                    self.content(ui);
                });
        });
    }

    fn section(ui: &mut Ui, title: &str) {
        ui.scope(|ui| {
            ui.set_width(706.0);
            motif::rule_title(ui, title, None);
        });
    }

    fn controls(&mut self, ui: &mut Ui) {
        Self::section(ui, "controls");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            ui.add(Button::primary("open"));
            ui.add(Button::ghost("cancel"));
            ui.add(Button::bracket("ask"));
            button::icon(ui, "settings", 28.0, false, "settings");
            ui.add(Key("ctrl k"));
            ui.add(Badge("24"));
            Chip::new("@p.12").icon("file-text").show(ui);
            ui.add(Checkbox::new(&mut self.checked, "pin translation"));
            ui.add(Switch(&mut self.switch_on));
            ui.add(Switch(&mut self.switch_off));
            ui.add(Slider::new(&mut self.slider, 0.0..=1.0).width(163.0));
            ui.add(Meter::new(0.58).label("58%"));
        });
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            ui.add(SearchButton {
                placeholder: "search or run a command",
                shortcut: "ctrl k",
                width: 320.0,
            });
            segmented::labels(
                ui,
                &mut self.theme,
                &[(0, "system"), (1, "light"), (2, "dark")],
            );
            let tip = ui.add(Button::plain("tooltip"));
            overlay::tooltip(tip, "find in document", Some("ctrl f"));
            TextField::search(&mut self.search, "softmax")
                .width(240.0)
                .toggle(crate::field::FieldToggle {
                    label: "Aa",
                    on: &mut self.case,
                    tooltip: "match case",
                })
                .toggle(crate::field::FieldToggle {
                    label: ".*",
                    on: &mut self.regex,
                    tooltip: "regular expression",
                })
                .show(ui);
        });
    }

    fn chrome(&mut self, ui: &mut Ui) {
        Self::section(ui, "chrome");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            for (title, active, dirty) in [
                ("vaswani-2017.pdf", false, false),
                ("attention.pdf", true, true),
            ] {
                let tab = DocTab {
                    title,
                    icon: Some("file-text"),
                    active,
                    dirty,
                    closable: true,
                };
                let width = tab.natural_width(ui);
                let (rect, response) =
                    ui.allocate_exact_size(vec2(width, tab::BAR_HEIGHT), egui::Sense::click());
                tab.paint(ui, rect, &response, false);
            }
            ui.allocate_ui_with_layout(
                vec2(40.0, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(40.0);
                    crate::rail::button(ui, "list-tree", false, "outline", None);
                },
            );
            ui.allocate_ui_with_layout(
                vec2(40.0, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(40.0);
                    crate::rail::button(ui, "list-tree", true, "outline", None);
                },
            );
            ui.allocate_ui_with_layout(
                vec2(300.0, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(300.0);
                    pane::Header::new("outline")
                        .trailing("24")
                        .action(Action::new("chevrons-down-up", "collapse all"))
                        .show(ui);
                },
            );
        });
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            ui.allocate_ui_with_layout(
                vec2(300.0, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(300.0);
                    ListRow::new("scaled dot-product attention")
                        .icon("bookmark")
                        .meta("p.4")
                        .show(ui);
                },
            );
            ui.allocate_ui_with_layout(
                vec2(300.0, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(300.0);
                    ListRow::new("multi-head attention")
                        .icon("bookmark")
                        .meta("p.5")
                        .active(true)
                        .show(ui);
                },
            );
            ui.allocate_ui_with_layout(
                vec2(300.0, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(300.0);
                    TreeRow {
                        title: "3.2 attention",
                        page: Some("4"),
                        ancestors_last: &[],
                        last: false,
                        active: false,
                    }
                    .show(ui);
                },
            );
            ui.allocate_ui_with_layout(
                vec2(220.0, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(220.0);
                    pane::popover(ui.ctx()).show(ui, |ui| {
                        menu::style(ui);
                        menu::Item::new("copy text")
                            .icon("copy")
                            .key("ctrl c")
                            .show(ui);
                        menu::Item::new("highlight")
                            .icon("highlighter")
                            .key("h")
                            .show(ui);
                        menu::Item::new("ask assistant")
                            .icon("message-square-plus")
                            .key("a")
                            .show(ui);
                        menu::separator(ui);
                        menu::Item::new("translate selection")
                            .icon("languages")
                            .key("t")
                            .show(ui);
                        menu::Item::new("bookmark page")
                            .icon("bookmark")
                            .key("b")
                            .show(ui);
                    });
                },
            );
        });
        ui.allocate_ui(vec2(706.0, status::HEIGHT), |ui| {
            let (rect, _) =
                ui.allocate_exact_size(vec2(706.0, status::HEIGHT), egui::Sense::hover());
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            status::bar(
                &mut child,
                vec![
                    Segment::label("page", Side::Left, Some("file-text"), "p 4 / 15"),
                    Segment::label("section", Side::Left, None, "3.2 attention").static_text(),
                    Segment::custom("progress", Side::Left, 160.0, |ui, rect| {
                        Meter::new(0.27)
                            .bar()
                            .label("27%")
                            .paint(ui, rect.shrink2(vec2(10.0, 0.0)));
                    }),
                    Segment::label("present", Side::Right, Some("presentation"), "present"),
                    Segment::label("assistant", Side::Right, Some("sparkles"), "assistant")
                        .accent(true),
                    Segment::label("zoom", Side::Right, Some("zoom-in"), "125%"),
                ],
            );
        });
    }

    fn content(&mut self, ui: &mut Ui) {
        Self::section(ui, "content");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 22.0;
            DocCover::new("attention is all you need", "pdf · 15p")
                .progress(Some(0.42), "42%")
                .show(ui);
            ui.allocate_ui_with_layout(
                vec2(320.0, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(320.0);
                    chat::user_message(
                        ui,
                        "you",
                        "Explain why the scaling factor 1/√dₖ matters here.",
                    );
                },
            );
            ui.allocate_ui_with_layout(
                vec2(320.0, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(320.0);
                    TimelineStep {
                        state: StepState::Done,
                        tool: "read_page",
                        detail: "p.4 · 2.1k tok",
                        time: "0.4s",
                        faded: false,
                    }
                    .show(ui);
                    chat::fold_header(ui, "worked · 3 steps · 2.4s", true, false);
                },
            );
            ui.allocate_ui_with_layout(
                vec2(320.0, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(320.0);
                    chat::suggestion(ui, "summarize this page", "/sum");
                },
            );
            PageThumb {
                label: "4",
                current: false,
                texture: None,
                aspect: 96.0 / 124.0,
                width: 96.0,
            }
            .show(ui);
        });
        let (rect, _) = ui.allocate_exact_size(vec2(300.0, 120.0), egui::Sense::hover());
        motif::corner_ticks(
            ui,
            Rect::from_center_size(rect.center(), vec2(200.0, 80.0)),
            6.0,
            10.0,
            Tokens::get(ui.ctx()).accent,
        );
        motif::scanning_dots(
            ui,
            Rect::from_center_size(rect.center(), vec2(180.0, 60.0)),
            10.0,
        );
    }
}
