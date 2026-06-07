use std::ops::Range;

use crate::picker::{Picker, PickerDelegate, fuzzy_match};
use crate::theme::Theme;
use gpui::{
    Action, AnyElement, Context, FocusHandle, FontWeight, HighlightStyle, SharedString, StyledText,
    Window, div, prelude::*, rgb,
};

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
                name: humanize_action_name(action.name()).into(),
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
        "CommandPalette"
    }

    fn placeholder(&self) -> SharedString {
        "Execute a command…".into()
    }

    fn empty_message(&self) -> SharedString {
        "No commands found".into()
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
        theme: Theme,
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
                        color: Some(rgb(theme.match_highlight).into()),
                        font_weight: Some(FontWeight::BOLD),
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
                        .text_color(rgb(theme.text_muted))
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
        match (last_char, char) {
            (Some(':'), ':') => continue,
            (Some(last_char), char) if last_char.is_whitespace() && char.is_whitespace() => {
                continue;
            }
            _ => {
                last_char = Some(char);
            }
        }
        result.push(char);
    }

    result
}

fn humanize_action_name(name: &str) -> String {
    let capacity = name.len() + name.chars().filter(|ch| ch.is_uppercase()).count();
    let mut result = String::with_capacity(capacity);
    for char in name.chars() {
        if char == ':' {
            if result.ends_with(':') {
                result.push(' ');
            } else {
                result.push(':');
            }
        } else if char == '_' {
            result.push(' ');
        } else if char.is_uppercase() {
            if !result.ends_with(' ') {
                result.push(' ');
            }
            result.extend(char.to_lowercase());
        } else {
            result.push(char);
        }
    }
    result
}
