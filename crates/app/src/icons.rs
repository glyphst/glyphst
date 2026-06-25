use gpui::{IntoElement, Rems, RenderOnce, Rgba, SharedString, Styled as _, svg};

#[derive(Clone, Copy)]
pub(crate) enum IconName {
    AppWindow,
    Bot,
    File,
    LayoutGrid,
    Menu,
    NotebookTabs,
    PanelLeft,
    Plus,
    TableOfContents,
    X,
}

impl IconName {
    fn path(self) -> SharedString {
        let name = match self {
            Self::AppWindow => "app-window",
            Self::Bot => "bot",
            Self::File => "file",
            Self::LayoutGrid => "layout-grid",
            Self::Menu => "menu",
            Self::NotebookTabs => "notebook-tabs",
            Self::PanelLeft => "panel-left",
            Self::Plus => "plus",
            Self::TableOfContents => "table-of-contents",
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
