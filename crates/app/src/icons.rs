use crate::{
    settings::ui_rems,
    style::{CONTROL_ICON_SIZE, CONTROL_SIZE, RADIUS_MD},
};
use gpui::{
    Div, IntoElement, ParentElement, Rems, RenderOnce, Rgba, SharedString, Styled as _, div,
    prelude::*, px, svg,
};

#[derive(Clone, Copy)]
pub(crate) enum IconName {
    AppWindow,
    ArrowUpRight,
    Bot,
    CaseSensitive,
    Check,
    ChevronDown,
    ChevronUp,
    Circle,
    Copy,
    Delete,
    File,
    Info,
    LayoutGrid,
    Menu,
    NotebookTabs,
    PanelLeft,
    Pencil,
    Play,
    Plus,
    RotateCcw,
    Search,
    Stop,
    TableOfContents,
    TriangleAlert,
    X,
}

impl IconName {
    fn path(self) -> SharedString {
        let name = match self {
            Self::AppWindow => "app-window",
            Self::ArrowUpRight => "arrow-up-right",
            Self::Bot => "bot",
            Self::CaseSensitive => "case-sensitive",
            Self::Check => "check",
            Self::ChevronDown => "chevron-down",
            Self::ChevronUp => "chevron-up",
            Self::Circle => "circle",
            Self::Copy => "copy",
            Self::Delete => "delete",
            Self::File => "file",
            Self::Info => "info",
            Self::LayoutGrid => "layout-grid",
            Self::Menu => "menu",
            Self::NotebookTabs => "notebook-tabs",
            Self::PanelLeft => "panel-left",
            Self::Pencil => "pencil",
            Self::Play => "play",
            Self::Plus => "plus",
            Self::RotateCcw => "rotate-ccw",
            Self::Search => "search",
            Self::Stop => "stop",
            Self::TableOfContents => "table-of-contents",
            Self::TriangleAlert => "triangle-alert",
            Self::X => "x",
        };

        format!("icons/{name}.svg").into()
    }
}

#[derive(IntoElement)]
pub(crate) struct Icon {
    name: IconName,
    size: Rems,
    color: Rgba,
}

impl Icon {
    pub(crate) fn new(name: IconName, size: Rems, color: Rgba) -> Self {
        Self { name, size, color }
    }
}

impl RenderOnce for Icon {
    fn render(self, _: &mut gpui::Window, _: &mut gpui::App) -> impl IntoElement {
        svg()
            .path(self.name.path())
            .size(self.size)
            .flex_none()
            .text_color(self.color)
    }
}

#[derive(IntoElement)]
pub(crate) struct IconButton {
    icon: IconName,
    icon_size: Rems,
    icon_color: Rgba,
    hover_background: Rgba,
    toggled: bool,
    toggled_icon_color: Option<Rgba>,
    toggled_background: Option<Rgba>,
}

impl IconButton {
    pub(crate) fn new(icon: IconName, icon_color: Rgba, hover_background: Rgba) -> Self {
        Self {
            icon,
            icon_size: ui_rems(CONTROL_ICON_SIZE),
            icon_color,
            hover_background,
            toggled: false,
            toggled_icon_color: None,
            toggled_background: None,
        }
    }

    pub(crate) fn icon_size(mut self, icon_size: Rems) -> Self {
        self.icon_size = icon_size;
        self
    }

    pub(crate) fn toggle_state(mut self, toggled: bool) -> Self {
        self.toggled = toggled;
        self
    }

    pub(crate) fn toggled_icon_color(mut self, color: Rgba) -> Self {
        self.toggled_icon_color = Some(color);
        self
    }

    pub(crate) fn toggled_background(mut self, color: Rgba) -> Self {
        self.toggled_background = Some(color);
        self
    }

    pub(crate) fn into_div(self) -> Div {
        let icon_color = if self.toggled {
            self.toggled_icon_color.unwrap_or(self.icon_color)
        } else {
            self.icon_color
        };
        let hover_background = if self.toggled {
            self.toggled_background.unwrap_or(self.hover_background)
        } else {
            self.hover_background
        };

        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(ui_rems(CONTROL_SIZE))
            .h(ui_rems(CONTROL_SIZE))
            .rounded(px(RADIUS_MD))
            .cursor_pointer()
            .when(self.toggled, |this| {
                this.when_some(self.toggled_background, |this, background| {
                    this.bg(background)
                })
            })
            .hover(move |style| style.bg(hover_background))
            .child(Icon::new(self.icon, self.icon_size, icon_color))
    }
}

impl RenderOnce for IconButton {
    fn render(self, _: &mut gpui::Window, _: &mut gpui::App) -> impl IntoElement {
        self.into_div()
    }
}
