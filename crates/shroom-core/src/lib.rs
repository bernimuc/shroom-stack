//! Image processing core of Shroom Stack: DNG I/O, raw processing, alignment and stacking.
//!
//! The UI only talks to this crate through the types re-exported here; nothing from the
//! underlying raw decoder leaks into the public API.

pub mod align;
mod compute;
mod error;
pub mod filter;
mod gpu;
mod image;
pub mod io;
pub mod raw;
pub mod render;
pub mod retouch;
pub mod session;
pub mod stack;

#[cfg(test)]
mod profile;

pub use error::{Error, Result};
pub use image::{DisplayImage, Orientation, Plane, Rect, RgbImage};
