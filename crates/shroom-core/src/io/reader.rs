use std::path::Path;

use chrono::NaiveDateTime;
use rawler::decoders::{Decoder, RawDecodeParams, RawMetadata, WellKnownIFD};
use rawler::formats::tiff::Value;
use rawler::imgop::xyz::Illuminant;
use rawler::rawimage::RawPhotometricInterpretation;
use rawler::rawsource::RawSource;
use rawler::{RawImage, RawImageData};
use rayon::prelude::*;

use super::FrameInfo;
use crate::error::{Error, Result};
use crate::image::{DisplayImage, Plane, RgbImage};
use crate::raw::{CfaPattern, demosaic};
use crate::render::ColorInfo;

/// DNG tags of the raw IFD that are still meaningful after demosaicing.
const KEPT_RAW_TAGS: [u16; 2] = [
    50738, // AntiAliasStrength
    51041, // NoiseProfile
];

/// TIFF tag holding the XMP packet.
const XMP_TAG: u16 = 700;

/// Root IFD tags describing camera colour calibration and the embedded camera profile.
/// They are copied verbatim so raw converters render the result exactly like the source;
/// in particular the profile's hue/sat and look tables only give the intended colours
/// together with the ForwardMatrix tags.
const COLOR_ROOT_TAGS: &[u16] = &[
    50721, 50722, // ColorMatrix1/2
    50723, 50724, // CameraCalibration1/2
    50725, 50726, // ReductionMatrix1/2
    50727, // AnalogBalance
    50728, // AsShotNeutral
    50729, // AsShotWhiteXY
    50730, // BaselineExposure
    50731, // BaselineNoise
    50732, // BaselineSharpness
    50734, // LinearResponseLimit
    50735, // CameraSerialNumber
    50736, // LensInfo
    50739, // ShadowScale
    50778, 50779, // CalibrationIlluminant1/2
    50879, // ColorimetricReference
    50931, // CameraCalibrationSignature
    50932, // ProfileCalibrationSignature
    50936, 50937, 50938, 50939, 50940, 50941, 50942, // ProfileName … ProfileCopyright
    50964, 50965, // ForwardMatrix1/2
    50981, 50982, // ProfileLookTableDims/Data
    51041, // NoiseProfile
    51107, 51108, // ProfileHueSatMapEncoding, ProfileLookTableEncoding
    51109, // BaselineExposureOffset
    51110, // DefaultBlackRender
    52525, // ProfileGainTableMap
    52529, 52530, 52531, 52532, 52533, 52534, 52535, // third illuminant, IlluminantData1-3
    52537, 52538, // ProfileHueSatMapData3, ReductionMatrix3
    52543, // RGBTables
    52551, 52552, // ProfileDynamicRange, ProfileGroupName
];

/// Everything from a source file needed to write a result DNG with the same camera,
/// colour calibration and EXIF data. Opaque to users of the crate.
#[derive(Clone)]
pub struct DngTemplate {
    /// Decoded image description with the pixel data removed.
    pub(super) raw: RawImage,
    pub(super) metadata: RawMetadata,
    pub(super) root_tags: Vec<(u16, Value)>,
    pub(super) raw_tags: Vec<(u16, Value)>,
    /// XMP packet of the source (labels, lens, develop settings), if any.
    pub(super) xmp: Option<String>,
}

impl std::fmt::Debug for DngTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DngTemplate")
            .field("make", &self.raw.clean_make)
            .field("model", &self.raw.clean_model)
            .finish_non_exhaustive()
    }
}

impl DngTemplate {
    pub fn color(&self) -> ColorInfo {
        color_info(&self.raw)
    }

    pub fn orientation(&self) -> crate::Orientation {
        crate::Orientation::from_exif(self.raw.orientation.to_u16())
    }

    /// The same template, writing `orientation` into result files.
    pub fn with_orientation(&self, orientation: crate::Orientation) -> Self {
        let mut t = self.clone();
        t.raw.orientation = rawler::Orientation::from_u16(orientation.to_exif());
        t
    }
}

/// A Bayer raw frame, cropped to the active area and normalised to `0..=1`.
#[derive(Clone, Debug)]
pub struct RawFrame {
    pub mosaic: Plane,
    pub pattern: CfaPattern,
    pub color: ColorInfo,
    pub template: DngTemplate,
}

