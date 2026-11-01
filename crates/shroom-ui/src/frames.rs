//! The list of input frames and the checks run before stacking. Pure model code without
//! UI, so it can be unit-tested.

use std::path::{Path, PathBuf};

use shroom_core::io::FrameInfo;

/// One input file; `info` is filled in once its metadata has been read.
#[derive(Clone, Debug)]
pub struct Frame {
    pub path: PathBuf,
    pub info: Option<FrameInfo>,
    /// Error while reading the file, if any.
    pub error: Option<String>,
}

impl Frame {
    pub fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

fn is_dng(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dng"))
}

/// Expands folders (one level) and keeps DNG files; returns them and the number of files
/// skipped because they are not DNGs.
pub fn collect_dngs(paths: impl IntoIterator<Item = PathBuf>) -> (Vec<PathBuf>, usize) {
    let (mut dngs, mut skipped) = (Vec::new(), 0);
    for path in paths {
        if path.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&path)
                .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_file()).collect())
                .unwrap_or_default();
            entries.sort();
            for entry in entries {
                if is_dng(&entry) {
                    dngs.push(entry);
                } else if !entry.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
                    skipped += 1;
                }
            }
        } else if is_dng(&path) {
            dngs.push(path);
        } else {
            skipped += 1;
        }
    }
    (dngs, skipped)
}

#[derive(Clone, Debug, Default)]
pub struct FrameList {
    pub frames: Vec<Frame>,
}

impl FrameList {
    /// Adds files not yet in the list; returns the paths actually added.
    pub fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
        let mut added = Vec::new();
        for path in paths {
            if !self.frames.iter().any(|f| f.path == path) {
                self.frames.push(Frame { path: path.clone(), info: None, error: None });
                added.push(path);
            }
        }
        added
    }

    pub fn set_info(&mut self, path: &Path, result: Result<FrameInfo, String>) {
        if let Some(frame) = self.frames.iter_mut().find(|f| f.path == path) {
            match result {
                Ok(info) => frame.info = Some(info),
                Err(e) => frame.error = Some(e),
            }
        }
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.frames.len() {
            self.frames.remove(index);
        }
    }

    /// Moves a frame by `delta` positions (clamped to the list); returns its new index.
    pub fn shift(&mut self, index: usize, delta: isize) -> usize {
        let target = (index as isize + delta).clamp(0, self.frames.len() as isize - 1) as usize;
        let frame = self.frames.remove(index);
        self.frames.insert(target, frame);
        target
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }

    pub fn reverse(&mut self) {
        self.frames.reverse();
    }

    /// Sorts into shooting order (capture time, then file name), like the core does.
    pub fn sort_by_capture_time(&mut self) {
        self.frames.sort_by(|a, b| {
            let time = |f: &Frame| f.info.as_ref().and_then(|i| i.capture_time);
            time(a).cmp(&time(b)).then_with(|| a.file_name().cmp(&b.file_name()))
        });
    }

    pub fn is_loading(&self) -> bool {
        self.frames.iter().any(|f| f.info.is_none() && f.error.is_none())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Severity {
    /// Stacking is not possible.
    Error,
    /// Stacking works, but the result may be poor.
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub severity: Severity,
    pub message: String,
}

impl Issue {
    fn error(message: impl Into<String>) -> Self {
        Self { severity: Severity::Error, message: message.into() }
    }
    fn warning(message: impl Into<String>) -> Self {
        Self { severity: Severity::Warning, message: message.into() }
    }
}

/// Approximate peak memory of stacking and showing the result, per image pixel. Measured
/// on the examples (≈6.4 GB for 47 MP on the GPU path) plus the result view and brush
/// source in the UI. Independent of the number of frames, since stacking streams them.
const BYTES_PER_PIXEL: u64 = 180;

/// Memory needed to stack frames of `width × height` pixels.
pub fn estimated_memory(width: usize, height: usize) -> u64 {
    BYTES_PER_PIXEL * (width * height) as u64
}

/// Installed memory in bytes, if it can be determined.
pub fn total_memory() -> Option<u64> {
    let sys = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_memory(sysinfo::MemoryRefreshKind::everything()),
    );
    sys.ok().map(|s| s.total_memory()).filter(|m| *m > 0)
}

