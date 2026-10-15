//! Glyphst's design system on egui: the tokens, type and icons of the glyph
//! design (`design/main.pen`) and one reusable component per pen component.
//! Components read [`theme::Tokens::get`] only, so callers never pick
//! colours, radii or fonts.
//!
//! | pen component | module |
//! | --- | --- |
//! | btn/primary, btn/ghost, btn/bracket, btn/icon | [`button`] |
//! | key | [`key`] |
//! | badge, chip/context | [`badge`] |
//! | checkbox, switch/on, switch/off | [`toggle`] |
//! | slider | [`slider`] |
//! | meter | [`meter`] |
//! | input/search | [`field`] |
//! | segmented | [`segmented`] |
//! | tooltip, toast, pending-keys | [`overlay`] |
//! | tab, tab/active | [`tab`] |
//! | rail-btn, rail-btn/active | [`rail`] |
//! | pane-header | [`pane`] |
//! | status-seg | [`status`] |
//! | row/list, row/list-active, row/tree | [`row`] |
//! | menu, menu-item | [`menu`] |
//! | doc-cover | [`cover`] |
//! | msg/user, timeline-step, suggestion | [`chat`] |
//! | page-thumb | [`thumb`] |
//! | rule titles, trees, meters, ticks, dot fields | [`motif`] |
//! | settings rows (frame 07) | [`settings`] |

pub mod badge;
pub mod button;
pub mod chat;
pub mod cover;
pub mod field;
pub mod fonts;
pub mod gallery;
mod icon_data;
pub mod icons;
pub mod key;
pub mod menu;
pub mod meter;
pub mod motif;
pub mod overlay;
pub mod paint;
pub mod pane;
pub mod rail;
pub mod row;
pub mod segmented;
pub mod settings;
pub mod slider;
pub mod status;
pub mod tab;
pub mod text;
pub mod theme;
pub mod thumb;
pub mod toggle;

pub use overlay::Tip;
