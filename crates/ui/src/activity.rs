use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use gpui::{
    Anchor, AnyElement, App, Context, Entity, Global, IntoElement, SharedString, Window, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, Size, StyledExt as _,
    button::{Button, ButtonVariants as _},
    group_box::{GroupBox, GroupBoxVariants as _},
    h_flex,
    popover::Popover,
    progress::{Progress, ProgressCircle},
    scroll::ScrollableElement as _,
    v_flex,
};

const MAX_ACTIVITY_NOTICES: usize = 50;

pub type UiCallback = Arc<dyn Fn(&mut Window, &mut App) + 'static>;
pub type ActivityCleanup = Arc<dyn Fn(&mut App) + 'static>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BackgroundTaskId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActivityNoticeId(u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActivityNoticeLevel {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

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

#[derive(Clone)]
pub struct ActivityNoticeEntry {
    pub level: ActivityNoticeLevel,
    pub title: SharedString,
    pub detail: SharedString,
    pub actions: Vec<CenterAction>,
    pub created_at: SystemTime,
    pub unread: bool,
    pub on_remove: Option<ActivityCleanup>,
}

impl ActivityNoticeEntry {
    pub fn new(
        level: ActivityNoticeLevel,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
    ) -> Self {
        Self {
            level,
            title: title.into(),
            detail: detail.into(),
            actions: Vec::new(),
            created_at: SystemTime::now(),
            unread: true,
            on_remove: None,
        }
    }

    pub fn on_remove(mut self, callback: impl Fn(&mut App) + 'static) -> Self {
        self.on_remove = Some(Arc::new(callback));
        self
    }
}

struct GlobalActivityCenter(Entity<ActivityCenter>);

impl Global for GlobalActivityCenter {}

/// App-wide registry for long-running work and retained feedback.
pub struct ActivityCenter {
    next_id: u64,
    tasks: Vec<(BackgroundTaskId, BackgroundTaskEntry)>,
    notices: Vec<(ActivityNoticeId, ActivityNoticeEntry)>,
}

pub fn init(cx: &mut App) {
    let center = cx.new(|_| ActivityCenter {
        next_id: 1,
        tasks: Vec::new(),
        notices: Vec::new(),
    });
    cx.set_global(GlobalActivityCenter(center));
}

