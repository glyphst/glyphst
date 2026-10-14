//! The panel's header, after Zed's agent panel: the chat's title (pulsing
//! while a model names it) and buttons for a new chat, the history and
//! more; and the history of saved chats, grouped by day and searchable.

use egui::{Align2, CornerRadius, Rect, Sense, Stroke, vec2};
use glyphst_library::current_timestamp;

use super::{Assistant, Panel, PanelAction};
use crate::{
    commands::Command,
    theme::{self, Tokens},
};

const HEADER: f32 = 34.0;

impl Assistant {
    pub(super) fn header_ui(
        &mut self,
        ui: &mut egui::Ui,
        panel: &Panel<'_>,
    ) -> Option<PanelAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER), Sense::hover());
        let agent = self.agent.as_deref().map(|id| {
            panel
                .agents
                .iter()
                .find(|agent| agent.id() == id)
                .map_or_else(|| id.to_string(), |agent| agent.name.clone())
        });
        let icon = if agent.is_some() {
            "terminal"
        } else {
            "sparkles"
        };
        crate::icons::paint(
            ui,
            Rect::from_center_size(rect.left_center() + vec2(9.0, 0.0), vec2(16.0, 16.0)),
            icon,
            15.0,
            tokens.accent,
        );
        let buttons = 3.0 * 28.0 + 4.0;
        let (title, naming) = match &self.title {
            Some(title) => (title.clone(), false),
            None if self.messages.is_empty() => (
                match &agent {
                    Some(agent) => format!("New chat with {agent}"),
                    None => "New chat".into(),
                },
                false,
            ),
            None => (
                self.messages[0].display_text(),
                self.title_job.is_some() || self.run.is_some(),
            ),
        };
        let color = if naming {
            Tokens::fade(tokens.text, crate::widgets::pulse(ui, 0.45, 0.85, 2.0))
        } else {
            tokens.text
        };
        crate::widgets::row_text(
            ui,
            rect.left_center() + vec2(26.0, 0.0),
            &title,
            theme::semibold(13.5),
            color,
            rect.width() - 30.0 - buttons,
        );
        let mut x = rect.right() - 28.0;
        let mut button = |ui: &mut egui::Ui, icon: &str, selected: bool, tooltip: &str| {
            let at = Rect::from_min_size(egui::pos2(x, rect.center().y - 14.0), vec2(28.0, 28.0));
            x -= 30.0;
            ui.put(at, |ui: &mut egui::Ui| {
                crate::icons::button(ui, icon, 28.0, selected, tooltip)
            })
        };
        let more = button(ui, "ellipsis", self.more_open, "More");
        if more.clicked() {
            self.more_open = !self.more_open;
        }
        if button(ui, "clock", self.show_sessions, "History").clicked() {
            self.show_sessions = !self.show_sessions;
            self.sessions_query.clear();
        }
        if button(ui, "message-square-plus", false, "New chat").clicked() {
            action = Some(PanelAction::NewSession);
        }
        ui.painter().hline(
            ui.max_rect().x_range(),
            rect.bottom() + 4.0,
            Stroke::new(1.0, tokens.hairline(0.08)),
        );
        ui.add_space(8.0);
        if self.more_open
            && let Some(chosen) = self.more_menu(ui, more.rect)
        {
            action = Some(chosen);
        }
        action
    }

    /// The `…` menu under its button.
    fn more_menu(&mut self, ui: &egui::Ui, button: Rect) -> Option<PanelAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        let area = egui::Area::new(egui::Id::new("assistant-more"))
            .order(egui::Order::Foreground)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(button.right_bottom() + vec2(0.0, 4.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(tokens.card)
                    .stroke(Stroke::new(1.0, tokens.hairline(0.14)))
                    .corner_radius(CornerRadius::same(10))
                    .shadow(tokens.popup_shadow())
                    .inner_margin(egui::Margin::same(5))
                    .show(ui, |ui| {
                        ui.set_width(250.0);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let item = |ui: &mut egui::Ui, icon: &str, label: &str, enabled: bool| {
                            let (rect, response) = ui.allocate_exact_size(
                                vec2(ui.available_width(), 30.0),
                                Sense::click(),
                            );
                            if enabled && response.hovered() {
                                ui.painter().rect_filled(
                                    rect,
                                    CornerRadius::same(7),
                                    tokens.ink(0.07),
                                );
                            }
                            let color = if enabled {
                                tokens.text
                            } else {
                                tokens.text_faint
                            };
                            crate::icons::paint(
                                ui,
                                Rect::from_center_size(
                                    rect.left_center() + vec2(16.0, 0.0),
                                    vec2(14.0, 14.0),
                                ),
                                icon,
                                14.0,
                                if enabled {
                                    tokens.text_muted
                                } else {
                                    tokens.text_faint
                                },
                            );
                            ui.painter().text(
                                rect.left_center() + vec2(32.0, 0.0),
                                Align2::LEFT_CENTER,
                                label,
                                theme::regular(13.0),
                                color,
                            );
                            enabled && response.clicked()
                        };
                        if item(
                            ui,
                            "list",
                            "Summarize the chat so far",
                            !self.messages.is_empty(),
                        ) {
                            self.compact_next = true;
                            self.status = Some(
                                "The next message starts by summarizing the chat so far.".into(),
                            );
                            self.more_open = false;
                        }
                        if item(ui, "copy", "Copy the MCP config for agents", true) {
                            action = Some(PanelAction::Command(Command::McpConfig));
                            self.more_open = false;
                        }
                        if item(ui, "settings", "AI settings…", true) {
                            action = Some(PanelAction::OpenAiSettings);
                            self.more_open = false;
                        }
                    });
            });
        if area.response.clicked_elsewhere()
            && !button.contains(ui.input(|i| i.pointer.interact_pos().unwrap_or_default()))
        {
            self.more_open = false;
        }
        action
    }

    /// The saved chats, newest first, grouped by day.
    pub(super) fn history_ui(&mut self, ui: &mut egui::Ui) -> Option<PanelAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        let field = crate::widgets::filter_field(ui, &mut self.sessions_query, "Search chats");
        if !field.has_focus()
            && self.sessions_query.is_empty()
            && ui.input(|i| i.key_pressed(egui::Key::Escape))
        {
            self.show_sessions = false;
        }
        ui.add_space(6.0);
        let query = self.sessions_query.trim().to_lowercase();
        let now = current_timestamp();
        let today = now - now % 86_400;
        egui::ScrollArea::vertical()
            .id_salt("assistant-history")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let sessions = self
                    .sessions
                    .iter()
                    .filter(|session| {
                        query.is_empty() || session.title.to_lowercase().contains(&query)
                    })
                    .collect::<Vec<_>>();
                if sessions.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new(if self.sessions.is_empty() {
                                "No saved chats yet"
                            } else {
                                "No chat matches"
                            })
                            .color(tokens.text_faint),
                        );
                    });
                }
                let mut group = "";
                for session in sessions {
                    let day = if session.updated >= today {
                        "Today"
                    } else if session.updated + 86_400 >= today {
                        "Yesterday"
                    } else if session.updated + 7 * 86_400 >= today {
                        "This week"
                    } else {
                        "Earlier"
                    };
                    if day != group {
                        group = day;
                        ui.add_space(6.0);
                        let (rect, _) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
                        ui.painter().text(
                            rect.left_center() + vec2(8.0, 0.0),
                            Align2::LEFT_CENTER,
                            day,
                            theme::medium(11.0),
                            tokens.text_faint,
                        );
                    }
                    let current = session.id == self.session_id;
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
                    if current {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(8), tokens.accent_soft);
                    } else if response.hovered() {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(8), tokens.ink(0.06));
                    }
                    let icon = if session.agent.is_some() {
                        "terminal"
                    } else {
                        "message-square"
                    };
                    crate::icons::paint(
                        ui,
                        Rect::from_center_size(
                            rect.left_center() + vec2(16.0, 0.0),
                            vec2(14.0, 14.0),
                        ),
                        icon,
                        14.0,
                        tokens.text_muted,
                    );
                    let width = rect.width() - 36.0 - if response.hovered() { 30.0 } else { 0.0 };
                    crate::widgets::row_text(
                        ui,
                        egui::pos2(rect.left() + 32.0, rect.top() + 14.0),
                        &session.title,
                        theme::regular(13.0),
                        if current {
                            tokens.accent_text
                        } else {
                            tokens.text
                        },
                        width,
                    );
                    let detail = format!(
                        "{} · {} messages{}",
                        relative_time(session.updated, now),
                        session.messages.len(),
                        session
                            .agent
                            .as_ref()
                            .map(|agent| format!(" · {}", agent.agent))
                            .unwrap_or_default()
                    );
                    crate::widgets::row_text(
                        ui,
                        egui::pos2(rect.left() + 32.0, rect.top() + 31.0),
                        &detail,
                        theme::regular(11.0),
                        tokens.text_faint,
                        width,
                    );
                    let mut deleted = false;
                    if response.hovered() || response.contains_pointer() {
                        let at = Rect::from_center_size(
                            rect.right_center() - vec2(18.0, 0.0),
                            vec2(26.0, 26.0),
                        );
                        deleted = ui
                            .put(at, |ui: &mut egui::Ui| {
                                crate::icons::button(ui, "trash-2", 26.0, false, "Delete this chat")
                            })
                            .clicked();
                    }
                    if deleted {
                        action = Some(PanelAction::DeleteSession(session.id));
                    } else if response.clicked() {
                        action = Some(PanelAction::LoadSession(session.id));
                    }
                }
            });
        action
    }
}

/// "just now", "5m ago", "3h ago", "2d ago", or the date.
pub(super) fn relative_time(then: u64, now: u64) -> String {
    let seconds = now.saturating_sub(then);
    match seconds {
        0..60 => "just now".into(),
        60..3_600 => format!("{}m ago", seconds / 60),
        3_600..86_400 => format!("{}h ago", seconds / 3_600),
        86_400..604_800 => format!("{}d ago", seconds / 86_400),
        _ => {
            let days = then / 86_400;
            let (year, month, day) = civil_date(days as i64);
            format!("{year}-{month:02}-{day:02}")
        }
    }
}

/// The date of a day counted from 1970-01-01 (Howard Hinnant's algorithm).
pub(super) fn civil_date(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
