//! Adobe's per-camera values that raw files don't contain: brightness correction
//! (BaselineExposure), calibration and forward matrices. Lightroom recognises the camera
//! by `UniqueCameraModel` and applies its own profile, but uses these tags from the file;
//! without them, results from raw files look darker and slightly different in colour.
//!
//! Values come from a built-in table (`cameras.json`) and are learned automatically from
//! every Lightroom/Adobe DNG the app reads; learned values are stored in the user's
//! configuration directory and take precedence.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use rawler::formats::tiff::{Rational, SRational, TiffAscii, Value};
use serde::{Deserialize, Serialize};

/// DNG tags handled here.
mod tag {
    pub const BASELINE_EXPOSURE: u16 = 50730;
    pub const BASELINE_NOISE: u16 = 50731;
    pub const BASELINE_SHARPNESS: u16 = 50732;
    pub const CAMERA_CALIBRATION: [u16; 2] = [50723, 50724];
    pub const CAMERA_CALIBRATION_SIGNATURE: u16 = 50931;
    pub const FORWARD_MATRIX: [u16; 2] = [50964, 50965];
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraValues {
    /// Black and white level of Adobe's raw data, to relate BaselineExposure to ours.
    pub black_level: f32,
    pub white_level: f32,
    pub baseline_exposure: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_noise: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_sharpness: Option<f32>,
    /// CameraCalibration1 and 2 (row-major 3×3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration: Option<[Vec<f32>; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration_signature: Option<String>,
    /// ForwardMatrix1 and 2 (row-major 3×3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_matrices: Option<[Vec<f32>; 2]>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CameraStore {
    /// By `UniqueCameraModel` (e.g. "Canon EOS R5").
    pub cameras: BTreeMap<String, CameraValues>,
}

fn f32_of(value: &Value) -> Vec<f32> {
    match value {
        Value::SRational(v) => v.iter().map(|r| r.n as f32 / r.d as f32).collect(),
        Value::Rational(v) => v.iter().map(|r| r.n as f32 / r.d as f32).collect(),
        Value::Short(v) => v.iter().map(|&x| x as f32).collect(),
        Value::Long(v) => v.iter().map(|&x| x as f32).collect(),
        _ => Vec::new(),
    }
}

fn srational(values: &[f32]) -> Value {
    Value::SRational(values.iter().map(|v| SRational::new((v * 10_000.0).round() as i32, 10_000)).collect())
}

fn rational(value: f32) -> Value {
    Value::Rational(vec![Rational::new((value * 10_000.0).round() as u32, 10_000)])
}

impl CameraValues {
    /// Extracts the values from the root tags of an Adobe-made raw DNG.
    pub fn from_dng_tags(tags: &[(u16, Value)], black_level: f32, white_level: f32) -> Option<Self> {
        let get = |t: u16| tags.iter().find(|(k, _)| *k == t).map(|(_, v)| v);
        let scalar = |t| get(t).and_then(|v| f32_of(v).first().copied());
        let pair = |ts: [u16; 2]| -> Option<[Vec<f32>; 2]> {
            let (a, b) = (f32_of(get(ts[0])?), f32_of(get(ts[1])?));
            (a.len() == 9 && b.len() == 9).then_some([a, b])
        };
        Some(Self {
            black_level,
            white_level,
            baseline_exposure: scalar(tag::BASELINE_EXPOSURE)?,
            baseline_noise: scalar(tag::BASELINE_NOISE),
            baseline_sharpness: scalar(tag::BASELINE_SHARPNESS),
            calibration: pair(tag::CAMERA_CALIBRATION),
            calibration_signature: get(tag::CAMERA_CALIBRATION_SIGNATURE).and_then(|v| match v {
                Value::Ascii(a) => a.strings().first().cloned(),
                _ => None,
            }),
            forward_matrices: pair(tag::FORWARD_MATRIX),
        })
    }

