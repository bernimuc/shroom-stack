//! Reading and writing image files: raw formats and JPEG in, Linear DNG or JPEG out.

pub mod cameras;
mod jpeg;
mod reader;
mod writer;
mod xmp;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::NaiveDateTime;

pub use jpeg::{DEFAULT_QUALITY as DEFAULT_JPEG_QUALITY, JPEG_EXTENSIONS, is_jpeg_file, write_jpeg};
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

/// File extensions of all readable formats: [`RAW_EXTENSIONS`] and [`JPEG_EXTENSIONS`].
pub const INPUT_EXTENSIONS: &[&str] = &[
    "dng", "cr3", "cr2", "crw", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw", "iiq", "3fr",
    "fff", "erf", "mef", "mos", "mrw", "kdc", "dcr", "x3f", "jpg", "jpeg", "jpe",
];

/// Whether `path` has the extension of a readable format (raw or JPEG).
pub fn is_input_file(path: &Path) -> bool {
    is_raw_file(path) || is_jpeg_file(path)
}

/// Whether decoding `path` is slow compared to processing it (camera raw formats like
/// CR3), so that keeping decoded frames between passes pays off. DNGs and JPEGs decode
/// quickly.
pub fn is_slow_to_decode(path: &Path) -> bool {
    !(is_jpeg_file(path) || path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dng")))
}

/// File format of stacking results.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResultFormat {
    /// Linear DNG: the full linear data with the source's colour calibration, for raw
    /// converters.
    #[default]
    Dng,
    /// JPEG rendered like the on-screen display, with the given quality (1–100).
    Jpeg { quality: u8 },
}

impl ResultFormat {
    /// File extension of results in this format.
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Dng => "dng",
            Self::Jpeg { .. } => "jpg",
        }
    }

    /// Writes `image` in this format, with metadata from `template`.
    pub fn write(
        &self,
        path: &Path,
        image: &crate::RgbImage,
        template: &DngTemplate,
        software: &str,
    ) -> crate::Result<()> {
        match *self {
            Self::Dng => write_linear_dng(path, image, template, software),
            Self::Jpeg { quality } => write_jpeg(path, image, template, quality, software),
        }
    }
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
    /// Bayer raw data; false for Linear DNGs such as stacking results, and for JPEGs.
    pub mosaic: bool,
    /// A JPEG file (gamma-encoded sRGB) rather than a raw file.
    pub jpeg: bool,
    /// For stacking results: file names of the frames they were stacked from (from XMP).
    pub sources: Vec<String>,
    /// For stacking results: the settings they were made with (from XMP).
    pub settings: Option<crate::stack::StackSettings>,
    /// Why the image data can't be decoded (e.g. Nikon HE* compression); the other fields
    /// come from the metadata and the embedded preview. Such files can't be stacked.
    pub unsupported: Option<String>,
    pub exposure: Exposure,
}

impl FrameInfo {
    /// Whether the file is a stacking result rather than a photo: Linear DNGs are, JPEGs
    /// only if they record how they were stacked.
    pub fn is_result(&self) -> bool {
        if self.jpeg { !self.sources.is_empty() || self.settings.is_some() } else { !self.mosaic }
    }
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
            jpeg: false,
            sources: Vec::new(),
            settings: None,
            unsupported: None,
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
    fn input_extensions_are_raw_and_jpeg() {
        let expected: Vec<_> = RAW_EXTENSIONS.iter().chain(JPEG_EXTENSIONS).collect();
        assert_eq!(INPUT_EXTENSIONS.iter().collect::<Vec<_>>(), expected);
        assert!(is_input_file(Path::new("a.JPG")) && is_input_file(Path::new("b.cr3")));
        assert!(!is_input_file(Path::new("c.png")));
    }

    #[test]
    fn slow_formats() {
        assert!(is_slow_to_decode(Path::new("a.CR3")));
        assert!(!is_slow_to_decode(Path::new("a.DNG")));
        assert!(!is_slow_to_decode(Path::new("a.jpeg")));
    }

    #[test]
    fn results_are_recognised() {
        let mut f = info("a.dng", None);
        assert!(!f.is_result());
        f.mosaic = false;
        assert!(f.is_result());
        f.jpeg = true;
        assert!(!f.is_result(), "a plain JPEG is a photo");
        f.sources = vec!["x.jpg".into()];
        assert!(f.is_result());
    }

    #[test]
    fn result_formats() {
        assert_eq!(ResultFormat::default(), ResultFormat::Dng);
        assert_eq!(ResultFormat::Dng.extension(), "dng");
        assert_eq!(ResultFormat::Jpeg { quality: 90 }.extension(), "jpg");
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