/// Checks whether the frames can be stacked. `memory` is the installed memory in bytes.
pub fn preflight(list: &FrameList, memory: Option<u64>) -> Vec<Issue> {
    let mut issues = Vec::new();
    if list.frames.len() < 2 {
        issues.push(Issue::error("At least two images are needed for stacking."));
    }
    for f in list.frames.iter().filter(|f| f.error.is_some()) {
        issues.push(Issue::error(format!("{} cannot be read: {}", f.file_name(), f.error.as_deref().unwrap_or(""))));
    }
    if list.is_loading() {
        issues.push(Issue::error("Some images are still being loaded."));
    }
    let infos: Vec<(&Frame, &FrameInfo)> = list.frames.iter().filter_map(|f| f.info.as_ref().map(|i| (f, i))).collect();
    if let Some((_, first)) = infos.first() {
        for (f, info) in &infos[1..] {
            if (info.width, info.height) != (first.width, first.height) {
                issues.push(Issue::error(format!(
                    "{} is {}×{}, but the first image is {}×{}.",
                    f.file_name(),
                    info.width,
                    info.height,
                    first.width,
                    first.height
                )));
            }
        }
        if infos.iter().any(|(_, i)| i.camera != first.camera) {
            issues.push(Issue::warning("The images come from different cameras."));
        }
        if infos.iter().any(|(_, i)| i.orientation != first.orientation) {
            issues.push(Issue::warning(
                "The images have different orientations. The stack uses the first image's orientation; \
                 use Rotate left/right to change it.",
            ));
        }
        let needed = estimated_memory(first.width, first.height);
        if let Some(total) = memory
            && needed > total / 4 * 3
        {
            let gb = |b: u64| b as f64 / (1u64 << 30) as f64;
            issues.push(Issue::warning(format!(
                "Stacking images of this size needs about {:.0} GB of memory; this computer has {:.0} GB. \
                 It may become very slow.",
                gb(needed),
                gb(total)
            )));
        }
    }
    let times: Vec<_> = infos.iter().map(|(_, i)| i.capture_time).collect();
    if times.iter().all(Option::is_some)
        && times.windows(2).any(|w| w[0] > w[1])
        && !times.windows(2).all(|w| w[0] >= w[1])
    {
        issues.push(Issue::warning(
            "The images are not in shooting order. Stacking expects them sorted by focus distance.",
        ));
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;

    fn info(name: &str, time: &str, w: usize, camera: &str) -> FrameInfo {
        FrameInfo {
            path: PathBuf::from(name),
            file_name: name.into(),
            file_modified: None,
            capture_time: Some(NaiveDateTime::parse_from_str(time, "%Y-%m-%d %H:%M:%S").unwrap()),
            camera: camera.into(),
            width: w,
            height: 100,
            preview: None,
            color: Default::default(),
            orientation: Default::default(),
        }
    }

    fn list(entries: &[(&str, &str, usize, &str)]) -> FrameList {
        let mut l = FrameList::default();
        l.add(entries.iter().map(|e| PathBuf::from(e.0)));
        for (name, time, w, cam) in entries {
            l.set_info(Path::new(name), Ok(info(name, time, *w, cam)));
        }
        l
    }

    fn names(l: &FrameList) -> Vec<String> {
        l.frames.iter().map(Frame::file_name).collect()
    }

    #[test]
    fn add_ignores_duplicates() {
        let mut l = FrameList::default();
        assert_eq!(l.add(["a.dng".into(), "b.dng".into()]).len(), 2);
        assert_eq!(l.add(["b.dng".into(), "c.dng".into()]), vec![PathBuf::from("c.dng")]);
        assert_eq!(names(&l), ["a.dng", "b.dng", "c.dng"]);
        assert!(l.is_loading());
    }

    #[test]
    fn collects_dngs_from_files_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["b.DNG", "a.dng", "notes.txt", ".DS_Store"] {
            std::fs::write(dir.path().join(name), b"").unwrap();
        }
        let (dngs, skipped) = collect_dngs([dir.path().to_path_buf(), PathBuf::from("x.dng"), PathBuf::from("y.jpg")]);
        let names: Vec<_> = dngs.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["a.dng", "b.DNG", "x.dng"]);
        assert_eq!(skipped, 2, "notes.txt and y.jpg; hidden files are ignored silently");
    }

    #[test]
    fn reorder_operations() {
        let mut l = list(&[
            ("a", "2026-01-01 10:00:03", 10, "C"),
            ("b", "2026-01-01 10:00:01", 10, "C"),
            ("c", "2026-01-01 10:00:02", 10, "C"),
        ]);
        assert_eq!(l.shift(0, 1), 1);
        assert_eq!(names(&l), ["b", "a", "c"]);
        assert_eq!(l.shift(2, -5), 0);
        assert_eq!(names(&l), ["c", "b", "a"]);
        l.reverse();
        assert_eq!(names(&l), ["a", "b", "c"]);
        l.sort_by_capture_time();
        assert_eq!(names(&l), ["b", "c", "a"]);
        l.remove(0);
        assert_eq!(names(&l), ["c", "a"]);
        l.clear();
        assert!(l.frames.is_empty());
    }

    #[test]
    fn preflight_accepts_good_stack() {
        let l = list(&[("a", "2026-01-01 10:00:01", 10, "C"), ("b", "2026-01-01 10:00:02", 10, "C")]);
        assert!(preflight(&l, None).is_empty());
        // Reverse shooting order is fine too (focus from far to near).
        let l = list(&[("a", "2026-01-01 10:00:02", 10, "C"), ("b", "2026-01-01 10:00:01", 10, "C")]);
        assert!(preflight(&l, None).is_empty());
    }

    #[test]
    fn preflight_warns_about_memory() {
        let mut l = list(&[("a", "2026-01-01 10:00:01", 10, "C"), ("b", "2026-01-01 10:00:02", 10, "C")]);
        assert!(preflight(&l, Some(1 << 30)).is_empty());
        for f in &mut l.frames {
            let info = f.info.as_mut().unwrap();
            (info.width, info.height) = (8352, 5586);
        }
        let issues = preflight(&l, Some(8 << 30));
        assert!(issues.iter().any(|i| i.severity == Severity::Warning && i.message.contains("8 GB")), "{issues:?}");
        assert!(preflight(&l, Some(24 << 30)).is_empty());
        assert!(total_memory().is_some_and(|m| m > 1 << 30));
    }

    #[test]
    fn preflight_reports_problems() {
        let has = |issues: &[Issue], sev: Severity, text: &str| {
            issues.iter().any(|i| i.severity == sev && i.message.contains(text))
        };
        let one = list(&[("a", "2026-01-01 10:00:01", 10, "C")]);
        assert!(has(&preflight(&one, None), Severity::Error, "At least two"));

        let mixed = list(&[
            ("a", "2026-01-01 10:00:01", 10, "C"),
            ("b", "2026-01-01 10:00:03", 12, "D"),
            ("c", "2026-01-01 10:00:02", 10, "C"),
        ]);
        let issues = preflight(&mixed, None);
        assert!(!has(&issues, Severity::Warning, "orientations"));
        assert!(has(&issues, Severity::Error, "b is 12×100"));
        assert!(has(&issues, Severity::Warning, "different cameras"));
        assert!(has(&issues, Severity::Warning, "shooting order"));

        let mut rotated = list(&[("a", "2026-01-01 10:00:01", 10, "C"), ("b", "2026-01-01 10:00:02", 10, "C")]);
        rotated.frames[1].info.as_mut().unwrap().orientation = shroom_core::Orientation::Cw90;
        assert!(has(&preflight(&rotated, None), Severity::Warning, "different orientations"));

        let mut loading = list(&[("a", "2026-01-01 10:00:01", 10, "C")]);
        loading.add(["x".into()]);
        assert!(has(&preflight(&loading, None), Severity::Error, "still being loaded"));
        loading.set_info(Path::new("x"), Err("broken".into()));
        assert!(has(&preflight(&loading, None), Severity::Error, "x cannot be read: broken"));
    }
}