/// A frame as linear camera RGB, normalised to `0..=1`.
#[derive(Clone, Debug)]
pub struct LinearFrame {
    pub image: RgbImage,
    pub color: ColorInfo,
    pub template: DngTemplate,
}

struct Opened {
    source: RawSource,
    decoder: Box<dyn Decoder>,
}

fn open(path: &Path) -> Result<Opened> {
    let source = RawSource::new(path).map_err(|source| Error::Io { path: path.into(), source })?;
    let decoder = rawler::get_decoder(&source).map_err(|e| decode_err(path, e))?;
    Ok(Opened { source, decoder })
}

fn decode_err(path: &Path, e: impl std::fmt::Display) -> Error {
    Error::Decode { path: path.into(), message: e.to_string() }
}

pub(super) fn read_frame_info(path: &Path) -> Result<FrameInfo> {
    let Opened { source, decoder } = open(path)?;
    let params = RawDecodeParams::default();
    let metadata = decoder.raw_metadata(&source, &params).map_err(|e| decode_err(path, e))?;
    // A dummy decode parses the image layout without decompressing pixel data.
    let raw = decoder.raw_image(&source, &params, true).map_err(|e| decode_err(path, e))?;
    let (width, height) = raw.active_area.map(|a| (a.d.w, a.d.h)).unwrap_or((raw.width, raw.height));
    let camera_values_known = camera_values(decoder.as_ref(), &raw).1;
    let preview = decoder.preview_image(&source, &params).ok().flatten().map(|img| {
        // Some formats embed full-size previews (CR3: 8192 px); thumbnails don't need that.
        let img =
            if img.width().max(img.height()) > MAX_PREVIEW { img.thumbnail(MAX_PREVIEW, MAX_PREVIEW) } else { img };
        let rgb = img.to_rgb8();
        DisplayImage { width: rgb.width() as usize, height: rgb.height() as usize, data: rgb.into_raw() }
    });
    Ok(FrameInfo {
        path: path.into(),
        file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        file_modified: std::fs::metadata(path).and_then(|m| m.modified()).ok(),
        capture_time: capture_time(&metadata),
        camera: format!("{} {}", raw.clean_make, raw.clean_model).trim().to_string(),
        width,
        height,
        preview,
        color: color_info(&raw),
        orientation: crate::Orientation::from_exif(raw.orientation.to_u16()),
        camera_values_known,
        mosaic: matches!(raw.photometric, RawPhotometricInterpretation::Cfa(_)),
        exposure: exposure(&metadata),
    })
}

fn exposure(metadata: &RawMetadata) -> super::Exposure {
    let e = &metadata.exif;
    let ratio = |r: &rawler::formats::tiff::Rational| (r.d != 0).then(|| r.n as f32 / r.d as f32);
    super::Exposure {
        time: e.exposure_time.as_ref().and_then(ratio),
        f_number: e.fnumber.as_ref().and_then(ratio),
        iso: e.iso_speed.or(e.iso_speed_ratings.map(u32::from)),
        focal_length: e.focal_length.as_ref().and_then(ratio),
    }
}

