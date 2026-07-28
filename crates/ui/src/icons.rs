use crate::{settings::ui_rems, style::CONTROL_SIZE};
use gpui::{ElementId, MouseButton, SharedString, Styled as _, prelude::*};
use gpui_component::{
    Sizable as _,
    button::{Button, ButtonVariants as _},
};

pub use gpui_component::Icon;

#[derive(Clone, Copy)]
pub enum IconName {
    AppWindow,
    ArrowUp,
    ArrowUpRight,
    BookOpen,
    Bot,
    CaseSensitive,
    Check,
    ChevronDown,
    ChevronUp,
    Circle,
    Copy,
    Delete,
    Download,
    File,
    FileText,
    FolderOpen,
    Image,
    Info,
    LayoutGrid,
    LayoutList,
    LocateFixed,
    Menu,
    NotebookTabs,
    PanelLeft,
    Pencil,
    Play,
    Plus,
    Return,
    RotateCcw,
    Scan,
    Search,
    Settings,
    Stop,
    TableOfContents,
    Table2,
    TextUnwrap,
    TextWrap,
    TriangleAlert,
    X,
}

impl IconName {
    pub fn path(self) -> SharedString {
        let name = match self {
            Self::AppWindow => "app-window",
            Self::ArrowUp => "arrow-up",
            Self::ArrowUpRight => "arrow-up-right",
            Self::BookOpen => "book-open",
            Self::Bot => "bot",
            Self::CaseSensitive => "case-sensitive",
            Self::Check => "check",
            Self::ChevronDown => "chevron-down",
            Self::ChevronUp => "chevron-up",
            Self::Circle => "circle",
            Self::Copy => "copy",
            Self::Delete => "delete",
            Self::Download => "download",
            Self::File => "file",
            Self::FileText => "file-text",
            Self::FolderOpen => "folder-open",
            Self::Image => "image",
            Self::Info => "info",
            Self::LayoutGrid => "layout-grid",
            Self::LayoutList => "layout-list",
            Self::LocateFixed => "locate-fixed",
            Self::Menu => "menu",
            Self::NotebookTabs => "notebook-tabs",
            Self::PanelLeft => "panel-left",
            Self::Pencil => "pencil",
            Self::Play => "play",
            Self::Plus => "plus",
            Self::Return => "return",
            Self::RotateCcw => "rotate-ccw",
            Self::Scan => "scan",
            Self::Search => "search",
            Self::Settings => "settings",
            Self::Stop => "stop",
            Self::TableOfContents => "table-of-contents",
            Self::Table2 => "table-2",
            Self::TextUnwrap => "text-unwrap",
            Self::TextWrap => "text-wrap",
            Self::TriangleAlert => "triangle-alert",
            Self::X => "x",
        };

        format!("icons/{name}.svg").into()
    }
}

impl gpui_component::IconNamed for IconName {
    fn path(self) -> SharedString {
        IconName::path(self)
    }
}

/// A compact gpui-component button sized to Glyphst's document toolbar grid.
///
/// The shared mouse-down guard keeps toolbar clicks from leaking into
/// document selection and drag surfaces. Callers attach their action with
/// [`Button::on_click`].
pub fn toolbar_icon_button(id: impl Into<ElementId>, icon: IconName) -> Button {
    Button::new(id)
        .small()
        .compact()
        .ghost()
        .icon(icon)
        .w(ui_rems(CONTROL_SIZE))
        .h(ui_rems(CONTROL_SIZE))
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
}
