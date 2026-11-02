//! Sessions: the photos of a folder grouped into stacks.
//!
//! Grouping looks for patterns rather than fixed thresholds. Setting up a new stack always
//! takes longer than the pause between shots of a stack, so each gap between consecutive
//! shots is judged against the gaps around it. Changed camera settings and a changed picture (embedded
//! previews compared at low resolution, where focus differences vanish) add evidence.
//! Every gap gets a probability of being a stack boundary; values near ½ are uncertain
//! and shown for review.
//!
//! The grouping is kept in a session file next to the photos, so reopening the folder
//! restores it and the Lightroom plugin can apply it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::Orientation;
use crate::image::DisplayImage;
use crate::io::{Exposure, FrameInfo};

/// What grouping needs to know about a photo.
#[derive(Clone, Debug)]
pub struct Shot {
    pub path: PathBuf,
    pub capture_time: Option<NaiveDateTime>,
    pub camera: String,
    pub size: (usize, usize),
    pub orientation: Orientation,
    pub exposure: Exposure,
    pub signature: Option<Signature>,
}

impl Shot {
    pub fn from_info(info: &FrameInfo) -> Self {
        Self {
            path: info.path.clone(),
            capture_time: info.capture_time,
            camera: info.camera.clone(),
            size: (info.width, info.height),
            orientation: info.orientation,
            exposure: info.exposure,
            signature: info.preview.as_ref().and_then(Signature::new),
        }
    }
}

const SIGNATURE_W: usize = 32;
const SIGNATURE_H: usize = 24;

/// A picture reduced to a tiny luminance grid with zero mean and unit variance, for
/// telling a new composition from a refocused frame of the same one.
#[derive(Clone, Debug, PartialEq)]
pub struct Signature(Vec<f32>);

impl Signature {
    /// `None` for an empty or completely flat picture.
    pub fn new(img: &DisplayImage) -> Option<Self> {
        if img.width < SIGNATURE_W || img.height < SIGNATURE_H {
            return None;
        }
        let mut cells = vec![0.0f32; SIGNATURE_W * SIGNATURE_H];
        let mut counts = vec![0u32; SIGNATURE_W * SIGNATURE_H];
        for y in 0..img.height {
            let cy = y * SIGNATURE_H / img.height;
            for x in 0..img.width {
                let cx = x * SIGNATURE_W / img.width;
                let p = &img.data[(y * img.width + x) * 3..][..3];
                cells[cy * SIGNATURE_W + cx] += 0.3 * p[0] as f32 + 0.59 * p[1] as f32 + 0.11 * p[2] as f32;
                counts[cy * SIGNATURE_W + cx] += 1;
            }
        }
        cells.iter_mut().zip(&counts).for_each(|(c, n)| *c /= *n as f32);
        let mean = cells.iter().sum::<f32>() / cells.len() as f32;
        let var = cells.iter().map(|c| (c - mean).powi(2)).sum::<f32>() / cells.len() as f32;
        if var < 1e-3 {
            return None;
        }
        let sd = var.sqrt();
        Some(Self(cells.iter().map(|c| (c - mean) / sd).collect()))
    }

    /// Correlation in `-1..=1`; about 1 for the same composition.
    pub fn similarity(&self, other: &Signature) -> f32 {
        self.0.iter().zip(&other.0).map(|(a, b)| a * b).sum::<f32>() / self.0.len() as f32
    }
}

/// Evidence about the gap between two consecutive shots.
#[derive(Clone, Debug, PartialEq)]
pub struct Gap {
    /// Probability that a new stack starts after this gap.
    pub boundary: f32,
    /// What speaks for (or against) a boundary, for display.
    pub reasons: Vec<String>,
}

impl Gap {
    pub fn is_boundary(&self) -> bool {
        self.boundary > 0.5
    }

    /// Neither clearly a boundary nor clearly inside a stack.
    pub fn is_uncertain(&self) -> bool {
        (UNCERTAIN_LOW..UNCERTAIN_HIGH).contains(&self.boundary)
    }
}

const UNCERTAIN_LOW: f32 = 0.2;
const UNCERTAIN_HIGH: f32 = 0.8;
/// Gaps on each side compared with a gap.
const LOCAL_WINDOW: usize = 5;
/// Shortest typical gap assumed (cameras record time to 1/100 s).
const MIN_TYPICAL_GAP: f64 = 0.02;