fn capture_time(metadata: &RawMetadata) -> Option<NaiveDateTime> {
    let base = metadata.exif.date_time_original.as_deref()?;
    let subsec = metadata.exif.sub_sec_time_original.as_deref().unwrap_or("0");
    NaiveDateTime::parse_from_str(&format!("{base}.{}", subsec.trim()), "%Y:%m:%d %H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(base, "%Y:%m:%d %H:%M:%S"))
        .ok()
}

/// Longer side of previews kept in [`FrameInfo`].
const MAX_PREVIEW: u32 = 1024;

/// TIFF tag present in every DNG.
const DNG_VERSION_TAG: u16 = 50706;

fn root_entries(decoder: &dyn Decoder, filter: &dyn Fn(u16) -> bool) -> Vec<(u16, Value)> {
    decoder
        .ifd(WellKnownIFD::Root)
        .ok()
        .flatten()
        .map(|ifd| ifd.value_iter().filter(|(t, _)| filter(**t)).map(|(t, v)| (*t, v.clone())).collect())
        .unwrap_or_default()
}

/// Key of a camera in the Adobe value store; matches `UniqueCameraModel` of our files.
fn camera_key(raw: &RawImage) -> String {
    format!("{} {}", raw.clean_make, raw.clean_model)
}

/// Learns Adobe's camera values from an Adobe-made raw DNG, or looks them up for other
/// raw formats. Returns tags to add to result files (empty for DNGs, which carry them),
/// and whether values are available.
fn camera_values(decoder: &dyn Decoder, raw: &RawImage) -> (Vec<(u16, Value)>, bool) {
    let is_dng = !root_entries(decoder, &|t| t == DNG_VERSION_TAG).is_empty();
    let black = raw.blacklevel.levels.first().map(|r| r.as_f32()).unwrap_or(0.0);
    let white = raw.whitelevel.0.first().copied().unwrap_or(u16::MAX as u32) as f32;
    let cfa = matches!(raw.photometric, RawPhotometricInterpretation::Cfa(_));
    if is_dng {
        // Only raw (CFA) DNGs: linear DNGs, e.g. our own results, use another normalisation.
        if cfa
            && let Some(values) = super::cameras::CameraValues::from_dng_tags(
                &root_entries(decoder, &|t| COLOR_ROOT_TAGS.contains(&t)),
                black,
                white,
            )
        {
            super::cameras::learn(&camera_key(raw), values);
        }
        return (Vec::new(), true);
    }
    match super::cameras::lookup(&camera_key(raw)) {
        Some(values) => (values.to_dng_tags(black, white), true),
        None => (Vec::new(), false),
    }
}

/// Minimal XMP with the lens fields Lightroom uses to find lens profiles, for raw formats
/// without XMP of their own.
fn lens_xmp(metadata: &RawMetadata) -> Option<String> {
    let lens = metadata.lens.as_ref()?;
    let name = lens.identifiers.name.as_deref()?;
    let escape = |s: &str| s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;");
    let id = lens.identifiers.id.map(|(id, _)| format!(" aux:LensID=\"{id}\"")).unwrap_or_default();
    Some(format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
         <rdf:Description rdf:about=\"\" xmlns:aux=\"http://ns.adobe.com/exif/1.0/aux/\" \
         xmlns:exifEX=\"http://cipa.jp/exif/1.0/\" aux:Lens=\"{n}\"{id} exifEX:LensModel=\"{n}\"/>\
         </rdf:RDF></x:xmpmeta>",
        n = escape(name)
    ))
}