impl ActivityCenter {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalActivityCenter>().0.clone()
    }

    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalActivityCenter>()
            .map(|center| center.0.clone())
    }

    pub fn active_task_count(&self) -> usize {
        self.tasks.len()
    }

    pub fn notice_count(&self) -> usize {
        self.notices.len()
    }

    pub fn unread_notice_count(&self) -> usize {
        self.notices
            .iter()
            .filter(|(_, notice)| notice.unread)
            .count()
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
        cx.notify();
    }

    pub fn add_notice(
        &mut self,
        notice: ActivityNoticeEntry,
        cx: &mut Context<Self>,
    ) -> ActivityNoticeId {
        let id = ActivityNoticeId(self.next_id());
        self.notices.push((id, notice));
        while self.notices.len() > MAX_ACTIVITY_NOTICES {
            let (_, mut removed) = self.notices.remove(0);
            if let Some(cleanup) = removed.on_remove.take() {
                cleanup(cx);
            }
        }
        cx.notify();
        id
    }

    pub fn finish_task(
        &mut self,
        task_id: BackgroundTaskId,
        notice: ActivityNoticeEntry,
        cx: &mut Context<Self>,
    ) -> ActivityNoticeId {
        self.tasks.retain(|(candidate, _)| *candidate != task_id);
        self.add_notice(notice, cx)
    }

    pub fn dismiss_notice(&mut self, id: ActivityNoticeId, cx: &mut Context<Self>) {
        let Some(index) = self
            .notices
            .iter()
            .position(|(candidate, _)| *candidate == id)
        else {
            return;
        };
        let (_, mut notice) = self.notices.remove(index);
        if let Some(cleanup) = notice.on_remove.take() {
            cleanup(cx);
        }
        cx.notify();
    }

    /// Remove a notice at the request of its owner without invoking the
    /// `on_remove` callback. Owner callbacks commonly update the same entity
    /// that initiated dismissal, so invoking them here would create a nested
    /// GPUI entity update.
    pub fn dismiss_notice_by_owner(&mut self, id: ActivityNoticeId, cx: &mut Context<Self>) {
        let Some(index) = self
            .notices
            .iter()
            .position(|(candidate, _)| *candidate == id)
        else {
            return;
        };
        self.notices.remove(index);
        cx.notify();
    }

    pub fn clear_notices(&mut self, cx: &mut Context<Self>) {
        let notices = std::mem::take(&mut self.notices);
        for (_, mut notice) in notices {
            if let Some(cleanup) = notice.on_remove.take() {
                cleanup(cx);
            }
        }
        cx.notify();
    }

    pub fn mark_all_read(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        for (_, notice) in &mut self.notices {
            if notice.unread {
                notice.unread = false;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    pub fn render_activity(&self, anchor: Anchor) -> Option<AnyElement> {
        if self.tasks.is_empty() && self.notices.is_empty() {
            return None;
        }

        let unread = self.unread_notice_count();
        let label: SharedString = if let Some((_, primary)) = self.tasks.last() {
            let additional = self.tasks.len().saturating_sub(1);
            if additional == 0 {
                primary.title.clone()
            } else {
                format!("{} + {additional} more", primary.title).into()
            }
        } else if unread > 0 {
            crate::fl!("activity-unread", count = unread as i64).into()
        } else {
            crate::fl!("activity-title").into()
        };
        let tasks = self.tasks.clone();
        let notices = self.notices.clone();

        Some(
            Popover::new("background-activity-popover")
                .anchor(anchor)
                .trigger(
                    Button::new("background-activity")
                        .when(!tasks.is_empty(), |button| {
                            button.icon(
                                ProgressCircle::new("background-activity-progress-circle")
                                    .loading(true),
                            )
                        })
                        .when(tasks.is_empty(), |button| button.icon(IconName::Bell))
                        .child(
                            div()
                                .min_w_0()
                                .max_w(px(240.0))
                                .truncate()
                                .child(label.clone()),
                        )
                        .when(unread > 0, |button| {
                            button.child(
                                div()
                                    .min_w_4()
                                    .h_4()
                                    .px_1()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .text_xs()
                                    .bg(gpui::red())
                                    .text_color(gpui::white())
                                    .child(unread.min(99).to_string()),
                            )
                        })
                        .tooltip(label)
                        .with_size(Size::XSmall)
                        .ghost(),
                )
                .on_open_change(|open, _, cx| {
                    if *open && let Some(center) = ActivityCenter::try_global(cx) {
                        center.update(cx, |center, cx| center.mark_all_read(cx));
                    }
                })
                .content(move |_, _, cx| {
                    let center = ActivityCenter::try_global(cx);
                    let mut content = v_flex()
                        .w_112()
                        .max_w_full()
                        .max_h_96()
                        .gap_2()
                        .overflow_y_scrollbar();

                    if !notices.is_empty() {
                        let clear_center = center.clone();
                        content = content
                            .child(
                                h_flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_semibold()
                                            .child(crate::fl!("activity-notifications")),
                                    )
                                    .when_some(clear_center, |header, center| {
                                        header.child(
                                            Button::new("activity-clear-all")
                                                .label(crate::fl!("activity-clear-all"))
                                                .with_size(Size::XSmall)
                                                .ghost()
                                                .on_click(move |_, _, cx| {
                                                    center.update(cx, |center, cx| {
                                                        center.clear_notices(cx)
                                                    });
                                                    cx.stop_propagation();
                                                }),
                                        )
                                    }),
                            )
                            .children(
                                notices
                                    .iter()
                                    .rev()
                                    .cloned()
                                    .map(|(id, notice)| render_notice_card(id, notice, cx)),
                            );
                    }

                    if !tasks.is_empty() {
                        content = content
                            .child(
                                div()
                                    .mt_1()
                                    .text_sm()
                                    .font_semibold()
                                    .child(crate::fl!("activity-in-progress")),
                            )
                            .children(
                                tasks
                                    .iter()
                                    .rev()
                                    .cloned()
                                    .map(|(id, task)| render_task_card(id, task, cx)),
                            );
                    }

                    content
                })
                .into_any_element(),
        )
    }
}

fn render_notice_card(id: ActivityNoticeId, notice: ActivityNoticeEntry, cx: &App) -> AnyElement {
    let icon = match notice.level {
        ActivityNoticeLevel::Info => Icon::new(IconName::Info).text_color(cx.theme().info),
        ActivityNoticeLevel::Success => {
            Icon::new(IconName::CircleCheck).text_color(cx.theme().success)
        }
        ActivityNoticeLevel::Warning => {
            Icon::new(IconName::TriangleAlert).text_color(cx.theme().warning)
        }
        ActivityNoticeLevel::Error => Icon::new(IconName::CircleX).text_color(cx.theme().danger),
    };
    let actions = notice
        .actions
        .into_iter()
        .enumerate()
        .map(|(index, action)| {
            let callback = action.callback;
            Button::new(format!("activity-notice-{}-{index}", id.0))
                .label(action.label)
                .with_size(Size::XSmall)
                .on_click(move |_, window, cx| {
                    callback(window, cx);
                    cx.stop_propagation();
                })
        })
        .collect::<Vec<_>>();
    let dismiss_center = ActivityCenter::try_global(cx);
    let age = activity_age(notice.created_at);

    GroupBox::new()
        .id(format!("activity-notice-{}", id.0))
        .outline()
        .title(
            h_flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    h_flex().min_w_0().flex_1().gap_2().child(icon).child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(cx.theme().foreground)
                            .child(notice.title),
                    ),
                )
                .when_some(dismiss_center, |title, center| {
                    title.child(
                        Button::new(format!("activity-notice-dismiss-{}", id.0))
                            .icon(IconName::Close)
                            .with_size(Size::XSmall)
                            .ghost()
                            .on_click(move |_, _, cx| {
                                center.update(cx, |center, cx| center.dismiss_notice(id, cx));
                                cx.stop_propagation();
                            }),
                    )
                }),
        )
        .child(
            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(notice.detail),
                )
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(age),
                        )
                        .child(h_flex().items_center().gap_1().children(actions)),
                ),
        )
        .into_any_element()
}