/// Judges the gaps between consecutive `shots`, which must be in shooting order.
pub fn analyse_gaps(shots: &[Shot]) -> Vec<Gap> {
    let seconds: Vec<Option<f64>> = shots
        .windows(2)
        .map(|w| match (w[0].capture_time, w[1].capture_time) {
            (Some(a), Some(b)) => Some(((b - a).num_milliseconds() as f64 / 1000.0).max(0.0)),
            _ => None,
        })
        .collect();
    shots
        .windows(2)
        .enumerate()
        .map(|(i, w)| {
            let (a, b) = (&w[0], &w[1]);
            let mut evidence = 0.0f32;
            let mut reasons = Vec::new();

            // Pause relative to the pauses around it. (A session-wide split into short and
            // long pauses misleads mixed sessions: next to bracketed stacks, every pause of
            // a manual stack looks long.)
            if let Some(g) = seconds[i] {
                let lo = i.saturating_sub(LOCAL_WINDOW);
                let mut around: Vec<f64> = (lo..(i + 1 + LOCAL_WINDOW).min(seconds.len()))
                    .filter(|&k| k != i)
                    .filter_map(|k| seconds[k])
                    .collect();
                if !around.is_empty() {
                    around.sort_by(f64::total_cmp);
                    let typical = around[around.len() / 2].max(MIN_TYPICAL_GAP);
                    let ratio = g / typical;
                    evidence += (1.5 * (ratio.log(3.0) - 1.0) as f32).clamp(-2.5, 3.0);
                    if ratio > 3.0 {
                        reasons.push(format!("Pause of {} (usually {})", duration(g), duration(typical)));
                    }
                }
            }

            // Changed settings, weighted by how surely they mean a new stack.
            const STRONG: f32 = 6.0;
            const WEAK: f32 = 1.0;
            let mut changed = |weight: f32, what: String| {
                evidence += weight;
                reasons.push(what);
            };
            if a.camera != b.camera {
                changed(STRONG, format!("Camera changed ({} → {})", a.camera, b.camera));
            }
            if a.size != b.size {
                changed(STRONG, "Image size changed".into());
            }
            let (ea, eb) = (&a.exposure, &b.exposure);
            if let (Some(x), Some(y)) = (ea.f_number, eb.f_number)
                && (x - y).abs() > 0.05
            {
                changed(STRONG, format!("Aperture changed (f/{x} → f/{y})"));
            }
            if let (Some(x), Some(y)) = (ea.focal_length, eb.focal_length)
                && (x - y).abs() > 0.5
            {
                changed(STRONG, format!("Focal length changed ({x} → {y} mm)"));
            }
            if a.orientation != b.orientation {
                changed(3.0, "Orientation changed".into());
            }
            // Exposure time and ISO may follow the light within a stack (auto modes).
            if let (Some(x), Some(y)) = (ea.time, eb.time)
                && (x / y - 1.0).abs() > 0.01
            {
                changed(WEAK, format!("Exposure time changed ({} → {})", exposure_time(x), exposure_time(y)));
            }
            if let (Some(x), Some(y)) = (ea.iso, eb.iso)
                && x != y
            {
                changed(WEAK, format!("ISO changed ({x} → {y})"));
            }

            // Changed picture.
            if let (Some(x), Some(y)) = (&a.signature, &b.signature) {
                let s = x.similarity(y);
                evidence += ((0.9 - s) * 10.0).clamp(-1.5, 3.0);
                if s < 0.8 {
                    reasons.push("Picture changed".into());
                }
            }

            Gap { boundary: 1.0 / (1.0 + (-evidence).exp()), reasons }
        })
        .collect()
}

/// Indices where stacks start (always including 0), from analysed gaps.
pub fn stack_starts(gaps: &[Gap]) -> Vec<usize> {
    std::iter::once(0).chain(gaps.iter().enumerate().filter(|(_, g)| g.is_boundary()).map(|(i, _)| i + 1)).collect()
}

fn duration(seconds: f64) -> String {
    match seconds {
        s if s < 10.0 => format!("{s:.2} s"),
        s if s < 120.0 => format!("{s:.0} s"),
        s if s < 7200.0 => format!("{:.0} min", s / 60.0),
        s => format!("{:.1} h", s / 3600.0),
    }
}

