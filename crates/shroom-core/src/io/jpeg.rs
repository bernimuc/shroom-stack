//! Reading JPEG frames and writing JPEG results.
//!
//! JPEGs hold gamma-encoded sRGB (8 bits per channel). For stacking they are decoded into
//! linear sRGB in `0..=1`, which the stackers treat like linear camera RGB; their
//! [`ColorInfo`] is the identity rendering, so [`render_display`] gives back the original
//! pixels. Like raw files, pixel data stays as stored and the EXIF orientation is applied
//! when displaying.

use std::io::{BufReader, Cursor, Read};
use std::path::Path;
use std::sync::LazyLock;

use image::ImageDecoder;
use image::codecs::jpeg::{JpegDecoder, JpegEncoder};
use rawler::RawImageData;
use rawler::decoders::{Camera, RawMetadata};
use rawler::exif::Exif;
use rawler::formats::tiff::reader::TiffReader;
use rawler::formats::tiff::{DirectoryWriter, GenericTiffReader, TiffWriter};
use rawler::imgop::xyz::Illuminant;
use rawler::pixarray::PixU16;
use rawler::rawimage::{BlackLevel, RawPhotometricInterpretation, WhiteLevel};
use rawler::tags::{ExifTag, TiffCommonTag};
use rayon::prelude::*;

use super::FrameInfo;
use super::reader::{DngTemplate, LinearFrame, MAX_PREVIEW, capture_time, exposure};
use crate::error::{Error, Result};
use crate::image::{DisplayImage, RgbImage};
use crate::render::{ColorInfo, XYZ_TO_SRGB, render_display};

/// File extensions of JPEG files.
pub const JPEG_EXTENSIONS: &[&str] = &["jpg", "jpeg", "jpe"];

/// Default quality of JPEG results (1–100).
pub const DEFAULT_QUALITY: u8 = 92;

/// Identifier of an XMP packet in a JPEG APP1 segment.
const XMP_SIGNATURE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const APP1: u8 = 0xE1;
/// Largest payload of a JPEG marker segment (its length field counts itself).
const MAX_SEGMENT: usize = 65533;

/// Whether `path` has the extension of a JPEG file.
pub fn is_jpeg_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| JPEG_EXTENSIONS.iter().any(|j| j.eq_ignore_ascii_case(e)))
}

/// Whether the file starts like a JPEG (SOI marker), whatever its name.
pub(super) fn has_jpeg_signature(path: &Path) -> bool {
    let mut magic = [0u8; 3];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && magic == [0xFF, 0xD8, 0xFF]
}

/// The metadata of a JPEG that matters for stacking and for result files.
struct Metadata {
    width: usize,
    height: usize,
    make: String,
    model: String,
    exif: Exif,
    orientation: crate::Orientation,
    xmp: Option<String>,
    icc: Option<Vec<u8>>,
}

fn decode_err(path: &Path, e: impl std::fmt::Display) -> Error {
    Error::Decode { path: path.into(), message: e.to_string() }
}

fn open(path: &Path) -> Result<(JpegDecoder<BufReader<std::fs::File>>, Metadata)> {
    let file = std::fs::File::open(path).map_err(|source| Error::Io { path: path.into(), source })?;
    let mut decoder = JpegDecoder::new(BufReader::new(file)).map_err(|e| decode_err(path, e))?;
    let (width, height) = decoder.dimensions();
    let exif_blob = decoder.exif_metadata().ok().flatten();
    let xmp = decoder.xmp_metadata().ok().flatten().map(|x| String::from_utf8_lossy(&x).into_owned());
    let icc = decoder.icc_profile().ok().flatten();
    let (exif, make, model) = exif_blob.as_deref().and_then(parse_exif).unwrap_or_default();
    let orientation = crate::Orientation::from_exif(exif.orientation.unwrap_or(1));
    let metadata =
        Metadata { width: width as usize, height: height as usize, make, model, exif, orientation, xmp, icc };
    Ok((decoder, metadata))
}

