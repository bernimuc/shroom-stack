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

/// Checks whether the frames can be stacked.
pub fn preflight(list: &FrameList) -> Vec<Issue> {
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
    }

    #[test]
    fn preflight_accepts_good_stack() {
        let l = list(&[("a", "2026-01-01 10:00:01", 10, "C"), ("b", "2026-01-01 10:00:02", 10, "C")]);
        assert!(preflight(&l).is_empty());
        // Reverse shooting order is fine too (focus from far to near).
        let l = list(&[("a", "2026-01-01 10:00:02", 10, "C"), ("b", "2026-01-01 10:00:01", 10, "C")]);
        assert!(preflight(&l).is_empty());
    }

    #[test]
    fn preflight_reports_problems() {
        let has = |issues: &[Issue], sev: Severity, text: &str| {
            issues.iter().any(|i| i.severity == sev && i.message.contains(text))
        };
        let one = list(&[("a", "2026-01-01 10:00:01", 10, "C")]);
        assert!(has(&preflight(&one), Severity::Error, "At least two"));

        let mixed = list(&[
            ("a", "2026-01-01 10:00:01", 10, "C"),
            ("b", "2026-01-01 10:00:03", 12, "D"),
            ("c", "2026-01-01 10:00:02", 10, "C"),
        ]);
        let issues = preflight(&mixed);
        assert!(has(&issues, Severity::Error, "b is 12×100"));
        assert!(has(&issues, Severity::Warning, "different cameras"));
        assert!(has(&issues, Severity::Warning, "shooting order"));

        let mut loading = list(&[("a", "2026-01-01 10:00:01", 10, "C")]);
        loading.add(["x".into()]);
        assert!(has(&preflight(&loading), Severity::Error, "still being loaded"));
        loading.set_info(Path::new("x"), Err("broken".into()));
        assert!(has(&preflight(&loading), Severity::Error, "x cannot be read: broken"));
    }
}
