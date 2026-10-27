//! Safe wrappers over the Win32 APIs CapShotX needs.
//!
//! Every `unsafe` Win32 call lives in this crate; the rest of the workspace
//! only sees safe, `Send` handle types.

#![cfg(windows)]

pub mod audio;
pub mod clipboard;
pub mod clock;
pub mod d2d;
pub mod hotkeys;
pub mod monitors;
pub mod popup_menu;
pub mod rec_outline;
pub mod rec_staging;
pub mod rec_toolbar;
pub mod setup_ops;
pub mod shell;
pub mod single_instance;
pub mod sound;
pub mod startup;
pub mod toast;
