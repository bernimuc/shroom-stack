//! Image processing core of Shroom Stack: DNG I/O, raw processing, alignment and stacking.
//!
//! The UI only talks to this crate through the types re-exported here; nothing from the
//! underlying raw decoder leaks into the public API.

pub mod align;
mod error;
pub mod filter;
mod image;
pub mod io;
pub mod raw;
pub mod render;
pub mod stack;

pub use error::{Error, Result};
pub use image::{DisplayImage, Plane, Rect, RgbImage};
