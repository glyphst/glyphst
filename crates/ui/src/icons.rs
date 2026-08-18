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
    Bookmark,
    BookOpen,
    Bot,
    CaseSensitive,
    Check,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    Circle,
    Copy,
    Delete,
    Download,
    File,
    FileText,
    FolderOpen,
    GripVertical,
    Home,
    Image,
    Info,
    LayoutGrid,
    LayoutList,
    ListTree,
    Link2,
    LocateFixed,
    Menu,
    Minus,
    MoveHorizontal,
    MoveVertical,
    NotebookTabs,
    PanelLeft,
    Pause,
    Pencil,
    Play,
    Plus,
    Return,
    Regex,
    Repeat,
    RotateCcw,
    Scan,
    Search,
    Settings,
    Stop,
    TableOfContents,
    Table2,
    TextUnwrap,
    TextWrap,
    Trash2,
    TriangleAlert,
    Unlink2,
    X,
}

impl IconName {
    pub fn path(self) -> SharedString {
        let name = match self {
            Self::AppWindow => "app-window",
            Self::ArrowUp => "arrow-up",
            Self::ArrowUpRight => "arrow-up-right",
            Self::Bookmark => "bookmark",
            Self::BookOpen => "book-open",
            Self::Bot => "bot",
            Self::CaseSensitive => "case-sensitive",
            Self::Check => "check",
            Self::ChevronDown => "chevron-down",
            Self::ChevronLeft => "chevron-left",
            Self::ChevronRight => "chevron-right",
            Self::ChevronUp => "chevron-up",
            Self::Circle => "circle",
            Self::Copy => "copy",
            Self::Delete => "delete",
            Self::Download => "download",
            Self::File => "file",
            Self::FileText => "file-text",
            Self::FolderOpen => "folder-open",
            Self::GripVertical => "grip-vertical",
            Self::Home => "home",
            Self::Image => "image",
            Self::Info => "info",
            Self::LayoutGrid => "layout-grid",
            Self::LayoutList => "layout-list",
            Self::ListTree => "list-tree",
            Self::Link2 => "link-2",
            Self::LocateFixed => "locate-fixed",
            Self::Menu => "menu",
            Self::Minus => "minus",
            Self::MoveHorizontal => "move-horizontal",
            Self::MoveVertical => "move-vertical",
            Self::NotebookTabs => "notebook-tabs",
            Self::PanelLeft => "panel-left",
            Self::Pause => "pause",
            Self::Pencil => "pencil",
            Self::Play => "play",
            Self::Plus => "plus",
            Self::Return => "return",
            Self::Regex => "regex",
            Self::Repeat => "repeat",
            Self::RotateCcw => "rotate-ccw",
            Self::Scan => "scan",
            Self::Search => "search",
            Self::Settings => "settings",
            Self::Stop => "stop",
            Self::TableOfContents => "table-of-contents",
            Self::Table2 => "table-2",
            Self::TextUnwrap => "text-unwrap",
            Self::TextWrap => "text-wrap",
            Self::Trash2 => "trash-2",
            Self::TriangleAlert => "triangle-alert",
            Self::Unlink2 => "unlink-2",
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
