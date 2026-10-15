//! Settings rows (frame 07): a label in mono with a hint under it at the
//! left, the control at the right, rows split by hairlines. A section
//! starts with a heading and a prose description.

use egui::{Rect, Sense, Ui, vec2};

use crate::theme::{self, Tokens};

/// A section's heading and description (`Appearance` / `How Glyphst
/// looks.`).
pub fn heading(ui: &mut Ui, title: &str, description: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(
        egui::RichText::new(title)
            .font(theme::semibold(17.0))
            .color(t.text),
    );
    if !description.is_empty() {
        ui.add_space(2.0);
        ui.add(
            egui::Label::new(
                egui::RichText::new(description)
                    .font(theme::regular(13.0))
                    .color(t.muted),
            )
            .wrap(),
        );
    }
    ui.add_space(14.0);
}

/// A group of rows, optionally titled with a rule title; `add` adds them
/// through [`Group`].
pub fn group<R>(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Group<'_>) -> R) -> R {
    if !title.is_empty() {
        ui.add_space(10.0);
        crate::motif::rule_title(ui, &title.to_lowercase(), None);
        ui.add_space(2.0);
    }
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut group = Group { ui, rows: 0 };
        let inner = add(&mut group);
        // A hairline closes the group.
        group.rule();
        inner
    })
    .inner
}

/// The rows of a [`group`].
pub struct Group<'a> {
    ui: &'a mut Ui,
    rows: usize,
}

impl Group<'_> {
    pub fn ctx(&self) -> &egui::Context {
        self.ui.ctx()
    }

    fn rule(&mut self) {
        if self.rows == 0 {
            return;
        }
        let t = Tokens::get(self.ui.ctx());
        let (rect, _) = self
            .ui
            .allocate_exact_size(vec2(self.ui.available_width(), 1.0), Sense::hover());
        self.ui
            .painter()
            .hline(rect.x_range(), rect.center().y, t.hairline());
    }

    /// A hairline before every row but the first.
    fn next_row(&mut self) {
        self.rule();
        self.rows += 1;
    }

    /// A setting: its name and what it does on the left, `control` at the
    /// right (laid out right to left, so add the rightmost first).
    pub fn row<R>(
        &mut self,
        label: &str,
        description: Option<&str>,
        control: impl FnOnce(&mut Ui) -> R,
    ) -> R {
        let t = Tokens::get(self.ui.ctx());
        let description = description.map(|text| {
            egui::RichText::new(text)
                .font(theme::regular(12.0))
                .color(t.muted)
        });
        self.row_with(label, description, control)
    }

    /// [`Self::row`] with a description styled by the caller (a warning
    /// colour, say).
    pub fn row_with<R>(
        &mut self,
        label: &str,
        description: Option<egui::RichText>,
        control: impl FnOnce(&mut Ui) -> R,
    ) -> R {
        self.next_row();
        let t = Tokens::get(self.ui.ctx());
        let ui = &mut *self.ui;
        let width = ui.available_width();
        let label_width = (width * 0.5).clamp(140.0, 360.0);
        let wrap = Some(egui::TextWrapMode::Wrap);
        let galley = |ui: &Ui, text: egui::RichText| {
            egui::WidgetText::from(text).into_galley(
                ui,
                wrap,
                label_width,
                egui::FontSelection::Default,
            )
        };
        let title = galley(
            ui,
            egui::RichText::new(label.to_lowercase())
                .font(theme::mono(12.0))
                .color(t.text),
        );
        let description = description.map(|text| galley(ui, text));
        let text_height = title.size().y
            + description
                .as_ref()
                .map_or(0.0, |galley| 4.0 + galley.size().y);
        // The label and the control are centred on the row's height.
        let height = (text_height + t.row(22.0)).max(t.row(44.0));
        let top_left = ui.cursor().min;
        let rect = Rect::from_min_size(top_left, vec2(width, height));
        let mut y = rect.center().y - text_height / 2.0;
        let title_height = title.size().y;
        ui.painter()
            .galley(egui::pos2(rect.left(), y), title, t.text);
        y += title_height + 4.0;
        if let Some(description) = description {
            ui.painter()
                .galley(egui::pos2(rect.left(), y), description, t.muted);
        }
        let controls = Rect::from_min_max(
            egui::pos2(rect.left() + label_width + 12.0, rect.top()),
            rect.max,
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(controls)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        child.spacing_mut().item_spacing = vec2(8.0, 4.0);
        let inner = control(&mut child);
        let used = child.min_rect().union(rect);
        ui.allocate_rect(
            Rect::from_min_max(rect.min, egui::pos2(rect.right(), used.bottom())),
            Sense::hover(),
        );
        inner
    }

    /// A row the width of the group, for galleries, lists and notes.
    pub fn full<R>(&mut self, add: impl FnOnce(&mut Ui) -> R) -> R {
        self.next_row();
        self.ui
            .vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.add_space(10.0);
                let inner = add(ui);
                ui.add_space(10.0);
                inner
            })
            .inner
    }

    /// A muted line of explanation.
    pub fn note(&mut self, text: &str) {
        self.full(|ui| {
            let t = Tokens::get(ui.ctx());
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .font(theme::regular(12.0))
                        .color(t.muted),
                )
                .wrap(),
            );
        });
    }

    /// A row that goes somewhere else: an icon, `label` and `→`; returns
    /// whether it was clicked.
    pub fn link(&mut self, icon: &str, label: &str) -> bool {
        self.next_row();
        crate::row::ListRow::new(label)
            .icon(icon)
            .meta("→")
            .indent(0.0)
            .height(40.0)
            .show(self.ui)
            .clicked()
    }
}

/// The look of egui's own controls on a form (combo boxes, drag values,
/// text edits): framed on `raised`, hairline edges, an accent focus ring.
pub fn form_style(ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let style = ui.style_mut();
    let radius = t.ctl_radius();
    let widgets = &mut style.visuals.widgets;
    widgets.inactive.bg_fill = t.raised;
    widgets.inactive.weak_bg_fill = t.raised;
    widgets.inactive.bg_stroke = t.edge();
    widgets.hovered.weak_bg_fill = t.hover;
    widgets.hovered.bg_stroke = egui::Stroke::new(1.0, t.faint);
    widgets.active.weak_bg_fill = t.rule_strong;
    widgets.active.bg_stroke = egui::Stroke::new(1.0, t.accent);
    widgets.open.weak_bg_fill = t.hover;
    widgets.open.bg_stroke = egui::Stroke::new(1.0, t.accent);
    for state in [
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        state.corner_radius = radius;
    }
    style.visuals.selection.stroke = egui::Stroke::new(1.0, t.accent);
    style.spacing.interact_size.y = 28.0;
    style.spacing.button_padding = vec2(10.0, 5.0);
}
