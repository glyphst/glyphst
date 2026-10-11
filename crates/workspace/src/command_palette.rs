use crate::keybindings;
use glyphst_actions::menu;
use gpui::{
    Action, App, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseButton, Render,
    SharedString, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _,
    command::{Command as ComponentCommand, CommandItem, CommandState},
};

const PALETTE_WIDTH: f32 = 560.0;
const PALETTE_MAX_HEIGHT: f32 = 400.0;

pub enum CommandPaletteEvent {
    Dismissed,
    Confirmed(Box<dyn Action>),
}

struct Command {
    name: SharedString,
    action: Box<dyn Action>,
}

pub struct CommandPalette {
    state: Entity<CommandState>,
    previous_focus_handle: FocusHandle,
    commands: Vec<Command>,
}

impl CommandPalette {
    pub fn new(
        actions: Vec<Box<dyn Action>>,
        previous_focus_handle: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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

        let state = cx.new(|cx| CommandState::new(window, cx));
        let state_to_focus = state.clone();
        let palette = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            if palette.upgrade().is_some() {
                state_to_focus.update(cx, |state, cx| state.focus(window, cx));
            }
        });

        Self {
            state,
            previous_focus_handle,
            commands,
        }
    }

    fn keybinding_for_command(&self, command: &Command, window: &Window) -> Option<String> {
        let binding = window.highest_precedence_binding_for_action_in(
            command.action.as_ref(),
            &self.previous_focus_handle,
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

impl EventEmitter<CommandPaletteEvent> for CommandPalette {}

impl Focusable for CommandPalette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.focus_handle(cx)
    }
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self
            .commands
            .iter()
            .map(|command| {
                let name = command.name.clone();
                let keybinding = self.keybinding_for_command(command, window);
                let keyword: SharedString = humanize_action_name(command.action.name()).into();

                CommandItem::new()
                    .label(name.clone())
                    .keywords([keyword])
                    .child(move |_, cx| {
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .w_full()
                            .text_xs()
                            .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                            .when_some(keybinding.clone(), |this, keybinding| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .ml_3()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(keybinding),
                                )
                            })
                    })
            })
            .collect::<Vec<_>>();

        let palette = cx.weak_entity();
        let dismiss_palette = cx.weak_entity();
        let command = ComponentCommand::new(&self.state)
            .items(items)
            .placeholder(crate::fl!("command-palette-placeholder"))
            .empty(|_, _, cx| {
                div()
                    .w_full()
                    .py_6()
                    .text_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::fl!("command-palette-empty"))
            })
            .max_h(px(PALETTE_MAX_HEIGHT))
            .on_confirm(move |index, _, cx| {
                _ = palette.update(cx, |palette, cx| {
                    if let Some(command) = palette.commands.get(index.row) {
                        cx.emit(CommandPaletteEvent::Confirmed(command.action.boxed_clone()));
                    }
                });
            })
            .on_cancel(move |window, cx| {
                let dismiss_palette = dismiss_palette.clone();
                window.defer(cx, move |_, cx| {
                    _ = dismiss_palette.update(cx, |_, cx| {
                        cx.emit(CommandPaletteEvent::Dismissed);
                    });
                });
            });

        div()
            .key_context("CommandPalette Menu")
            .w(px(PALETTE_WIDTH))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            // Application menu bindings (e.g. Ctrl-N/Ctrl-P) drive the
            // command list through GPUI Kit's own selection actions.
            .on_action(|_: &menu::SelectNext, window, cx| {
                window.dispatch_action(Box::new(gpui_base::actions::SelectDown), cx);
            })
            .on_action(|_: &menu::SelectPrev, window, cx| {
                window.dispatch_action(Box::new(gpui_base::actions::SelectUp), cx);
            })
            .child(command)
    }
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
    use gpui::{KeyBinding, Subscription, TestAppContext, actions};
    use gpui_component::{IndexPath, Root};

    actions!(
        command_palette_test,
        [FirstCommand, SecondCommand, ThirdCommand]
    );

    struct CommandPaletteTestHost {
        palette: Entity<CommandPalette>,
        previous_focus_handle: FocusHandle,
        executed_second_command: bool,
        _palette_subscription: Subscription,
    }

    impl CommandPaletteTestHost {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let previous_focus_handle = cx.focus_handle();
            previous_focus_handle.focus(window, cx);
            let palette = cx.new(|cx| {
                CommandPalette::new(
                    vec![
                        Box::new(FirstCommand) as Box<dyn Action>,
                        Box::new(SecondCommand),
                        Box::new(ThirdCommand),
                    ],
                    previous_focus_handle.clone(),
                    window,
                    cx,
                )
            });
            let _palette_subscription =
                cx.subscribe_in(&palette, window, |this, _, event, window, cx| {
                    if let CommandPaletteEvent::Confirmed(action) = event {
                        let previous_focus_handle = this.previous_focus_handle.clone();
                        let action = action.boxed_clone();
                        window.defer(cx, move |window, cx| {
                            previous_focus_handle.focus(window, cx);
                            previous_focus_handle.dispatch_action(action.as_ref(), window, cx);
                        });
                    }
                });
            Self {
                palette,
                previous_focus_handle,
                executed_second_command: false,
                _palette_subscription,
            }
        }
    }

    impl Render for CommandPaletteTestHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .track_focus(&self.previous_focus_handle)
                .on_action(cx.listener(|this, _: &SecondCommand, _, cx| {
                    this.executed_second_command = true;
                    cx.notify();
                }))
                .child(self.palette.clone())
        }
    }

    #[gpui::test]
    fn configurable_item_navigation_actions_move_component_selection(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.bind_keys([
                KeyBinding::new("ctrl-n", menu::SelectNext, Some("Menu")),
                KeyBinding::new("ctrl-p", menu::SelectPrev, Some("Menu")),
            ]);
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let host = cx.new(|cx| CommandPaletteTestHost::new(window, cx));
            Root::new(host, window, cx)
        });
        let host = root.read_with(cx, |root, _| {
            root.view()
                .clone()
                .downcast::<CommandPaletteTestHost>()
                .unwrap()
        });
        let palette = host.read_with(cx, |host, _| host.palette.clone());

        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        palette.read_with(cx, |palette, cx| {
            assert_eq!(
                palette.state.read(cx).selected_index(),
                Some(IndexPath::new(0))
            );
        });

        cx.simulate_keystrokes("ctrl-n");
        palette.read_with(cx, |palette, cx| {
            assert_eq!(
                palette.state.read(cx).selected_index(),
                Some(IndexPath::new(1))
            );
        });

        cx.simulate_keystrokes("ctrl-p");
        palette.read_with(cx, |palette, cx| {
            assert_eq!(
                palette.state.read(cx).selected_index(),
                Some(IndexPath::new(0))
            );
        });

        cx.simulate_keystrokes("ctrl-n enter");
        cx.run_until_parked();
        host.read_with(cx, |host, _| assert!(host.executed_second_command));
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