    /// DNG tags for a file whose raw data is normalised with `black_level` and
    /// `white_level`; BaselineExposure is corrected for the different normalisation, so
    /// the file looks as bright as Adobe's DNG of the same raw data.
    pub fn to_dng_tags(&self, black_level: f32, white_level: f32) -> Vec<(u16, Value)> {
        // Our values are larger by this factor than Adobe's for the same raw value.
        let factor = (self.white_level - self.black_level) / (white_level - black_level);
        let mut tags = vec![(tag::BASELINE_EXPOSURE, srational(&[self.baseline_exposure - factor.log2()]))];
        if let Some(v) = self.baseline_noise {
            tags.push((tag::BASELINE_NOISE, rational(v)));
        }
        if let Some(v) = self.baseline_sharpness {
            tags.push((tag::BASELINE_SHARPNESS, rational(v)));
        }
        if let Some([c1, c2]) = &self.calibration {
            tags.push((tag::CAMERA_CALIBRATION[0], srational(c1)));
            tags.push((tag::CAMERA_CALIBRATION[1], srational(c2)));
        }
        if let Some(sig) = &self.calibration_signature {
            tags.push((tag::CAMERA_CALIBRATION_SIGNATURE, Value::Ascii(TiffAscii::new(sig))));
        }
        if let Some([f1, f2]) = &self.forward_matrices {
            tags.push((tag::FORWARD_MATRIX[0], srational(f1)));
            tags.push((tag::FORWARD_MATRIX[1], srational(f2)));
        }
        tags
    }
}

const BUILT_IN: &str = include_str!("cameras.json");

fn learned_path() -> Option<PathBuf> {
    // Unit tests must not touch the user's configuration.
    if cfg!(test) {
        return None;
    }
    if let Some(path) = std::env::var_os("SHROOM_CAMERA_STORE") {
        return Some(PathBuf::from(path));
    }
    Some(dirs::config_dir()?.join("shroom-stack").join("cameras.json"))
}

fn load(path: &std::path::Path) -> CameraStore {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Built-in table plus learned values (cached; learned values win).
fn store() -> &'static Mutex<(CameraStore, CameraStore)> {
    static STORE: std::sync::OnceLock<Mutex<(CameraStore, CameraStore)>> = std::sync::OnceLock::new();
    STORE.get_or_init(|| {
        let built_in = serde_json::from_str(BUILT_IN).expect("valid built-in camera table");
        let learned = learned_path().map(|p| load(&p)).unwrap_or_default();
        Mutex::new((built_in, learned))
    })
}

/// Values for a camera, if known.
pub fn lookup(model: &str) -> Option<CameraValues> {
    let guard = store().lock().unwrap();
    guard.1.cameras.get(model).or_else(|| guard.0.cameras.get(model)).cloned()
}

/// Remembers values read from an Adobe DNG; writes the learned file only on changes.
pub fn learn(model: &str, values: CameraValues) {
    let mut guard = store().lock().unwrap();
    let known = guard.1.cameras.get(model).or_else(|| guard.0.cameras.get(model));
    if known == Some(&values) {
        return;
    }
    guard.1.cameras.insert(model.to_string(), values);
    if let Some(path) = learned_path() {
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
        if let Ok(json) = serde_json::to_string_pretty(&guard.1) {
            let _ = std::fs::write(path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r5() -> CameraValues {
        CameraValues {
            black_level: 2048.0,
            white_level: 16383.0,
            baseline_exposure: 0.39,
            baseline_noise: Some(0.8),
            baseline_sharpness: Some(1.25),
            calibration: Some(std::array::from_fn(|_| vec![1.017, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])),
            calibration_signature: Some("com.adobe".into()),
            forward_matrices: Some([vec![0.5; 9], vec![0.25; 9]]),
        }
    }

    #[test]
    fn tags_roundtrip() {
        let v = r5();
        let tags = v.to_dng_tags(2048.0, 16383.0);
        let back = CameraValues::from_dng_tags(&tags, 2048.0, 16383.0).unwrap();
        assert!((back.baseline_exposure - 0.39).abs() < 1e-4);
        assert_eq!(back.calibration_signature.as_deref(), Some("com.adobe"));
        assert!((back.calibration.as_ref().unwrap()[0][0] - 1.017).abs() < 1e-4);
        assert_eq!(back.forward_matrices.as_ref().unwrap()[1][4], 0.25);
    }

    #[test]
    fn baseline_exposure_compensates_normalisation() {
        // rawler normalises the R5's CR3 to its clipping point 14888: values are 11.65 %
        // larger than Adobe's, so the file needs 0.159 EV less baseline exposure.
        let tags = r5().to_dng_tags(2047.0, 14888.0);
        let be = f32_of(&tags[0].1)[0];
        let expected = 0.39 - ((16383.0f32 - 2048.0) / (14888.0 - 2047.0)).log2();
        assert!((be - expected).abs() < 1e-4, "{be} vs {expected}");
        assert!((expected - 0.231).abs() < 0.002);
    }

    #[test]
    fn needs_baseline_exposure() {
        assert!(CameraValues::from_dng_tags(&[], 0.0, 1.0).is_none());
    }

    #[test]
    fn built_in_table_parses() {
        let table: CameraStore = serde_json::from_str(BUILT_IN).unwrap();
        for (model, values) in &table.cameras {
            assert!(values.white_level > values.black_level, "{model}");
        }
    }

    #[test]
    fn store_serialises() {
        let mut store = CameraStore::default();
        store.cameras.insert("Canon EOS R5".into(), r5());
        let json = serde_json::to_string(&store).unwrap();
        assert_eq!(serde_json::from_str::<CameraStore>(&json).unwrap(), store);
    }
}