/// Decodes the full image data and the metadata needed to write a result file.
fn decode_full(path: &Path) -> Result<(RawImage, DngTemplate)> {
    let Opened { source, decoder } = open(path)?;
    let params = RawDecodeParams::default();
    let mut metadata = decoder.raw_metadata(&source, &params).map_err(|e| decode_err(path, e))?;
    let mut raw = decoder.raw_image(&source, &params, false).map_err(|e| decode_err(path, e))?;
    // rawler replaces the lens name with its database spelling; Lightroom matches lens
    // profiles by the name the camera wrote.
    if let Some(name) = metadata.lens.as_ref().and_then(|l| l.identifiers.name.clone()) {
        metadata.exif.lens_model = Some(name);
    }
    let ifd_tags = |which, filter: &dyn Fn(u16) -> bool| -> Result<Vec<(u16, Value)>> {
        Ok(decoder
            .ifd(which)
            .map_err(|e| decode_err(path, e))?
            .map(|ifd| {
                ifd.value_iter().filter(|(tag, _)| filter(**tag)).map(|(tag, value)| (*tag, value.clone())).collect()
            })
            .unwrap_or_default())
    };
    let mut root_tags = ifd_tags(WellKnownIFD::Root, &|t| COLOR_ROOT_TAGS.contains(&t))?;
    for (tag, value) in ifd_tags(WellKnownIFD::VirtualDngRootTags, &|_| true)? {
        if !root_tags.iter().any(|(t, _)| *t == tag) {
            root_tags.push((tag, value));
        }
    }
    let raw_tags = ifd_tags(WellKnownIFD::VirtualDngRawTags, &|t| KEPT_RAW_TAGS.contains(&t))?;
    for (tag, value) in camera_values(decoder.as_ref(), &raw).0 {
        if !root_tags.iter().any(|(t, _)| *t == tag) {
            root_tags.push((tag, value));
        }
    }
    // rawler's DNG decoder doesn't implement `xpacket`, so read the XMP tag directly.
    let xmp = ifd_tags(WellKnownIFD::Root, &|t| t == XMP_TAG)?
        .into_iter()
        .find_map(|(_, value)| match value {
            Value::Byte(bytes) | Value::Undefined(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
            _ => None,
        })
        .or_else(|| lens_xmp(&metadata));
    let data = std::mem::replace(&mut raw.data, RawImageData::Integer(Vec::new()));
    let template = DngTemplate { raw: raw.clone(), metadata, root_tags, raw_tags, xmp };
    raw.data = data;
    Ok((raw, template))
}

/// Reads the metadata needed to write a result DNG based on this file.
pub fn read_template(path: &Path) -> Result<DngTemplate> {
    Ok(decode_full(path)?.1)
}

/// Decodes a Bayer DNG into a normalised mosaic.
pub fn decode_raw(path: &Path) -> Result<RawFrame> {
    let (raw, template) = decode_full(path)?;
    cfa_frame(path, raw, template)
}

/// Normalises decoded Bayer data into a [`RawFrame`].
fn cfa_frame(path: &Path, raw: RawImage, template: DngTemplate) -> Result<RawFrame> {
    let RawPhotometricInterpretation::Cfa(config) = &raw.photometric else {
        return Err(Error::Unsupported { path: path.into(), message: "not a Bayer raw image".into() });
    };
    if raw.cpp != 1 {
        return Err(Error::Unsupported { path: path.into(), message: format!("{} samples per CFA pixel", raw.cpp) });
    }
    let (ax, ay, w, h) = raw.active_area.map(|a| (a.p.x, a.p.y, a.d.w, a.d.h)).unwrap_or((0, 0, raw.width, raw.height));
    let name: String = (0..4)
        .map(|i| match config.cfa.color_at(i / 2, i % 2) {
            0 => 'R',
            1 => 'G',
            2 => 'B',
            _ => '?',
        })
        .collect();
    let pattern = CfaPattern::from_name(&name)
        .ok_or_else(|| Error::Unsupported { path: path.into(), message: format!("CFA pattern {name}") })?
        .shifted(ax, ay);

    let white = raw.whitelevel.0.first().copied().unwrap_or(u16::MAX as u32) as f32;
    let black = &raw.blacklevel;
    let sample: Box<dyn Fn(usize) -> f32 + Sync> = match &raw.data {
        RawImageData::Integer(v) => Box::new(|i| v[i] as f32),
        RawImageData::Float(v) => Box::new(|i| v[i]),
    };
    let mut mosaic = Plane::new(w, h);
    mosaic.data_mut().par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let sy = y + ay;
        for (x, out) in row.iter_mut().enumerate() {
            let sx = x + ax;
            let b = black.levels[(sy % black.height) * black.width + sx % black.width].as_f32();
            *out = (sample(sy * raw.width + sx) - b) / (white - b);
        }
    });
    // Like raw converters, hide hot photosites (see `raw::remove_hot_pixels`).
    crate::raw::remove_hot_pixels(&mut mosaic);
    let color = color_info(&raw);
    Ok(RawFrame { mosaic, pattern, color, template })
}

/// Loads a frame for stacking: Bayer data stays a mosaic (demosaiced by the compute
/// backend), anything else is loaded as linear RGB.
pub(crate) fn load_for_stacking(path: &Path) -> Result<crate::stack::SourceFrame> {
    let (raw, template) = decode_full(path)?;
    if matches!(raw.photometric, RawPhotometricInterpretation::Cfa(_)) {
        let frame = cfa_frame(path, raw, template)?;
        Ok(crate::stack::SourceFrame::Mosaic(frame.mosaic, frame.pattern))
    } else {
        Ok(crate::stack::SourceFrame::Rgb(load_linear(path)?.image))
    }
}

