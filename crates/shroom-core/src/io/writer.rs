use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use rawler::RawImageData;
use rawler::dng::writer::DngWriter;
use rawler::dng::{CropMode, DNG_VERSION_V1_4, DngCompression, DngPhotometricConversion};
use rawler::rawimage::{BlackLevel, RawPhotometricInterpretation, WhiteLevel};
use rawler::tags::{ExifTag, TiffCommonTag};

use super::reader::DngTemplate;
use crate::error::{Error, Result};
use crate::image::RgbImage;
use crate::render::render_display;

const PREVIEW_SIZE: usize = 1024;
const PREVIEW_QUALITY: f32 = 0.85;
/// LJ92 predictor 1 (left neighbour), the most widely supported choice.
const LJ92_PREDICTOR: u8 = 1;

/// Writes `image` as a losslessly compressed Linear DNG with camera, colour and EXIF
/// metadata taken from `template`.
///
/// The file is written to a temporary name first and renamed on success, so a failed
/// write never leaves a truncated file at `path`.
pub fn write_linear_dng(path: &Path, image: &RgbImage, template: &DngTemplate, software: &str) -> Result<()> {
    let tmp = path.with_extension("dng.part");
    let result = write_to(&tmp, image, template, software).and_then(|()| {
        std::fs::rename(&tmp, path).map_err(|source| Error::Io {
            path: path.into(),
            source,
        })
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn write_to(path: &Path, image: &RgbImage, template: &DngTemplate, software: &str) -> Result<()> {
    let enc_err = |e: rawler::dng::writer::DngError| Error::Encode {
        path: path.into(),
        message: e.to_string(),
    };

    let mut raw = template.raw.clone();
    raw.width = image.width();
    raw.height = image.height();
    raw.cpp = 3;
    raw.bps = 16;
    raw.photometric = RawPhotometricInterpretation::LinearRaw;
    raw.whitelevel = WhiteLevel::new(vec![u16::MAX as u32; 3]);
    raw.blacklevel = BlackLevel::new(&[0u32, 0, 0], 1, 1, 3);
    raw.active_area = None;
    raw.crop_area = None;
    raw.blackareas.clear();
    raw.data = RawImageData::Integer(
        image
            .data()
            .iter()
            .map(|v| (v.clamp(0.0, 1.0) * u16::MAX as f32).round() as u16)
            .collect(),
    );

    let display = render_display(image, &template.color(), Some(PREVIEW_SIZE));
    let preview = image::RgbImage::from_raw(display.width as u32, display.height as u32, display.data)
        .map(image::DynamicImage::ImageRgb8)
        .expect("display image size is consistent");

    let file = File::create(path).map_err(|source| Error::Io {
        path: path.into(),
        source,
    })?;
    let mut dng = DngWriter::new(BufWriter::new(file), DNG_VERSION_V1_4).map_err(enc_err)?;

    let mut main = dng.subframe(0);
    main.raw_image(
        &raw,
        CropMode::None,
        DngCompression::Lossless,
        DngPhotometricConversion::Original,
        LJ92_PREDICTOR,
    )
    .map_err(enc_err)?;
    main.ifd_mut().copy(template.raw_tags.iter().map(|(t, v)| (t, v)));
    main.finalize().map_err(enc_err)?;

    let mut prev = dng.subframe(1);
    prev.preview(&preview, PREVIEW_QUALITY).map_err(enc_err)?;
    prev.finalize().map_err(enc_err)?;
    dng.thumbnail(&preview).map_err(enc_err)?;

    dng.load_base_tags(&raw).map_err(enc_err)?;
    dng.load_metadata(&template.metadata).map_err(enc_err)?;
    dng.root_ifd_mut()
        .add_tag(ExifTag::Orientation, raw.orientation.to_u16());
    dng.root_ifd_mut().copy(template.root_tags.iter().map(|(t, v)| (t, v)));
    dng.root_ifd_mut().add_tag(TiffCommonTag::Software, software);
    dng.root_ifd_mut().add_tag(
        ExifTag::ModifyDate,
        chrono::Local::now().format("%Y:%m:%d %H:%M:%S").to_string(),
    );
    dng.close().map_err(enc_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::load_linear;
    use crate::io::reader::tests::synthetic_template;

    fn gradient(w: usize, h: usize) -> RgbImage {
        let mut img = RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / (w - 1) as f32, y as f32 / (h - 1) as f32);
                img.set_pixel(x, y, [fx, fy, 0.5 * (fx + fy)]);
            }
        }
        img
    }

    #[test]
    fn linear_dng_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.dng");
        let img = gradient(37, 23);
        let template = synthetic_template();
        write_linear_dng(&path, &img, &template, "Shroom Stack test").unwrap();
        assert!(!path.with_extension("dng.part").exists());

        let back = load_linear(&path).unwrap();
        assert_eq!((back.image.width(), back.image.height()), (37, 23));
        let max_err = img
            .data()
            .iter()
            .zip(back.image.data())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err <= 1.0 / 65535.0, "max error {max_err}");
        // Colour data is stored as rationals (1/10000 for matrices), so allow rounding.
        let (got, want) = (back.color, template.color());
        let close = |a: f32, b: f32| (a - b).abs() < 2e-4;
        assert!(got.wb.iter().zip(want.wb).all(|(a, b)| close(*a, b)), "{got:?}");
        let (gm, wm) = (got.xyz_to_cam.unwrap(), want.xyz_to_cam.unwrap());
        assert!(
            gm.as_flattened()
                .iter()
                .zip(wm.as_flattened())
                .all(|(a, b)| close(*a, *b)),
            "{gm:?}"
        );
        assert_eq!(back.template.raw.clean_make, "Shroom");
    }

    #[test]
    fn out_of_range_values_are_clipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.dng");
        let mut img = RgbImage::new(4, 4);
        img.set_pixel(0, 0, [-0.5, 1.5, 0.25]);
        write_linear_dng(&path, &img, &synthetic_template(), "test").unwrap();
        let back = load_linear(&path).unwrap().image;
        assert_eq!(back.pixel(0, 0)[0], 0.0);
        assert_eq!(back.pixel(0, 0)[1], 1.0);
        assert!((back.pixel(0, 0)[2] - 0.25).abs() < 1e-4);
    }

    #[test]
    fn failed_write_leaves_no_file() {
        let path = Path::new("/nonexistent-dir/out.dng");
        assert!(write_linear_dng(path, &gradient(4, 4), &synthetic_template(), "test").is_err());
    }
}
