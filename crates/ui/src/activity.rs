use std::sync::Arc;

use gpui::{
    Anchor, AnyElement, App, Context, Entity, Global, IntoElement, SharedString, Window, div,
    prelude::*,
};
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    group_box::{GroupBox, GroupBoxVariants as _},
    popover::Popover,
    progress::Progress,
    scroll::ScrollableElement as _,
    v_flex,
};

pub type UiCallback = Arc<dyn Fn(&mut Window, &mut App) + 'static>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BackgroundTaskId(u64);

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

struct GlobalActivityCenter(Entity<ActivityCenter>);

impl Global for GlobalActivityCenter {}

/// App-wide registry for long-running work.
///
/// Notifications themselves are window-scoped and owned by gpui-component's
/// `Root`; this registry only keeps task state that may be surfaced from any
/// Glyphst window.
pub struct ActivityCenter {
    next_id: u64,
    tasks: Vec<(BackgroundTaskId, BackgroundTaskEntry)>,
}

pub fn init(cx: &mut App) {
    let center = cx.new(|_| ActivityCenter {
        next_id: 1,
        tasks: Vec::new(),
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

    pub fn render_activity(&self) -> Option<AnyElement> {
        let (_, primary) = self.tasks.last()?.clone();
        let additional = self.tasks.len().saturating_sub(1);
        let label = if additional == 0 {
            primary.title
        } else {
            format!("{} + {additional} more", primary.title).into()
        };
        let tasks = self.tasks.clone();

        Some(
            Popover::new("background-activity-popover")
                .anchor(Anchor::BottomRight)
                .trigger(
                    Button::new("background-activity")
                        .icon(IconName::LoaderCircle)
                        .label(label)
                        .with_size(Size::XSmall)
                        .ghost(),
                )
                .content(move |_, _, cx| {
                    v_flex()
                        .w_112()
                        .max_w_full()
                        .max_h_96()
                        .gap_2()
                        .overflow_y_scrollbar()
                        .children(
                            tasks
                                .iter()
                                .rev()
                                .cloned()
                                .map(|(id, task)| render_task_card(id, task, cx)),
                        )
                })
                .into_any_element(),
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
                                "Working…".to_string()
                            } else {
                                format!("{:.0}%", fraction * 100.0)
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
}
