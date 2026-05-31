//! ribb — rust iced booru browser.
//!
//! A native port of ebb. This crate currently exposes the [`booru`] API layer;
//! the iced GUI lives in the binary (`src/main.rs`).

pub mod app;
pub mod booru;
pub mod cache;
pub mod settings;
pub mod style;
pub mod video;