/// Parses an EXIF block (a TIFF structure) into the EXIF fields and camera make and model.
fn parse_exif(blob: &[u8]) -> Option<(Exif, String, String)> {
    // Some writers keep the APP1 identifier in front of the TIFF header.
    let blob = blob.strip_prefix(b"Exif\0\0").unwrap_or(blob);
    if blob.len() < 8 {
        return None;
    }
    let tiff = GenericTiffReader::new_with_buffer(blob, 0, 0, None).ok()?;
    let root = tiff.root_ifd();
    let text = |tag| root.get_entry(tag).and_then(|e| e.value.as_string()).map(|s| s.trim().to_string());
    let exif = Exif::new(root).ok()?;
    Some((exif, text(TiffCommonTag::Make).unwrap_or_default(), text(TiffCommonTag::Model).unwrap_or_default()))
}

/// Short make and model, as rawler names cameras of raw files: cameras often repeat the
/// make in the model ("Canon" / "Canon EOS R5") or add the company form ("NIKON
/// CORPORATION" / "NIKON Z 8"); both become e.g. "Canon" and "EOS R5".
fn clean_camera(make: &str, model: &str) -> (String, String) {
    let short = make.split_whitespace().next().unwrap_or_default();
    let model = match model.get(..short.len()) {
        Some(prefix) if !short.is_empty() && prefix.eq_ignore_ascii_case(short) => model[short.len()..].trim_start(),
        _ => model,
    };
    (short.to_string(), model.to_string())
}

fn camera_name(meta: &Metadata) -> String {
    let (make, model) = clean_camera(&meta.make, &meta.model);
    format!("{make} {model}").trim().to_string()
}

/// Decodes the pixel data as 8-bit RGB (greyscale and CMYK are converted).
fn decode_rgb8(path: &Path, decoder: JpegDecoder<BufReader<std::fs::File>>) -> Result<image::RgbImage> {
    Ok(image::DynamicImage::from_decoder(decoder).map_err(|e| decode_err(path, e))?.into_rgb8())
}

pub(super) fn read_frame_info(path: &Path) -> Result<FrameInfo> {
    let (decoder, meta) = open(path)?;
    // JPEGs have no preview of a useful size, so make one from the image itself.
    let rgb = decode_rgb8(path, decoder)?;
    let preview = if rgb.width().max(rgb.height()) > MAX_PREVIEW {
        image::DynamicImage::ImageRgb8(rgb).thumbnail(MAX_PREVIEW, MAX_PREVIEW).into_rgb8()
    } else {
        rgb
    };
    let preview =
        DisplayImage { width: preview.width() as usize, height: preview.height() as usize, data: preview.into_raw() };
    let raw_metadata = raw_metadata(&meta);
    // Results saved as JPEG record how they were made, like Linear DNGs.
    let sources = meta.xmp.as_deref().map(super::xmp::ingredients).unwrap_or_default();
    let params = meta.xmp.as_deref().map(super::xmp::params).unwrap_or_default();
    let settings = crate::stack::StackSettings::from_metadata(|k| params.get(k).cloned());
    Ok(FrameInfo {
        path: path.into(),
        file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        file_modified: std::fs::metadata(path).and_then(|m| m.modified()).ok(),
        capture_time: capture_time(&raw_metadata),
        camera: camera_name(&meta),
        width: meta.width,
        height: meta.height,
        preview: Some(preview),
        color: ColorInfo::srgb(),
        orientation: meta.orientation,
        camera_values_known: true,
        mosaic: false,
        jpeg: true,
        sources,
        settings,
        unsupported: None,
        exposure: exposure(&raw_metadata),
    })
}

/// Loads a JPEG as linear sRGB in `0..=1`.
pub(super) fn load_linear(path: &Path) -> Result<LinearFrame> {
    let (decoder, meta) = open(path)?;
    let rgb = decode_rgb8(path, decoder)?;
    let mut image = RgbImage::new(meta.width, meta.height);
    image
        .data_mut()
        .par_iter_mut()
        .zip(rgb.as_raw().par_iter())
        .for_each(|(out, v)| *out = SRGB_TO_LINEAR[*v as usize]);
    Ok(LinearFrame { image, color: ColorInfo::srgb(), template: template(meta) })
}

/// Reads the metadata needed to write a result based on this JPEG.
pub(super) fn read_template(path: &Path) -> Result<DngTemplate> {
    Ok(template(open(path)?.1))
}

