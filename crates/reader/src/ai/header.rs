//! The panel's header, after Zed's agent panel: the chat's title (pulsing
//! while a model names it) and buttons for a new chat, the history and
//! more; and the history of saved chats, grouped by day and searchable.

use egui::{Align2, Rect, Sense, vec2};
use glyphst_library::current_timestamp;
use glyphst_ui::{
    menu as ui_menu, motif,
    pane::{Action, Header},
};

use super::{Assistant, Panel, PanelAction};
use crate::{
    commands::Command,
    theme::{self, Tokens},
};

impl Assistant {
    /// The pane header: `assistant ── the chat's title` and new chat,
    /// history and more.
    pub(super) fn header_ui(
        &mut self,
        ui: &mut egui::Ui,
        panel: &Panel<'_>,
    ) -> Option<PanelAction> {
        let mut action = None;
        let agent = self.agent.as_deref().map(|id| {
            panel
                .agents
                .iter()
                .find(|agent| agent.id() == id)
                .map_or_else(|| id.to_string(), |agent| agent.name.clone())
        });
        let title = if self.show_sessions {
            format!("history · {} chats", self.sessions.len())
        } else {
            match &self.title {
                Some(title) => title.clone(),
                None if self.messages.is_empty() => match &agent {
                    Some(agent) => format!("new chat with {agent}"),
                    None => "new chat".into(),
                },
                None => self.messages[0].display_text(),
            }
        };
        let title = title.to_lowercase();
        let name = if agent.is_some() {
            "agent"
        } else {
            "assistant"
        };
        let response = Header::new(name)
            .subtitle(title.as_str())
            .action(Action::new("plus", "new chat"))
            .action(Action::new("rotate-ccw-clock", "history").selected(self.show_sessions))
            .action(Action::new("ellipsis", "more").selected(self.more_open))
            .show(ui);
        match response.clicked {
            Some(0) => action = Some(PanelAction::NewSession),
            Some(1) => {
                self.show_sessions = !self.show_sessions;
                self.sessions_query.clear();
            }
            Some(2) => self.more_open = !self.more_open,
            _ => {}
        }
        ui.add_space(10.0);
        if self.more_open
            && let Some(more) = response.actions.get(2)
            && let Some(chosen) = self.more_menu(ui, more.rect)
        {
            action = Some(chosen);
        }
        action
    }

    /// The `…` menu under its button.
    fn more_menu(&mut self, ui: &egui::Ui, button: Rect) -> Option<PanelAction> {
        let mut action = None;
        let area = egui::Area::new(egui::Id::new("assistant-more"))
            .order(egui::Order::Foreground)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(button.right_bottom() + vec2(0.0, 4.0))
            .show(ui.ctx(), |ui| {
                glyphst_ui::pane::popover(ui.ctx()).show(ui, |ui| {
                    ui.set_width(250.0);
                    ui_menu::style(ui);
                    let enabled = !self.messages.is_empty();
                    if ui
                        .add_enabled_ui(enabled, |ui| {
                            ui_menu::Item::new("summarize the chat so far")
                                .icon("list")
                                .show(ui)
                        })
                        .inner
                        .clicked()
                    {
                        self.compact_next = true;
                        self.status =
                            Some("The next message starts by summarizing the chat so far.".into());
                        self.more_open = false;
                    }
                    if ui_menu::Item::new("copy the mcp config for agents")
                        .icon("copy")
                        .show(ui)
                        .clicked()
                    {
                        action = Some(PanelAction::Command(Command::McpConfig));
                        self.more_open = false;
                    }
                    ui_menu::separator(ui);
                    if ui_menu::Item::new("ai settings…")
                        .icon("settings")
                        .show(ui)
                        .clicked()
                    {
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

    /// The saved chats, newest first, grouped by day under rule titles.
    pub(super) fn history_ui(&mut self, ui: &mut egui::Ui) -> Option<PanelAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        let field =
            glyphst_ui::field::TextField::search(&mut self.sessions_query, "search chats").show(ui);
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
                ui.spacing_mut().item_spacing.y = 0.0;
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
                                "no saved chats yet"
                            } else {
                                "no chat matches"
                            })
                            .font(theme::mono(11.0))
                            .color(tokens.faint),
                        );
                    });
                }
                let mut group = "";
                for session in sessions {
                    let day = if session.updated >= today {
                        "today"
                    } else if session.updated + 86_400 >= today {
                        "yesterday"
                    } else if session.updated + 7 * 86_400 >= today {
                        "this week"
                    } else {
                        "earlier"
                    };
                    if day != group {
                        group = day;
                        ui.add_space(6.0);
                        motif::rule_title(ui, day, None);
                        ui.add_space(2.0);
                    }
                    let current = session.id == self.session_id;
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                    if current {
                        ui.painter().rect_filled(rect, 0, tokens.accent_soft);
                    } else if response.hovered() {
                        ui.painter().rect_filled(rect, 0, tokens.hover);
                    }
                    let hovered = response.hovered() || response.contains_pointer();
                    let time = relative_time(session.updated, now);
                    let time_rect = glyphst_ui::text::line(
                        ui,
                        egui::pos2(rect.right() - 8.0, rect.top() + 13.0),
                        Align2::RIGHT_CENTER,
                        if hovered { String::new() } else { time },
                        theme::mono(10.0),
                        tokens.faint,
                        80.0,
                    );
                    let width = time_rect.left() - rect.left() - 16.0;
                    crate::widgets::row_text(
                        ui,
                        egui::pos2(rect.left() + 8.0, rect.top() + 13.0),
                        session.title.to_lowercase(),
                        theme::mono(12.0),
                        if current { tokens.text } else { tokens.text_2 },
                        width,
                    );
                    let detail = format!(
                        "{}{} msgs",
                        session
                            .agent
                            .as_ref()
                            .map(|agent| format!("{} · ", agent.agent))
                            .unwrap_or_default(),
                        session.messages.len(),
                    );
                    crate::widgets::row_text(
                        ui,
                        egui::pos2(rect.left() + 8.0, rect.top() + 29.0),
                        &detail,
                        theme::mono(10.0),
                        if current {
                            tokens.accent_text
                        } else {
                            tokens.faint
                        },
                        rect.width() - 16.0,
                    );
                    let mut deleted = false;
                    if hovered {
                        let at = Rect::from_center_size(
                            rect.right_center() - vec2(16.0, 0.0),
                            vec2(24.0, 24.0),
                        );
                        deleted = ui
                            .put(at, |ui: &mut egui::Ui| {
                                crate::icons::button(ui, "trash-2", 24.0, false, "delete this chat")
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
