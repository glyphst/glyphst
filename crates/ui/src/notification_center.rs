use std::{sync::Arc, time::Duration};

use crate::{
    icons::{Icon, IconName},
    settings::ui_rems,
    theme::Theme,
};
use gpui::{
    AnyElement, App, Context, Entity, Global, IntoElement, MouseButton, SharedString, Window, div,
    prelude::*, px, relative, rgb, rgba,
};

const ACTIVITY_ICON_SIZE: f32 = 14.0;
const ACTIVITY_BUTTON_HEIGHT: f32 = 22.0;
const CENTER_WIDTH: f32 = 420.0;
const NOTICE_WIDTH: f32 = 380.0;
pub const SUCCESS_NOTICE_DURATION: Duration = Duration::from_secs(5);

pub type UiCallback = Arc<dyn Fn(&mut Window, &mut App) + 'static>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BackgroundTaskId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NotificationId(u64);

#[derive(Clone)]
pub struct CenterAction {
    pub label: SharedString,
    pub callback: UiCallback,
}

impl CenterAction {
    pub fn new(
        label: impl Into<SharedString>,
        callback: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            callback: Arc::new(callback),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TaskProgress {
    pub fraction: f32,
    pub indeterminate: bool,
}

#[derive(Clone)]
pub struct BackgroundTaskEntry {
    pub title: SharedString,
    pub detail: SharedString,
    pub progress: Option<TaskProgress>,
    pub actions: Vec<CenterAction>,
}

impl BackgroundTaskEntry {
    pub fn new(title: impl Into<SharedString>, detail: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            detail: detail.into(),
            progress: None,
            actions: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationLevel {
    Success,
    Error,
    Info,
}

#[derive(Clone)]
pub struct NotificationEntry {
    pub level: NotificationLevel,
    pub title: SharedString,
    pub detail: Option<SharedString>,
    pub actions: Vec<CenterAction>,
}

impl NotificationEntry {
    pub fn new(level: NotificationLevel, title: impl Into<SharedString>) -> Self {
        Self {
            level,
            title: title.into(),
            detail: None,
            actions: Vec::new(),
        }
    }
}

struct GlobalNotificationCenter(Entity<NotificationCenter>);

impl Global for GlobalNotificationCenter {}

pub struct NotificationCenter {
    next_id: u64,
    tasks: Vec<(BackgroundTaskId, BackgroundTaskEntry)>,
    notifications: Vec<(NotificationId, NotificationEntry)>,
    activity_open: bool,
}

pub fn init(cx: &mut App) {
    let center = cx.new(|_| NotificationCenter {
        next_id: 1,
        tasks: Vec::new(),
        notifications: Vec::new(),
        activity_open: false,
    });
    cx.set_global(GlobalNotificationCenter(center));
}

impl NotificationCenter {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalNotificationCenter>().0.clone()
    }

    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalNotificationCenter>()
            .map(|center| center.0.clone())
    }

    pub fn active_task_count(&self) -> usize {
        self.tasks.len()
    }

    pub fn notification_count(&self) -> usize {
        self.notifications.len()
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    pub fn add_task(
        &mut self,
        task: BackgroundTaskEntry,
        cx: &mut Context<Self>,
    ) -> BackgroundTaskId {
        let id = BackgroundTaskId(self.next_id());
        self.tasks.push((id, task));
        cx.notify();
        id
    }

    pub fn update_task(
        &mut self,
        id: BackgroundTaskId,
        update: impl FnOnce(&mut BackgroundTaskEntry),
        cx: &mut Context<Self>,
    ) {
        if let Some((_, task)) = self
            .tasks
            .iter_mut()
            .find(|(candidate, _)| *candidate == id)
        {
            update(task);
            cx.notify();
        }
    }

    pub fn remove_task(&mut self, id: BackgroundTaskId, cx: &mut Context<Self>) {
        self.tasks.retain(|(candidate, _)| *candidate != id);
        if self.tasks.is_empty() {
            self.activity_open = false;
        }
        cx.notify();
    }

    pub fn show_notification(
        &mut self,
        notification: NotificationEntry,
        auto_hide: Option<Duration>,
        cx: &mut Context<Self>,
    ) -> NotificationId {
        let id = NotificationId(self.next_id());
        self.notifications.push((id, notification));
        if let Some(duration) = auto_hide {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(duration).await;
                _ = this.update(cx, |this, cx| this.dismiss_notification(id, cx));
            })
            .detach();
        }
        cx.notify();
        id
    }

    pub fn dismiss_notification(&mut self, id: NotificationId, cx: &mut Context<Self>) {
        self.notifications.retain(|(candidate, _)| *candidate != id);
        cx.notify();
    }

    pub fn render_activity(&mut self, theme: Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (_, primary) = self.tasks.last()?.clone();
        let additional = self.tasks.len().saturating_sub(1);
        let label = if additional == 0 {
            primary.title.to_string()
        } else {
            format!("{} + {additional} more", primary.title)
        };
        let popover = self.activity_open.then(|| {
            let tasks = self.tasks.clone();
            let cards = tasks
                .iter()
                .rev()
                .cloned()
                .map(|(id, task)| self.render_task_card(id, task, theme, cx))
                .collect::<Vec<_>>();
            div()
                .id("notification-center-activity-popover")
                .occlude()
                .absolute()
                .right_0()
                .bottom(px(ACTIVITY_BUTTON_HEIGHT + 4.0))
                .mb_2()
                .w(px(CENTER_WIDTH))
                .max_h(px(420.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .rounded(px(8.0))
                .border_1()
                .border_color(rgb(theme.border))
                .bg(rgb(theme.surface_background))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .children(cards)
        });
        Some(
            div()
                .relative()
                .flex()
                .items_center()
                .child(
                    div()
                        .id("notification-center-activity")
                        .flex()
                        .items_center()
                        .gap_1()
                        .h(ui_rems(ACTIVITY_BUTTON_HEIGHT))
                        .max_w(px(260.0))
                        .px_2()
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .text_color(rgb(if self.activity_open {
                            theme.accent
                        } else {
                            theme.footer_text
                        }))
                        .bg(if self.activity_open {
                            rgb(theme.element_selected)
                        } else {
                            rgba(0x00000000)
                        })
                        .hover(move |style| style.bg(rgb(theme.element_hover)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.activity_open = !this.activity_open;
                                cx.notify();
                                cx.stop_propagation();
                            }),
                        )
                        .child(Icon::new(
                            IconName::Circle,
                            ui_rems(ACTIVITY_ICON_SIZE),
                            rgb(theme.accent),
                        ))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(ui_rems(11.0))
                                .child(label),
                        ),
                )
                .children(popover)
                .into_any_element(),
        )
    }

    pub fn render_notifications(
        &mut self,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.notifications.is_empty() {
            return None;
        }
        let notifications = self.notifications.clone();
        let cards = notifications
            .iter()
            .rev()
            .cloned()
            .map(|(id, notification)| self.render_notification(id, notification, theme, cx))
            .collect::<Vec<_>>();
        Some(
            div()
                .w(px(NOTICE_WIDTH))
                .max_w_full()
                .flex()
                .flex_col()
                .gap_2()
                .children(cards)
                .into_any_element(),
        )
    }

    fn render_task_card(
        &mut self,
        id: BackgroundTaskId,
        task: BackgroundTaskEntry,
        theme: Theme,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let actions = task
            .actions
            .into_iter()
            .enumerate()
            .map(|(index, action)| {
                let callback = action.callback;
                div()
                    .id(SharedString::from(format!(
                        "background-task-{}-{index}",
                        id.0
                    )))
                    .h(ui_rems(22.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(theme.control_border))
                    .bg(rgb(theme.control_background))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(theme.control_hover)))
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        callback(window, cx);
                        cx.stop_propagation();
                    })
                    .child(action.label)
            })
            .collect::<Vec<_>>();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div().min_w_0().flex_1().child(
                            div()
                                .truncate()
                                .text_color(rgb(theme.text))
                                .child(task.title),
                        ),
                    )
                    .child(div().flex().items_center().gap_1().children(actions)),
            )
            .child(
                div()
                    .truncate()
                    .text_size(ui_rems(12.0))
                    .text_color(rgb(theme.text_muted))
                    .child(task.detail),
            )
            .when_some(task.progress, |this, progress| {
                let fraction = if progress.fraction.is_finite() {
                    progress.fraction.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .h(px(4.0))
                                .overflow_hidden()
                                .rounded(px(2.0))
                                .bg(rgb(theme.border_variant))
                                .child(
                                    div()
                                        .h_full()
                                        .w(relative(fraction))
                                        .rounded(px(2.0))
                                        .bg(rgb(theme.accent)),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .min_w(px(48.0))
                                .text_align(gpui::TextAlign::Right)
                                .text_size(ui_rems(11.0))
                                .text_color(rgb(theme.text_muted))
                                .child(if progress.indeterminate {
                                    "Working…".to_string()
                                } else {
                                    format!("{:.0}%", fraction * 100.0)
                                }),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_notification(
        &mut self,
        id: NotificationId,
        notification: NotificationEntry,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let color = match notification.level {
            NotificationLevel::Success => theme.success,
            NotificationLevel::Error => theme.error,
            NotificationLevel::Info => theme.accent,
        };
        let actions = notification
            .actions
            .into_iter()
            .enumerate()
            .map(|(index, action)| {
                let callback = action.callback;
                div()
                    .id(SharedString::from(format!("notification-{}-{index}", id.0)))
                    .h(ui_rems(22.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(theme.control_border))
                    .bg(rgb(theme.control_background))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(theme.control_hover)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.dismiss_notification(id, cx);
                            callback(window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(action.label)
            })
            .collect::<Vec<_>>();
        div()
            .occlude()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(color))
            .bg(rgb(theme.surface_background))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .whitespace_normal()
                            .text_color(rgb(theme.text))
                            .child(notification.title),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("notification-{}-dismiss", id.0)))
                            .flex_none()
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.dismiss_notification(id, cx);
                                    cx.stop_propagation();
                                }),
                            )
                            .child(Icon::new(IconName::X, ui_rems(13.0), rgb(theme.text_muted))),
                    ),
            )
            .children(notification.detail.map(|detail| {
                div()
                    .min_w_0()
                    .whitespace_normal()
                    .text_size(ui_rems(12.0))
                    .text_color(rgb(theme.text_muted))
                    .child(detail)
            }))
            .when(!actions.is_empty(), |this| {
                this.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_end()
                        .gap_1()
                        .children(actions),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn tasks_and_notifications_are_removed_independently(cx: &mut TestAppContext) {
        cx.update(init);
        let center = cx.update(|cx| NotificationCenter::global(cx));
        center.update(cx, |center, cx| {
            let task = center.add_task(BackgroundTaskEntry::new("Work", "Running"), cx);
            let notice = center.show_notification(
                NotificationEntry::new(NotificationLevel::Success, "Done"),
                None,
                cx,
            );
            assert_eq!(center.tasks.len(), 1);
            assert_eq!(center.notifications.len(), 1);
            center.remove_task(task, cx);
            assert!(center.tasks.is_empty());
            assert_eq!(center.notifications.len(), 1);
            center.dismiss_notification(notice, cx);
            assert!(center.notifications.is_empty());
        });
    }

    #[gpui::test]
    fn transient_notifications_hide_after_their_duration(cx: &mut TestAppContext) {
        cx.update(init);
        let center = cx.update(|cx| NotificationCenter::global(cx));
        center.update(cx, |center, cx| {
            center.show_notification(
                NotificationEntry::new(NotificationLevel::Success, "Done"),
                Some(Duration::from_secs(5)),
                cx,
            );
        });
        assert_eq!(cx.read(|cx| center.read(cx).notification_count()), 1);

        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();

        assert_eq!(cx.read(|cx| center.read(cx).notification_count()), 0);
    }
}
