use std::ops::Range;

use crate::{
    keybindings,
    picker::{Picker, PickerDelegate, fuzzy_match},
    style::TextSize,
};
use gpui::{
    Action, AnyElement, Context, FocusHandle, HighlightStyle, SharedString, StyledText, Window,
    div, prelude::*,
};
use gpui_component::ThemeColor;

const MAX_VISIBLE_MATCHES: usize = 10;
const MATCH_ROW_HEIGHT: f32 = 40.0;

pub type CommandPalette = Picker<CommandPaletteDelegate>;

struct Command {
    name: SharedString,
    action: Box<dyn Action>,
}

struct CommandMatch {
    command_ix: usize,
    highlights: Vec<Range<usize>>,
    score: usize,
}

pub struct CommandPaletteDelegate {
    commands: Vec<Command>,
    matches: Vec<CommandMatch>,
}

impl CommandPaletteDelegate {
    pub fn new(actions: Vec<Box<dyn Action>>) -> Self {
        let mut commands = actions
            .into_iter()
            .map(|action| Command {
                name: keybindings::localized_action_label(action.name())
                    .unwrap_or_else(|| humanize_action_name(action.name()))
                    .into(),
                action,
            })
            .collect::<Vec<_>>();

        commands.sort_by(|left, right| left.name.as_ref().cmp(right.name.as_ref()));

        Self {
            commands,
            matches: Vec::new(),
        }
    }

    fn keybinding_for_command(
        &self,
        command: &Command,
        previous_focus_handle: &FocusHandle,
        window: &Window,
    ) -> Option<String> {
        let binding = window.highest_precedence_binding_for_action_in(
            command.action.as_ref(),
            previous_focus_handle,
        )?;
        Some(
            binding
                .keystrokes()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" "),
        )
    }
}

impl PickerDelegate for CommandPaletteDelegate {
    fn key_context(&self) -> &'static str {
        "CommandPalette Menu TextInput"
    }

    fn placeholder(&self) -> SharedString {
        crate::fl!("command-palette-placeholder").into()
    }

    fn empty_message(&self) -> SharedString {
        crate::fl!("command-palette-empty").into()
    }

    fn width(&self) -> f32 {
        560.0
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

    fn match_id(&self, match_ix: usize) -> gpui::ElementId {
        self.matches
            .get(match_ix)
            .and_then(|command_match| self.commands.get(command_match.command_ix))
            .map(|command| format!("command-palette-{}", command.action.name()).into())
            .unwrap_or_else(|| ("command-palette", match_ix).into())
    }

    fn update_matches(&mut self, query: &str) {
        let query = normalize_action_query(query);
        let mut matches = self
            .commands
            .iter()
            .enumerate()
            .filter_map(|(command_ix, command)| {
                fuzzy_match(command.name.as_ref(), &query).map(|(highlights, score)| CommandMatch {
                    command_ix,
                    highlights,
                    score,
                })
            })
            .collect::<Vec<_>>();

        matches.sort_by(|left, right| {
            left.score.cmp(&right.score).then_with(|| {
                self.commands[left.command_ix]
                    .name
                    .as_ref()
                    .cmp(self.commands[right.command_ix].name.as_ref())
            })
        });

        self.matches = matches;
    }

    fn render_match(
        &self,
        match_ix: usize,
        theme: ThemeColor,
        previous_focus_handle: &FocusHandle,
        window: &Window,
    ) -> AnyElement {
        let Some(command_match) = self.matches.get(match_ix) else {
            return div().into_any_element();
        };
        let command = &self.commands[command_match.command_ix];
        let command_name = command.name.clone();
        let keybinding = self.keybinding_for_command(command, previous_focus_handle, window);
        let highlights = command_match
            .highlights
            .iter()
            .cloned()
            .map(|range| {
                (
                    range,
                    HighlightStyle {
                        color: Some(theme.warning),
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
                    .child(StyledText::new(command_name).with_highlights(highlights)),
            )
            .when_some(keybinding, |this, keybinding| {
                this.child(
                    div()
                        .flex_none()
                        .ml_3()
                        .text_color(theme.muted_foreground)
                        .child(keybinding),
                )
            })
            .into_any_element()
    }

    fn confirm(
        &mut self,
        match_ix: usize,
        previous_focus_handle: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        let Some(command_ix) = self
            .matches
            .get(match_ix)
            .map(|command_match| command_match.command_ix)
        else {
            return;
        };

        let action = self.commands[command_ix].action.boxed_clone();
        let previous_focus_handle = previous_focus_handle.clone();
        window.defer(cx, move |window, cx| {
            previous_focus_handle.focus(window, cx);
            previous_focus_handle.dispatch_action(action.as_ref(), window, cx);
        });
    }
}

fn normalize_action_query(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut last_char = None;

    for char in input.trim().chars() {
        let normalized_char = if char == '_' { ' ' } else { char };
        match (last_char, normalized_char) {
            (Some(':'), ':') => continue,
            (Some(last_char), char) if last_char.is_whitespace() && char.is_whitespace() => {
                continue;
            }
            _ => {
                last_char = Some(normalized_char);
            }
        }
        result.push(normalized_char);
    }

    result
}

fn humanize_action_name(name: &str) -> String {
    let chars = name.chars().collect::<Vec<_>>();
    let capacity = name.len() + chars.iter().filter(|ch| ch.is_uppercase()).count();
    let mut result = String::with_capacity(capacity);
    let mut index = 0;

    while index < chars.len() {
        let char = chars[index];
        if char == ':' {
            if result.ends_with(':') {
                result.push(' ');
            } else {
                result.push(':');
            }
            index += 1;
        } else if char == '_' {
            result.push(' ');
            index += 1;
        } else if char.is_uppercase() {
            let start = index;
            index += 1;
            while chars
                .get(index)
                .is_some_and(|next_char| next_char.is_uppercase())
            {
                index += 1;
            }

            let uppercase_run = &chars[start..index];
            if uppercase_run.len() > 1 {
                let split_before_last = chars
                    .get(index)
                    .is_some_and(|next_char| next_char.is_lowercase());
                let acronym_end = if split_before_last {
                    uppercase_run.len() - 1
                } else {
                    uppercase_run.len()
                };

                if acronym_end > 0 {
                    if !result.ends_with(' ') {
                        result.push(' ');
                    }
                    result.extend(&uppercase_run[..acronym_end]);
                }

                if split_before_last {
                    if !result.ends_with(' ') {
                        result.push(' ');
                    }
                    result.extend(uppercase_run[acronym_end].to_lowercase());
                }
            } else {
                if !result.ends_with(' ') {
                    result.push(' ');
                }
                result.extend(char.to_lowercase());
            }
        } else {
            result.push(char);
            index += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_action_queries_like_zed() {
        assert_eq!(
            normalize_action_query("terminal_panel::Toggle"),
            "terminal panel:Toggle"
        );
        assert_eq!(
            normalize_action_query("editor:    backspace"),
            "editor: backspace"
        );
    }

    #[test]
    fn humanizes_action_names_like_zed() {
        assert_eq!(
            humanize_action_name("editor::GoToDefinition"),
            "editor: go to definition"
        );
        assert_eq!(humanize_action_name("editor::OpenURL"), "editor: open URL");
        assert_eq!(
            humanize_action_name("editor::OpenURLParser"),
            "editor: open URL parser"
        );
    }
}
