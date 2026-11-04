//! Stacking several stacks unattended, one after another, saving each result next to its
//! originals.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use eframe::egui;
use shroom_core::io::{self, ResultFormat};
use shroom_core::stack::{CancelToken, FileFrames, Progress, StackSettings};

pub enum BatchEvent {
    /// Stack `index` of the batch makes progress.
    Progress(usize, Progress),
    /// Stack `index` is done: the saved file, or why it failed.
    Finished(usize, Result<PathBuf, String>),
    /// All stacks are done (or the batch was cancelled).
    Done,
}

pub struct Batch {
    rx: Receiver<BatchEvent>,
    cancel: CancelToken,
}

impl Batch {
    /// Stacks each list of frames in turn, saving results in `format`.
    pub fn start(ctx: egui::Context, stacks: Vec<Vec<PathBuf>>, settings: StackSettings, format: ResultFormat) -> Self {
        let (tx, rx) = channel();
        let cancel = CancelToken::default();
        let token = cancel.clone();
        std::thread::spawn(move || {
            let send = |event| {
                let _ = tx.send(event);
                ctx.request_repaint();
            };
            for (index, paths) in stacks.into_iter().enumerate() {
                if token.is_cancelled() {
                    break;
                }
                let progress = |p| send(BatchEvent::Progress(index, p));
                let source = FileFrames { paths: paths.clone() };
                let result = shroom_core::stack::stack(&source, &settings, &progress, &token).and_then(|result| {
                    let template = io::read_template(&paths[0])?.with_sources(&paths).with_settings(&settings);
                    let path = result_path(&paths[0], format);
                    let software = concat!("Shroom Stack ", env!("CARGO_PKG_VERSION"));
                    format.write(&path, &result.image, &template, software)?;
                    Ok(path)
                });
                match result {
                    Err(shroom_core::Error::Cancelled) => break,
                    result => send(BatchEvent::Finished(index, result.map_err(|e| e.to_string()))),
                }
            }
            send(BatchEvent::Done);
        });
        Self { rx, cancel }
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_cancelling(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn poll(&self) -> Vec<BatchEvent> {
        self.rx.try_iter().collect()
    }
}

/// `<first frame>-stacked.dng` (or `.jpg`) next to the first frame, numbered if that file
/// exists.
pub fn result_path(first: &Path, format: ResultFormat) -> PathBuf {
    let stem = first.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = first.parent().unwrap_or(Path::new(""));
    let ext = format.extension();
    (1..)
        .map(|n| dir.join(if n == 1 { format!("{stem}-stacked.{ext}") } else { format!("{stem}-stacked-{n}.{ext}") }))
        .find(|p| !p.exists())
        .expect("a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_names_never_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("1Z4A4075.CR3");
        assert_eq!(result_path(&first, ResultFormat::Dng), dir.path().join("1Z4A4075-stacked.dng"));
        std::fs::write(dir.path().join("1Z4A4075-stacked.dng"), b"").unwrap();
        assert_eq!(result_path(&first, ResultFormat::Dng), dir.path().join("1Z4A4075-stacked-2.dng"));
        let jpeg = ResultFormat::Jpeg { quality: 90 };
        assert_eq!(result_path(&first, jpeg), dir.path().join("1Z4A4075-stacked.jpg"));
    }
}