fn exposure_time(seconds: f32) -> String {
    if seconds >= 0.3 { format!("{seconds} s") } else { format!("1/{:.0} s", 1.0 / seconds) }
}

/// Name of the session file kept in a photo folder.
pub const SESSION_FILE_NAME: &str = "Shroom Stack session.json";

/// The grouping of a folder's photos, as stored in its session file. File names are
/// relative to the folder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionFile {
    pub version: u32,
    pub stacks: Vec<StackEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StackEntry {
    /// All photos of the stack in shooting order, including excluded ones.
    pub frames: Vec<String>,
    /// Photos left out when stacking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<String>,
    /// Stacking results made from this stack.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<String>,
    /// A single result, as written by earlier versions; read only.
    #[serde(default, skip_serializing)]
    pub result: Option<String>,
}

impl SessionFile {
    pub const VERSION: u32 = 1;

    /// Reads the session file of `folder`; `Ok(None)` if there is none.
    pub fn load(folder: &Path) -> crate::Result<Option<Self>> {
        let path = folder.join(SESSION_FILE_NAME);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(crate::Error::Io { path, source }),
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| crate::Error::InvalidInput(format!("{}: {e}", path.display())))
    }

    /// Writes the session file into `folder` (via a temporary file, so a crash never
    /// leaves a truncated one).
    pub fn save(&self, folder: &Path) -> crate::Result<()> {
        let path = folder.join(SESSION_FILE_NAME);
        let tmp = folder.join(format!("{SESSION_FILE_NAME}.tmp"));
        let text = serde_json::to_string_pretty(self).expect("serialisable");
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|source| crate::Error::Io { path, source })
    }
}

/// A stack of a session, with absolute paths.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stack {
    pub frames: Vec<PathBuf>,
    pub excluded: HashSet<PathBuf>,
    /// Stacking results made from this stack, by file name.
    pub results: Vec<PathBuf>,
    /// Probability that this stack really starts here (1 for the first stack and for
    /// stacks restored from a session file, where the grouping was already reviewed).
    pub boundary: f32,
    /// What speaks for the boundary before this stack, for display.
    pub reasons: Vec<String>,
}

impl Stack {
    /// Frames to stack, in order.
    pub fn included(&self) -> Vec<PathBuf> {
        self.frames.iter().filter(|p| !self.excluded.contains(*p)).cloned().collect()
    }

    pub fn is_uncertain(&self) -> bool {
        (UNCERTAIN_LOW..UNCERTAIN_HIGH).contains(&self.boundary)
    }
}

/// Starts a new stack at frame `frame` of stack `stack` (which keeps the frames before).
pub fn split(stacks: &mut Vec<Stack>, stack: usize, frame: usize) {
    let s = &mut stacks[stack];
    if frame == 0 || frame >= s.frames.len() {
        return;
    }
    let frames = s.frames.split_off(frame);
    let excluded = frames.iter().filter(|p| s.excluded.remove(*p)).cloned().collect();
    stacks.insert(stack + 1, Stack { frames, excluded, ..reviewed() });
}

/// Joins stack `stack` to the one before it.
pub fn merge_with_previous(stacks: &mut Vec<Stack>, stack: usize) {
    if stack == 0 || stack >= stacks.len() {
        return;
    }
    let s = stacks.remove(stack);
    let prev = &mut stacks[stack - 1];
    prev.frames.extend(s.frames);
    prev.excluded.extend(s.excluded);
    prev.results.extend(s.results);
    prev.results.sort();
}

/// Moves frames `0..=frame` of stack `stack` to the end of the previous stack.
pub fn move_to_previous(stacks: &mut Vec<Stack>, stack: usize, frame: usize) {
    if stack == 0 || stack >= stacks.len() {
        return;
    }
    split(stacks, stack, frame + 1);
    merge_with_previous(stacks, stack);
}

/// Moves frames `frame..` of stack `stack` to the start of the next stack.
pub fn move_to_next(stacks: &mut Vec<Stack>, stack: usize, frame: usize) {
    if stack + 1 >= stacks.len() {
        return;
    }
    if frame == 0 {
        merge_with_previous(stacks, stack + 1);
        return;
    }
    split(stacks, stack, frame);
    merge_with_previous(stacks, stack + 2);
}

/// A stack whose boundary the user has decided on.
fn reviewed() -> Stack {
    Stack { boundary: 1.0, ..Default::default() }
}

