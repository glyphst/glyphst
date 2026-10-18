//! The craft-ui components the Reader uses, copied from
//! [craft-libs](https://github.com/storytold/craft-libs) `crates/ui` at
//! revision `5c874d51eeb3de70e735c8ce87a59a7dedd47675` (MIT OR Apache-2.0;
//! the MIT notice is in `LICENSE-MIT` beside this file). Copyright (c) 2026
//! ArtCraft Team and the craft-libs contributors.
//!
//! Only the parts in use came along: icon buttons, the SVG icon set, tab
//! fitting and tabs, split geometry and resize handles, the docking model and
//! its drop targets (not its surface), and the widget palette. Later changes
//! are ours (egui upgrades, unconditional serde).

pub mod buttons;
pub mod docking;
pub mod icons;
pub mod layout;
pub mod tabs;
pub mod theme;