/// Linear value of each 8-bit sRGB code.
static SRGB_TO_LINEAR: LazyLock<[f32; 256]> = LazyLock::new(|| std::array::from_fn(|i| srgb_decode(i as f32 / 255.0)));

/// Inverse of the sRGB transfer function.
fn srgb_decode(v: f32) -> f32 {
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn raw_metadata(meta: &Metadata) -> RawMetadata {
    RawMetadata { exif: meta.exif.clone(), make: meta.make.clone(), model: meta.model.clone(), ..Default::default() }
}

/// A template describing linear sRGB data, so results from JPEG frames can be written as
/// Linear DNG too: the colour matrix is sRGB's and the white balance neutral.
fn template(meta: Metadata) -> DngTemplate {
    let (clean_make, clean_model) = clean_camera(&meta.make, &meta.model);
    let mut camera =
        Camera { make: meta.make.clone(), clean_make, model: meta.model.clone(), clean_model, ..Default::default() };
    camera.color_matrix.insert(Illuminant::D65, XYZ_TO_SRGB.as_flattened().to_vec());
    let mut raw = rawler::RawImage::new(
        camera,
        PixU16::new_with(vec![0; 3], 3, 1),
        3,
        [1.0, 1.0, 1.0, f32::NAN],
        RawPhotometricInterpretation::LinearRaw,
        Some(BlackLevel::new(&[0u32, 0, 0], 1, 1, 3)),
        Some(WhiteLevel::new(vec![65535, 65535, 65535])),
        false,
    );
    raw.data = RawImageData::Integer(Vec::new());
    raw.orientation = rawler::Orientation::from_u16(meta.orientation.to_exif());
    let metadata = raw_metadata(&meta);
    DngTemplate {
        raw,
        metadata,
        root_tags: Vec::new(),
        raw_tags: Vec::new(),
        xmp: meta.xmp,
        sources: Vec::new(),
        params: Vec::new(),
        icc: meta.icc,
    }
}

/// Writes `image` (linear, in the colours described by `template`) as a JPEG rendered like
/// the on-screen display, with EXIF from `template` and the sources and settings in XMP.
///
/// The file is written to a temporary name first and renamed on success.
pub fn write_jpeg(path: &Path, image: &RgbImage, template: &DngTemplate, quality: u8, software: &str) -> Result<()> {
    let bytes = encode_jpeg(image, template, quality, software)
        .map_err(|message| Error::Encode { path: path.into(), message })?;
    super::writer::write_atomically(path, |tmp| {
        std::fs::write(tmp, &bytes).map_err(|source| Error::Io { path: tmp.into(), source })
    })
}

fn encode_jpeg(
    image: &RgbImage,
    template: &DngTemplate,
    quality: u8,
    software: &str,
) -> std::result::Result<Vec<u8>, String> {
    let display = render_display(image, &template.color(), None);
    let exif = exif_block(template, software).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
    use image::ImageEncoder;
    encoder.set_exif_metadata(exif).map_err(|e| e.to_string())?;
    // A profile of the source (e.g. Adobe RGB) still describes the colours: rendering
    // JPEG frames is the identity.
    if let Some(icc) = &template.icc {
        encoder.set_icc_profile(icc.clone()).map_err(|e| e.to_string())?;
    }
    encoder
        .encode(&display.data, display.width as u32, display.height as u32, image::ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())?;
    // Only our own provenance goes into the XMP: develop settings of a raw source would
    // be applied a second time to the rendered JPEG.
    match super::xmp::with_provenance(None, &template.sources, &template.params) {
        Some(xmp) => Ok(insert_xmp(out, &xmp)),
        None => Ok(out),
    }
}

/// The EXIF block (a little TIFF structure) for a result: camera, exposure, lens and dates
/// of the source, the result's orientation, software and modification date.
fn exif_block(template: &DngTemplate, software: &str) -> rawler::formats::tiff::Result<Vec<u8>> {
    let mut buffer = Cursor::new(Vec::new());
    let mut tiff = TiffWriter::new(&mut buffer)?;
    let mut root = DirectoryWriter::new();
    let mut exif_ifd = DirectoryWriter::new();
    exif_ifd.add_tag_undefined(ExifTag::ExifVersion, b"0232".to_vec());
    let mut metadata = template.metadata.clone();
    metadata.exif.orientation = Some(template.raw.orientation.to_u16());
    metadata.exif.modify_date = Some(chrono::Local::now().format("%Y:%m:%d %H:%M:%S").to_string());
    metadata.write_exif_tags(&mut tiff, &mut root, &mut exif_ifd)?;
    if template.icc.is_none() {
        exif_ifd.add_tag(ExifTag::ColorSpace, 1u16); // sRGB
    }
    let (make, model) = (&template.raw.clean_make, &template.raw.model);
    if !make.is_empty() {
        root.add_tag(TiffCommonTag::Make, make.as_str());
    }
    if !model.is_empty() {
        root.add_tag(TiffCommonTag::Model, model.as_str());
    }
    root.add_tag(TiffCommonTag::Software, software);
    let exif_offset = exif_ifd.build(&mut tiff)?;
    root.add_tag(TiffCommonTag::ExifIFDPointer, exif_offset);
    tiff.build(root)?;
    Ok(buffer.into_inner())
}

/// Inserts `xmp` as an APP1 segment after the leading application segments (JFIF, EXIF,
/// ICC) of `jpeg`. Packets too large for one segment are left out.
fn insert_xmp(jpeg: Vec<u8>, xmp: &str) -> Vec<u8> {
    let payload_len = XMP_SIGNATURE.len() + xmp.len();
    if payload_len > MAX_SEGMENT || jpeg.len() < 4 {
        return jpeg;
    }
    // Skip SOI, then every APPn segment.
    let mut pos = 2;
    while pos + 4 <= jpeg.len() && jpeg[pos] == 0xFF && (0xE0..=0xEF).contains(&jpeg[pos + 1]) {
        pos += 2 + u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]) as usize;
    }
    let pos = pos.min(jpeg.len());
    let mut out = Vec::with_capacity(jpeg.len() + payload_len + 4);
    out.extend_from_slice(&jpeg[..pos]);
    out.extend_from_slice(&[0xFF, APP1]);
    out.extend_from_slice(&((payload_len + 2) as u16).to_be_bytes());
    out.extend_from_slice(XMP_SIGNATURE);
    out.extend_from_slice(xmp.as_bytes());
    out.extend_from_slice(&jpeg[pos..]);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::io::reader::tests::synthetic_template;

    /// Writes a small JPEG with the given pixels and optional raw EXIF block.
    pub(crate) fn write_test_jpeg(path: &Path, img: &image::RgbImage, exif: Option<Vec<u8>>) {
        use image::ImageEncoder;
        let mut out = Vec::new();
        let mut encoder = JpegEncoder::new_with_quality(&mut out, 100);
        if let Some(exif) = exif {
            encoder.set_exif_metadata(exif).unwrap();
        }
        encoder.write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgb8).unwrap();
        std::fs::write(path, out).unwrap();
    }

    /// An EXIF block as a camera would write it.
    pub(crate) fn camera_exif(time: &str, subsec: &str, orientation: u16) -> Vec<u8> {
        let mut buffer = Cursor::new(Vec::new());
        let mut tiff = TiffWriter::new(&mut buffer).unwrap();
        let mut root = DirectoryWriter::new();
        let mut exif = DirectoryWriter::new();
        root.add_tag(TiffCommonTag::Make, "Shroom");
        root.add_tag(TiffCommonTag::Model, "Jpeg Cam");
        root.add_tag(ExifTag::Orientation, orientation);
        exif.add_tag(ExifTag::DateTimeOriginal, time);
        exif.add_tag(ExifTag::SubSecTimeOriginal, subsec);
        exif.add_tag(ExifTag::ISOSpeedRatings, 400u16);
        exif.add_tag(ExifTag::FNumber, rawler::formats::tiff::Rational::new(56, 10));
        let offset = exif.build(&mut tiff).unwrap();
        root.add_tag(TiffCommonTag::ExifIFDPointer, offset);
        tiff.build(root).unwrap();
        buffer.into_inner()
    }

    fn flat(w: u32, h: u32, rgb: [u8; 3]) -> image::RgbImage {
        image::RgbImage::from_pixel(w, h, image::Rgb(rgb))
    }

    #[test]
    fn recognises_jpeg_extensions() {
        for name in ["a.jpg", "b.JPEG", "c.Jpe"] {
            assert!(is_jpeg_file(Path::new(name)), "{name}");
        }
        for name in ["a.dng", "b.png", "jpg", "c.jpg.xmp"] {
            assert!(!is_jpeg_file(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn camera_names() {
        let c = |make, model| clean_camera(make, model);
        assert_eq!(c("Canon", "Canon EOS R5"), ("Canon".into(), "EOS R5".into()));
        assert_eq!(c("NIKON CORPORATION", "NIKON Z 8"), ("NIKON".into(), "Z 8".into()));
        assert_eq!(c("SONY", "ILCE-7RM5"), ("SONY".into(), "ILCE-7RM5".into()));
        assert_eq!(c("", "Phone"), ("".into(), "Phone".into()));
    }

    #[test]
    fn srgb_decoding() {
        assert_eq!(SRGB_TO_LINEAR[0], 0.0);
        assert!((SRGB_TO_LINEAR[255] - 1.0).abs() < 1e-6);
        // Code 118 is about 18 % grey.
        assert!((SRGB_TO_LINEAR[118] - 0.1812).abs() < 1e-3);
        for i in 0..=255u8 {
            let back = (crate::render::srgb_encode(SRGB_TO_LINEAR[i as usize]) * 255.0 + 0.5) as u8;
            assert_eq!(back, i);
        }
    }

    #[test]
    fn reads_info_and_linear_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frame.jpg");
        write_test_jpeg(&path, &flat(40, 24, [118, 118, 118]), Some(camera_exif("2026:10:20 14:20:38", "25", 6)));

        let info = crate::io::read_frame_info(&path).unwrap();
        assert!(info.jpeg && !info.mosaic && !info.is_result());
        assert_eq!((info.width, info.height), (40, 24));
        assert_eq!(info.camera, "Shroom Jpeg Cam");
        assert_eq!(info.orientation, crate::Orientation::Cw90);
        let time = info.capture_time.unwrap().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        assert_eq!(time, "2026-10-20 14:20:38.250");
        assert_eq!(info.exposure.iso, Some(400));
        assert!((info.exposure.f_number.unwrap() - 5.6).abs() < 1e-6);
        let preview = info.preview.unwrap();
        assert_eq!((preview.width, preview.height), (40, 24));
        assert_eq!(info.color, ColorInfo::srgb());

        let frame = crate::io::load_linear(&path).unwrap();
        assert_eq!((frame.image.width(), frame.image.height()), (40, 24));
        let p = frame.image.pixel(20, 12);
        assert!(p.iter().all(|v| (v - 0.1812).abs() < 0.01), "{p:?}");
        assert_eq!(frame.template.orientation(), crate::Orientation::Cw90);
        // Stacking loads JPEGs as RGB.
        assert!(matches!(crate::io::load_for_stacking(&path).unwrap(), crate::stack::SourceFrame::Rgb(_)));
    }

    #[test]
    fn jpeg_without_exif() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.jpeg");
        write_test_jpeg(&path, &flat(8, 8, [10, 200, 30]), None);
        let info = crate::io::read_frame_info(&path).unwrap();
        assert_eq!(info.capture_time, None);
        assert_eq!(info.camera, "");
        assert_eq!(info.orientation, crate::Orientation::Normal);
    }

    #[test]
    fn corrupt_jpeg_is_decode_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.jpg");
        std::fs::write(&path, [0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 1, 2]).unwrap();
        assert!(matches!(crate::io::load_linear(&path).unwrap_err(), Error::Decode { .. }));
    }

    #[test]
    fn identity_rendering_roundtrip() {
        // A JPEG frame rendered for display gives back its pixels.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("colours.jpg");
        let mut img = image::RgbImage::new(16, 16);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([(x * 16) as u8, (y * 16) as u8, 128]);
        }
        write_test_jpeg(&path, &img, None);
        let decoded = image::open(&path).unwrap().into_rgb8();
        let frame = crate::io::load_linear(&path).unwrap();
        let display = render_display(&frame.image, &frame.color, None);
        let max = display.data.iter().zip(decoded.as_raw()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(max <= 1, "max difference {max}");
    }

    #[test]
    fn jpeg_result_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.jpg");
        write_test_jpeg(&src, &flat(32, 20, [200, 120, 40]), Some(camera_exif("2026:10:20 14:20:38", "5", 1)));
        let frame = crate::io::load_linear(&src).unwrap();
        let settings = crate::stack::StackSettings::new(crate::stack::Method::DEFAULT_PYRAMID);
        let template = frame
            .template
            .with_orientation(crate::Orientation::Rotate180)
            .with_sources(&[src.clone(), dir.path().join("b.jpg")])
            .with_settings(&settings);

        let out = dir.path().join("result.jpg");
        write_jpeg(&out, &frame.image, &template, 95, "Shroom Stack test").unwrap();
        assert!(!dir.path().join("result.jpg.part").exists());

        let info = crate::io::read_frame_info(&out).unwrap();
        assert!(info.is_result());
        assert_eq!(info.sources, vec!["src.jpg", "b.jpg"]);
        assert_eq!(info.settings, Some(settings));
        assert_eq!(info.orientation, crate::Orientation::Rotate180);
        assert_eq!(info.camera, "Shroom Jpeg Cam");
        assert!(info.capture_time.is_some());
        let back = crate::io::load_linear(&out).unwrap().image;
        let (a, b) = (frame.image.pixel(16, 10), back.pixel(16, 10));
        assert!(a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.01), "{a:?} vs {b:?}");
    }

    #[test]
    fn raw_template_renders_like_display() {
        // Results from raw frames are rendered with white balance and colour matrix.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("raw.jpg");
        let mut img = RgbImage::new(8, 8);
        (0..8).for_each(|y| (0..8).for_each(|x| img.set_pixel(x, y, [0.1, 0.2, 0.13])));
        let template = synthetic_template();
        write_jpeg(&out, &img, &template, 100, "test").unwrap();
        let expected = render_display(&img, &template.color(), None);
        let back = image::open(&out).unwrap().into_rgb8();
        let max = expected.data.iter().zip(back.as_raw()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(max <= 2, "max difference {max}");
        let info = crate::io::read_frame_info(&out).unwrap();
        assert_eq!(info.camera, "Shroom Test Cam");
        // Without sources or settings it isn't recognisable as a result.
        assert!(!info.is_result());
    }

    #[test]
    fn jpeg_frames_make_linear_dng_results() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.jpg");
        write_test_jpeg(&src, &flat(16, 8, [90, 160, 220]), Some(camera_exif("2026:01:02 03:04:05", "0", 8)));
        let frame = crate::io::load_linear(&src).unwrap();
        let out = dir.path().join("out.dng");
        crate::io::write_linear_dng(&out, &frame.image, &frame.template, "test").unwrap();
        let back = crate::io::load_linear(&out).unwrap();
        let max = frame.image.data().iter().zip(back.image.data()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(max <= 1.0 / 65535.0, "{max}");
        // Rendering the DNG gives the JPEG's colours.
        let m = back.color.cam_to_srgb();
        for (i, row) in m.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                assert!((v - if i == j { 1.0 } else { 0.0 }).abs() < 2e-3, "{m:?}");
            }
        }
        assert_eq!(back.template.orientation(), crate::Orientation::Cw270);
        let info = crate::io::read_frame_info(&out).unwrap();
        assert_eq!(info.camera, "Shroom Jpeg Cam");
        assert!(info.capture_time.is_some());
    }

    #[test]
    fn xmp_goes_after_app_segments() {
        let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 9, 9, 0xFF, 0xDB, 1, 2];
        let out = insert_xmp(jpeg, "<x/>");
        assert_eq!(&out[..8], &[0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 9, 9]);
        assert_eq!(&out[8..10], &[0xFF, APP1]);
        assert_eq!(&out[out.len() - 4..], &[0xFF, 0xDB, 1, 2]);
        // Too large for a segment: left out.
        let big = "x".repeat(70_000);
        assert_eq!(insert_xmp(vec![0xFF, 0xD8, 0xFF, 0xD9], &big).len(), 4);
    }
}