/// Groups `shots` (in shooting order) into stacks, keeping the grouping of a session file
/// for the photos it lists. Photos not in the file are grouped automatically; photos in
/// the file that no longer exist are dropped.
pub fn assemble(folder: &Path, shots: &[Shot], file: Option<&SessionFile>) -> Vec<Stack> {
    let index: HashMap<&Path, usize> = shots.iter().enumerate().map(|(i, s)| (s.path.as_path(), i)).collect();
    let mut stacks: Vec<(usize, Stack)> = Vec::new();
    let mut known = vec![false; shots.len()];
    for entry in file.map(|f| f.stacks.as_slice()).unwrap_or_default() {
        let mut frames: Vec<usize> =
            entry.frames.iter().filter_map(|n| index.get(folder.join(n).as_path()).copied()).collect();
        frames.sort();
        frames.dedup();
        frames.retain(|&i| !std::mem::replace(&mut known[i], true));
        let Some(&first) = frames.first() else { continue };
        let paths: Vec<PathBuf> = frames.iter().map(|&i| shots[i].path.clone()).collect();
        let excluded = entry.excluded.iter().map(|n| folder.join(n)).filter(|p| paths.contains(p)).collect();
        let results = entry.results.iter().chain(&entry.result).map(|n| folder.join(n)).collect();
        stacks.push((first, Stack { frames: paths, excluded, results, boundary: 1.0, reasons: Vec::new() }));
    }

    let new: Vec<usize> = (0..shots.len()).filter(|&i| !known[i]).collect();
    let new_shots: Vec<Shot> = new.iter().map(|&i| shots[i].clone()).collect();
    let gaps = analyse_gaps(&new_shots);
    let starts = stack_starts(&gaps);
    for (k, &start) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(new.len());
        if start >= end {
            continue;
        }
        let (boundary, reasons) = match start.checked_sub(1).map(|g| &gaps[g]) {
            Some(gap) => (gap.boundary, gap.reasons.clone()),
            None => (1.0, Vec::new()),
        };
        let frames = new[start..end].iter().map(|&i| shots[i].path.clone()).collect();
        stacks.push((new[start], Stack { frames, excluded: HashSet::new(), results: Vec::new(), boundary, reasons }));
    }
    stacks.sort_by_key(|(first, _)| *first);
    stacks.into_iter().map(|(_, s)| s).collect()
}

/// A stacking result (Linear DNG) found in a session folder.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultFile {
    pub path: PathBuf,
    /// File names of the frames it was stacked from, as recorded in the file.
    pub sources: Vec<String>,
}

/// Assigns the `results` of a folder to the stacks they were made from, replacing what the
/// stacks listed, and returns the results that fit no stack. A result goes to
/// 1. the stack with most of its recorded source frames,
/// 2. else the stack the session file listed it for (results of earlier versions),
/// 3. else the stack containing `<name>` for a result named `<name>-stacked[-n].dng`.
pub fn attach_results(stacks: &mut [Stack], results: &[ResultFile]) -> Vec<PathBuf> {
    let listed: HashMap<PathBuf, usize> = stacks
        .iter_mut()
        .enumerate()
        .flat_map(|(i, s)| std::mem::take(&mut s.results).into_iter().map(move |p| (p, i)))
        .collect();
    let stem = |p: &Path| p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let file_name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut unmatched = Vec::new();
    for result in results {
        let by_sources = stacks
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s.frames.iter().filter(|p| result.sources.contains(&file_name(p))).count()))
            .filter(|&(_, n)| n > 0)
            .max_by_key(|&(i, n)| (n, std::cmp::Reverse(i)))
            .map(|(i, _)| i);
        let by_name = || {
            let name = stem(&result.path);
            let base = stacked_base(&name)?;
            stacks.iter().position(|s| s.frames.iter().any(|p| stem(p) == base))
        };
        match by_sources.or_else(|| listed.get(&result.path).copied()).or_else(by_name) {
            Some(i) => stacks[i].results.push(result.path.clone()),
            None => unmatched.push(result.path.clone()),
        }
    }
    stacks.iter_mut().for_each(|s| s.results.sort());
    unmatched
}

