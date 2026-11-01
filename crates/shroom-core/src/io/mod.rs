//! Reading and writing image files. Only DNG is supported for now.

mod reader;
mod writer;
mod xmp;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::NaiveDateTime;

pub(crate) use reader::load_for_stacking;
pub use reader::{DngTemplate, LinearFrame, RawFrame, decode_raw, load_linear, read_template};
pub use writer::write_linear_dng;

use crate::image::DisplayImage;

/// Cheap-to-read information about an input file, for the file list.
#[derive(Clone, Debug)]
pub struct FrameInfo {
    pub path: PathBuf,
    pub file_name: String,
    pub file_modified: Option<SystemTime>,
    /// EXIF capture time including sub-seconds, if available.
    pub capture_time: Option<NaiveDateTime>,
    pub camera: String,
    /// Size of the usable image area in pixels.
    pub width: usize,
    pub height: usize,
    /// Embedded preview (typically 1024 px wide), if present.
    pub preview: Option<DisplayImage>,
    /// Colour information for rendering the image data.
    pub color: crate::render::ColorInfo,
}

/// Reads metadata and the embedded preview without decoding the raw image data.
pub fn read_frame_info(path: &Path) -> crate::Result<FrameInfo> {
    reader::read_frame_info(path)
}

/// Sorts frames into shooting order: by capture time, then by file name.
pub fn sort_frames(frames: &mut [FrameInfo]) {
    frames.sort_by(|a, b| a.capture_time.cmp(&b.capture_time).then_with(|| a.file_name.cmp(&b.file_name)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, time: Option<&str>) -> FrameInfo {
        FrameInfo {
            path: PathBuf::from(name),
            file_name: name.to_string(),
            file_modified: None,
            capture_time: time.map(|t| NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S%.f").unwrap()),
            camera: String::new(),
            width: 0,
            height: 0,
            preview: None,
            color: Default::default(),
        }
    }

    #[test]
    fn sorts_by_time_then_name() {
        let mut frames = vec![
            info("c.dng", Some("2026-10-20 14:20:38.50")),
            info("b.dng", Some("2026-10-20 14:20:38.10")),
            info("z.dng", Some("2026-10-20 14:20:37.90")),
            info("a.dng", Some("2026-10-20 14:20:38.50")),
        ];
        sort_frames(&mut frames);
        let names: Vec<_> = frames.iter().map(|f| f.file_name.as_str()).collect();
        assert_eq!(names, ["z.dng", "b.dng", "a.dng", "c.dng"]);
    }

    #[test]
    fn frames_without_time_come_first_sorted_by_name() {
        let mut frames = vec![info("b.dng", None), info("x.dng", Some("2026-01-01 00:00:00.0")), info("a.dng", None)];
        sort_frames(&mut frames);
        let names: Vec<_> = frames.iter().map(|f| f.file_name.as_str()).collect();
        assert_eq!(names, ["a.dng", "b.dng", "x.dng"]);
    }
}
