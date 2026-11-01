//! Raw sensor data processing.

mod cfa;
mod demosaic;
mod hotpixels;

pub use cfa::{BLUE, CfaPattern, GREEN, RED};
pub use demosaic::demosaic;
pub use hotpixels::remove_hot_pixels;
