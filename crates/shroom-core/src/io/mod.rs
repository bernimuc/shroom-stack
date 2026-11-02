//! Reading and writing image files. Only DNG is supported for now.

pub mod cameras;
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

/// File extensions of the raw formats that can be read (DNG plus the camera formats
/// rawler supports).
pub const RAW_EXTENSIONS: &[&str] = &[
    "dng", "cr3", "cr2", "crw", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw", "iiq", "3fr",
    "fff", "erf", "mef", "mos", "mrw", "kdc", "dcr", "x3f",
];

/// Whether `path` has the extension of a readable raw format.
pub fn is_raw_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| RAW_EXTENSIONS.iter().any(|r| r.eq_ignore_ascii_case(e)))
}

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
    /// Orientation recorded by the camera.
    pub orientation: crate::Orientation,
    /// Whether Adobe's calibration values for this camera are known (always for DNGs);
    /// without them results from raw files look darker in Lightroom.
    pub camera_values_known: bool,
    /// Bayer raw data; false for Linear DNGs such as stacking results.
    pub mosaic: bool,
    /// For stacking results: file names of the frames they were stacked from (from XMP).
    pub sources: Vec<String>,
    /// For stacking results: the settings they were made with (from XMP).
    pub settings: Option<crate::stack::StackSettings>,
    pub exposure: Exposure,
}

/// Camera settings of a shot, as far as recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Exposure {
    /// Exposure time in seconds.
    pub time: Option<f32>,
    pub f_number: Option<f32>,
    pub iso: Option<u32>,
    /// Focal length in millimetres.
    pub focal_length: Option<f32>,
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
            orientation: Default::default(),
            camera_values_known: true,
            mosaic: true,
            sources: Vec::new(),
            settings: None,
            exposure: Default::default(),
        }
    }

    #[test]
    fn recognises_raw_extensions() {
        for name in ["a.dng", "b.CR3", "c.nef", "d.ARW"] {
            assert!(is_raw_file(Path::new(name)), "{name}");
        }
        for name in ["a.jpg", "b.tif", "noext", "c.dng.xmp"] {
            assert!(!is_raw_file(Path::new(name)), "{name}");
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
