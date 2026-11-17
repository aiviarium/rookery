//! The parts of rookery's screens with no terminal in them: they draw into
//! ratatui frames and take keys of their own type, which rookery-page turns a
//! terminal's events into.

pub mod event;
pub mod fields;
pub mod flow;
pub mod form;
pub mod host;
pub mod links;
pub mod machine;
pub mod markdown;
pub mod page;
pub mod plugins;
pub mod provisioners;
pub mod qr;
pub mod rookery_key;
pub mod screen;
pub mod setup;
pub mod text;
pub mod theme;

pub use event::{Click, Code, Event, Key, Mouse};