/// `IMG_1` for result names `IMG_1-stacked` and `IMG_1-stacked-2`.
fn stacked_base(stem: &str) -> Option<&str> {
    let (base, rest) = stem.rsplit_once("-stacked")?;
    (rest.is_empty() || rest.strip_prefix('-').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
        .then_some(base)
}

/// The session file for `stacks` of `folder`.
pub fn to_file(folder: &Path, stacks: &[Stack]) -> SessionFile {
    let name = |p: &Path| p.strip_prefix(folder).unwrap_or(p).to_string_lossy().into_owned();
    SessionFile {
        version: SessionFile::VERSION,
        stacks: stacks
            .iter()
            .map(|s| StackEntry {
                frames: s.frames.iter().map(|p| name(p)).collect(),
                excluded: s.frames.iter().filter(|p| s.excluded.contains(*p)).map(|p| name(p)).collect(),
                results: s.results.iter().map(|p| name(p)).collect(),
                result: None,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(seconds: f64) -> NaiveDateTime {
        NaiveDateTime::parse_from_str("2026-10-17 12:00:00", "%Y-%m-%d %H:%M:%S").unwrap()
            + chrono::Duration::milliseconds((seconds * 1000.0) as i64)
    }

    fn shot(name: &str, seconds: f64) -> Shot {
        Shot {
            path: PathBuf::from(format!("/photos/{name}")),
            capture_time: Some(time(seconds)),
            camera: "Canon EOS R5".into(),
            size: (8192, 5464),
            orientation: Orientation::Normal,
            exposure: Exposure { time: Some(0.04), f_number: Some(6.3), iso: Some(1600), focal_length: Some(35.0) },
            signature: None,
        }
    }

    /// Shots at the given times, named by index.
    fn shots(times: &[f64]) -> Vec<Shot> {
        times.iter().enumerate().map(|(i, t)| shot(&format!("{i:03}.dng"), *t)).collect()
    }

    /// `count` shots starting at `start`, `step` seconds apart.
    fn burst(start: f64, count: usize, step: f64) -> Vec<f64> {
        (0..count).map(|i| start + i as f64 * step).collect()
    }

    fn starts(shots: &[Shot]) -> Vec<usize> {
        stack_starts(&analyse_gaps(shots))
    }

    #[test]
    fn bracketed_stacks_split_at_long_pauses() {
        let times: Vec<f64> = [burst(0.0, 30, 0.14), burst(60.0, 20, 0.15), burst(75.0, 40, 0.13)].concat();
        assert_eq!(starts(&shots(&times)), vec![0, 30, 50]);
    }

    #[test]
    fn manual_stack_with_irregular_pauses_stays_together() {
        // Refocusing by hand: 1–3 s between shots, one longer pause.
        let times = [0.0, 1.2, 3.0, 4.1, 6.8, 8.0, 9.5, 13.0, 14.2, 15.9, 17.0];
        let gaps = analyse_gaps(&shots(&times));
        assert_eq!(stack_starts(&gaps), vec![0]);
    }

    #[test]
    fn setup_time_is_judged_relative_to_the_stack() {
        // Manual stacks with ~2 s between shots; 12 s to set up the next one. In seconds
        // this is shorter than many pauses between bracketed stacks elsewhere.
        let times: Vec<f64> = [burst(0.0, 10, 2.0), burst(30.0, 10, 2.1), burst(500.0, 30, 0.1)].concat();
        let gaps = analyse_gaps(&shots(&times));
        assert_eq!(stack_starts(&gaps), vec![0, 10, 20]);
        // Inside the stacks nothing is in doubt (12 s to set up after 2 s pauses may be).
        let uncertain: Vec<usize> = (0..gaps.len()).filter(|&i| i != 9 && gaps[i].is_uncertain()).collect();
        assert!(
            uncertain.is_empty(),
            "uncertain gaps {uncertain:?}: {:?}",
            uncertain.iter().map(|&i| gaps[i].boundary).collect::<Vec<_>>()
        );
    }

    #[test]
    fn changed_aperture_starts_a_stack_even_without_pause() {
        let mut s = shots(&burst(0.0, 20, 0.15));
        for shot in &mut s[10..] {
            shot.exposure.f_number = Some(8.0);
        }
        let gaps = analyse_gaps(&s);
        assert_eq!(stack_starts(&gaps), vec![0, 10]);
        assert!(gaps[9].reasons.iter().any(|r| r.contains("Aperture")), "{:?}", gaps[9].reasons);
    }

    #[test]
    fn changed_picture_adds_evidence() {
        let img = |seed: u8| DisplayImage {
            width: 64,
            height: 48,
            data: (0..64 * 48 * 3).map(|i| ((i as u32).wrapping_mul(2654435761) >> (seed % 8)) as u8).collect(),
        };
        let (a, b) = (Signature::new(&img(1)).unwrap(), Signature::new(&img(5)).unwrap());
        assert!((a.similarity(&a) - 1.0).abs() < 1e-4);
        assert!(a.similarity(&b) < 0.5);
        // A borderline pause (4× the usual one) with and without a new picture.
        let times: Vec<f64> = [burst(0.0, 10, 1.0), burst(13.0, 10, 1.0)].concat();
        let mut s = shots(&times);
        let same = analyse_gaps(&s)[9].boundary;
        for (i, shot) in s.iter_mut().enumerate() {
            shot.signature = Some(if i < 10 { a.clone() } else { b.clone() });
        }
        let changed = analyse_gaps(&s)[9].boundary;
        assert!(changed > 0.8 && changed > same, "{same} → {changed}");
    }

    #[test]
    fn borderline_pauses_are_uncertain() {
        let times: Vec<f64> = [burst(0.0, 10, 1.0), burst(12.5, 10, 1.0)].concat();
        let gaps = analyse_gaps(&shots(&times));
        assert!(gaps[9].is_uncertain(), "{}", gaps[9].boundary);
        assert!(gaps[3].boundary < UNCERTAIN_LOW);
    }

    #[test]
    fn signature_ignores_flat_and_tiny_images() {
        let flat = DisplayImage { width: 64, height: 48, data: vec![128; 64 * 48 * 3] };
        assert!(Signature::new(&flat).is_none());
        let tiny = DisplayImage { width: 8, height: 8, data: vec![0; 8 * 8 * 3] };
        assert!(Signature::new(&tiny).is_none());
    }

    #[test]
    fn session_file_restores_grouping_and_adds_new_photos() {
        let folder = Path::new("/photos");
        let times: Vec<f64> = [burst(0.0, 6, 0.1), burst(100.0, 6, 0.1), burst(300.0, 4, 0.1)].concat();
        let s = shots(&times);
        // The user split the first burst in two and excluded a frame; the third burst is
        // new since the file was written, and one listed file no longer exists.
        let file = SessionFile {
            version: 1,
            stacks: vec![
                StackEntry { frames: vec!["000.dng".into(), "001.dng".into(), "002.dng".into()], ..Default::default() },
                StackEntry {
                    frames: vec!["003.dng".into(), "004.dng".into(), "005.dng".into(), "gone.dng".into()],
                    excluded: vec!["004.dng".into()],
                    results: vec!["003-stacked.dng".into()],
                    ..Default::default()
                },
                StackEntry { frames: (6..12).map(|i| format!("{i:03}.dng")).collect(), ..Default::default() },
            ],
        };
        let stacks = assemble(folder, &s, Some(&file));
        let sizes: Vec<usize> = stacks.iter().map(|s| s.frames.len()).collect();
        assert_eq!(sizes, vec![3, 3, 6, 4]);
        assert_eq!(stacks[1].included().len(), 2);
        assert_eq!(stacks[1].results, vec![PathBuf::from("/photos/003-stacked.dng")]);
        assert_eq!(stacks[3].boundary, 1.0);

        let mut back = to_file(folder, &stacks);
        back.stacks[1].frames.push("gone.dng".into());
        assert_eq!(back.stacks[..3], file.stacks[..]);
    }

    #[test]
    fn legacy_single_result_is_read() {
        let file: SessionFile = serde_json::from_str(
            r#"{"version":1,"stacks":[{"frames":["000.dng","001.dng"],"result":"000-stacked.dng"}]}"#,
        )
        .unwrap();
        let stacks = assemble(Path::new("/photos"), &shots(&[0.0, 0.1]), Some(&file));
        assert_eq!(stacks[0].results, vec![PathBuf::from("/photos/000-stacked.dng")]);
        let back = serde_json::to_string(&to_file(Path::new("/photos"), &stacks)).unwrap();
        assert!(back.contains(r#""results":["000-stacked.dng"]"#) && !back.contains(r#""result""#), "{back}");
    }

    #[test]
    fn results_attach_by_sources_then_listing_then_name() {
        let p = |n: &str| PathBuf::from(format!("/photos/{n}"));
        let frames = |r: std::ops::Range<usize>| r.map(|i| p(&format!("{i:03}.dng"))).collect::<Vec<_>>();
        let mut stacks: Vec<Stack> = [frames(0..3), frames(3..6), frames(6..8)]
            .into_iter()
            .map(|frames| Stack { frames, ..reviewed() })
            .collect();
        let result = |n: &str, sources: &[&str]| ResultFile {
            path: p(n),
            sources: sources.iter().map(|s| s.to_string()).collect(),
        };
        // Listed for stack 0 by the session file, but its sources say stack 1.
        stacks[0].results = vec![p("moved.dng"), p("listed.dng")];
        let unmatched = attach_results(
            &mut stacks,
            &[
                result("moved.dng", &["002.dng", "003.dng", "004.dng"]),
                result("listed.dng", &[]),
                result("006-stacked-2.dng", &[]),
                result("000-stacked.dng", &[]),
                result("other.dng", &[]),
                result("foreign-stacked.dng", &["x.dng"]),
            ],
        );
        assert_eq!(stacks[0].results, vec![p("000-stacked.dng"), p("listed.dng")]);
        assert_eq!(stacks[1].results, vec![p("moved.dng")]);
        assert_eq!(stacks[2].results, vec![p("006-stacked-2.dng")]);
        assert_eq!(unmatched, vec![p("other.dng"), p("foreign-stacked.dng")]);
    }

    #[test]
    fn stacked_base_names() {
        assert_eq!(stacked_base("IMG_1-stacked"), Some("IMG_1"));
        assert_eq!(stacked_base("IMG_1-stacked-12"), Some("IMG_1"));
        assert_eq!(stacked_base("IMG_1-stacked-"), None);
        assert_eq!(stacked_base("IMG_1-stacked-x"), None);
        assert_eq!(stacked_base("IMG_1"), None);
    }

    fn layout(stacks: &[Stack]) -> Vec<Vec<String>> {
        stacks
            .iter()
            .map(|s| s.frames.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect())
            .collect()
    }

    fn stacks_of(sizes: &[usize]) -> Vec<Stack> {
        let mut n = 0;
        sizes
            .iter()
            .map(|&k| {
                let frames = (n..n + k).map(|i| PathBuf::from(format!("/p/{i}"))).collect();
                n += k;
                Stack { frames, boundary: 0.5, ..Default::default() }
            })
            .collect()
    }

    #[test]
    fn editing_stacks() {
        let mut s = stacks_of(&[4, 3]);
        s[0].excluded.insert(PathBuf::from("/p/3"));
        split(&mut s, 0, 2);
        assert_eq!(layout(&s), [vec!["0", "1"], vec!["2", "3"], vec!["4", "5", "6"]]);
        assert!(s[1].excluded.contains(Path::new("/p/3")) && s[0].excluded.is_empty());
        assert!(!s[1].is_uncertain());
        move_to_previous(&mut s, 2, 0);
        assert_eq!(layout(&s), [vec!["0", "1"], vec!["2", "3", "4"], vec!["5", "6"]]);
        move_to_next(&mut s, 1, 1);
        assert_eq!(layout(&s), [vec!["0", "1"], vec!["2"], vec!["3", "4", "5", "6"]]);
        assert!(s[2].excluded.contains(Path::new("/p/3")));
        move_to_next(&mut s, 1, 0);
        assert_eq!(layout(&s), [vec!["0", "1"], vec!["2", "3", "4", "5", "6"]]);
        merge_with_previous(&mut s, 1);
        assert_eq!(layout(&s), [vec!["0", "1", "2", "3", "4", "5", "6"]]);
        // Out-of-range edits do nothing.
        split(&mut s, 0, 0);
        merge_with_previous(&mut s, 0);
        move_to_previous(&mut s, 0, 1);
        move_to_next(&mut s, 0, 1);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn session_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(SessionFile::load(dir.path()).unwrap(), None);
        let file = SessionFile {
            version: SessionFile::VERSION,
            stacks: vec![StackEntry { frames: vec!["a.dng".into(), "b.dng".into()], ..Default::default() }],
        };
        file.save(dir.path()).unwrap();
        assert_eq!(SessionFile::load(dir.path()).unwrap(), Some(file));
        assert!(!dir.path().join(format!("{SESSION_FILE_NAME}.tmp")).exists());
    }
}