fn activity_age(created_at: SystemTime) -> String {
    let elapsed = SystemTime::now()
        .duration_since(created_at)
        .unwrap_or(Duration::ZERO);
    if elapsed < Duration::from_secs(60) {
        crate::fl!("activity-just-now")
    } else if elapsed < Duration::from_secs(60 * 60) {
        crate::fl!(
            "activity-minutes-ago",
            count = elapsed.as_secs() as i64 / 60
        )
    } else {
        crate::fl!(
            "activity-hours-ago",
            count = elapsed.as_secs() as i64 / 3600
        )
    }
}

fn render_task_card(id: BackgroundTaskId, task: BackgroundTaskEntry, cx: &App) -> AnyElement {
    let actions = task
        .actions
        .into_iter()
        .enumerate()
        .map(|(index, action)| {
            let callback = action.callback;
            Button::new(format!("background-task-{}-{index}", id.0))
                .label(action.label)
                .with_size(Size::XSmall)
                .on_click(move |_, window, cx| {
                    callback(window, cx);
                    cx.stop_propagation();
                })
        })
        .collect::<Vec<_>>();

    GroupBox::new()
        .id(format!("background-task-{}", id.0))
        .outline()
        .title(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_color(cx.theme().foreground)
                        .child(task.title),
                )
                .child(div().flex().items_center().gap_1().children(actions)),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
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
                        Progress::new(format!("background-task-progress-{}", id.0))
                            .with_size(Size::XSmall)
                            .loading(progress.indeterminate)
                            .value(fraction * 100.0),
                    )
                    .child(
                        div()
                            .flex_none()
                            .min_w_12()
                            .text_right()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if progress.indeterminate {
                                crate::fl!("activity-working")
                            } else {
                                format!("{}%", crate::format_user_float((fraction * 100.0) as f64))
                            }),
                    ),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::{cell::Cell, rc::Rc};

    #[gpui::test]
    fn tasks_can_be_added_updated_and_removed(cx: &mut TestAppContext) {
        cx.update(init);
        let center = cx.update(|cx| ActivityCenter::global(cx));
        center.update(cx, |center, cx| {
            let task = center.add_task(BackgroundTaskEntry::new("Work", "Queued"), cx);
            assert_eq!(center.active_task_count(), 1);

            center.update_task(task, |task| task.detail = "Running".into(), cx);
            assert_eq!(center.tasks[0].1.detail, "Running");

            center.remove_task(task, cx);
            assert_eq!(center.active_task_count(), 0);
        });
    }

    #[gpui::test]
    fn notices_are_bounded_and_marked_read(cx: &mut TestAppContext) {
        cx.update(init);
        let center = cx.update(|cx| ActivityCenter::global(cx));
        center.update(cx, |center, cx| {
            for index in 0..=MAX_ACTIVITY_NOTICES {
                center.add_notice(
                    ActivityNoticeEntry::new(
                        ActivityNoticeLevel::Info,
                        format!("Notice {index}"),
                        "Detail",
                    ),
                    cx,
                );
            }
            assert_eq!(center.notice_count(), MAX_ACTIVITY_NOTICES);
            assert_eq!(center.unread_notice_count(), MAX_ACTIVITY_NOTICES);
            assert_eq!(center.notices[0].1.title, "Notice 1");

            center.mark_all_read(cx);
            assert_eq!(center.unread_notice_count(), 0);
        });
    }

    #[gpui::test]
    fn finishing_a_task_moves_it_to_notice_history(cx: &mut TestAppContext) {
        cx.update(init);
        let center = cx.update(|cx| ActivityCenter::global(cx));
        center.update(cx, |center, cx| {
            let task = center.add_task(BackgroundTaskEntry::new("Work", "Running"), cx);
            center.finish_task(
                task,
                ActivityNoticeEntry::new(ActivityNoticeLevel::Success, "Done", "Finished"),
                cx,
            );
            assert_eq!(center.active_task_count(), 0);
            assert_eq!(center.notice_count(), 1);
        });
    }

    #[gpui::test]
    fn owner_dismissal_does_not_reenter_cleanup(cx: &mut TestAppContext) {
        cx.update(init);
        let center = cx.update(|cx| ActivityCenter::global(cx));
        let cleaned = Rc::new(Cell::new(false));
        let cleanup_flag = cleaned.clone();
        center.update(cx, |center, cx| {
            let notice = center.add_notice(
                ActivityNoticeEntry::new(ActivityNoticeLevel::Info, "Done", "Detail")
                    .on_remove(move |_| cleanup_flag.set(true)),
                cx,
            );
            center.dismiss_notice_by_owner(notice, cx);
        });

        assert!(!cleaned.get());
        assert_eq!(cx.update(|cx| center.read(cx).notice_count()), 0);
    }
}
