//! Raw sensor data processing.

mod cfa;
mod demosaic;

pub use cfa::{BLUE, CfaPattern, GREEN, RED};
pub use demosaic::demosaic;