/// Loads a DNG as linear camera RGB, demosaicing Bayer data if necessary.
pub fn load_linear(path: &Path) -> Result<LinearFrame> {
    let (raw, template) = decode_full(path)?;
    match &raw.photometric {
        RawPhotometricInterpretation::Cfa(_) => {
            let frame = cfa_frame(path, raw, template)?;
            Ok(LinearFrame {
                image: demosaic(&frame.mosaic, frame.pattern),
                color: frame.color,
                template: frame.template,
            })
        }
        RawPhotometricInterpretation::LinearRaw if raw.cpp == 3 => {
            let (ax, ay, w, h) =
                raw.active_area.map(|a| (a.p.x, a.p.y, a.d.w, a.d.h)).unwrap_or((0, 0, raw.width, raw.height));
            let samples = raw.data.as_f32();
            let white: Vec<f32> = (0..3)
                .map(|c| *raw.whitelevel.0.get(c).or(raw.whitelevel.0.first()).unwrap_or(&65535) as f32)
                .collect();
            let black = &raw.blacklevel;
            let mut image = RgbImage::new(w, h);
            let out = image.data_mut();
            for y in 0..h {
                for x in 0..w {
                    let (sx, sy) = (x + ax, y + ay);
                    for c in 0..3 {
                        let bi =
                            ((sy % black.height) * black.width + sx % black.width) * black.cpp + c.min(black.cpp - 1);
                        let b = black.levels[bi].as_f32();
                        let v = samples[(sy * raw.width + sx) * 3 + c];
                        out[(y * w + x) * 3 + c] = (v - b) / (white[c] - b);
                    }
                }
            }
            Ok(LinearFrame { image, color: color_info(&raw), template })
        }
        other => Err(Error::Unsupported {
            path: path.into(),
            message: format!("photometric {other:?} with {} samples", raw.cpp),
        }),
    }
}

fn color_info(raw: &RawImage) -> ColorInfo {
    let wb = [raw.wb_coeffs[0], raw.wb_coeffs[1], raw.wb_coeffs[2]];
    let wb = if wb.iter().all(|v| v.is_finite() && *v > 0.0) { wb } else { [1.0; 3] };
    let xyz_to_cam = raw
        .color_matrix
        .get(&Illuminant::D65)
        .or_else(|| raw.color_matrix.values().next())
        .filter(|m| m.len() >= 9)
        .map(|m| [[m[0], m[1], m[2]], [m[3], m[4], m[5]], [m[6], m[7], m[8]]]);
    ColorInfo { wb, xyz_to_cam }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rawler::decoders::Camera;
    use rawler::pixarray::PixU16;
    use rawler::rawimage::{BlackLevel, WhiteLevel};

    /// A minimal linear-RGB template, as if read from a camera file.
    pub(crate) fn synthetic_template() -> DngTemplate {
        let mut cam = Camera {
            make: "Shroom".into(),
            clean_make: "Shroom".into(),
            model: "Test Cam".into(),
            clean_model: "Test Cam".into(),
            ..Default::default()
        };
        cam.color_matrix
            .insert(Illuminant::D65, vec![0.9766, -0.3416, -0.0464, -0.4176, 1.1999, 0.2463, -0.0786, 0.1513, 0.6127]);
        let mut raw = RawImage::new(
            cam,
            PixU16::new_with(vec![0; 6], 6, 1),
            3,
            [2.0, 1.0, 1.5, f32::NAN],
            RawPhotometricInterpretation::LinearRaw,
            Some(BlackLevel::new(&[0u32, 0, 0], 1, 1, 3)),
            Some(WhiteLevel::new(vec![65535, 65535, 65535])),
            false,
        );
        raw.data = RawImageData::Integer(Vec::new());
        DngTemplate { raw, metadata: RawMetadata::default(), root_tags: Vec::new(), raw_tags: Vec::new(), xmp: None }
    }

    #[test]
    fn template_color_info() {
        let c = synthetic_template().color();
        assert_eq!(c.wb, [2.0, 1.0, 1.5]);
        assert!((c.xyz_to_cam.unwrap()[1][1] - 1.1999).abs() < 1e-6);
    }

    #[test]
    fn missing_file_is_io_error() {
        let err = load_linear(Path::new("/nonexistent/shroom.dng")).unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err}");
    }

    #[test]
    fn non_raw_file_is_decode_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("junk.dng");
        std::fs::write(&path, b"definitely not a DNG file").unwrap();
        assert!(matches!(load_linear(&path).unwrap_err(), Error::Decode { .. }));
    }
}
