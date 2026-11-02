//! Converting raw files the app can't decode (e.g. Nikon HE* NEFs) with Adobe DNG
//! Converter, which is free but only available for macOS and Windows.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};

use eframe::egui;

/// Files per converter run: each run reports progress and can be cancelled after.
const CHUNK: usize = 8;

/// The installed converter: `SHROOM_DNG_CONVERTER`, else the standard install location.
pub fn find() -> Option<PathBuf> {
    static FOUND: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    FOUND
        .get_or_init(|| {
            let mut candidates: Vec<PathBuf> =
                std::env::var_os("SHROOM_DNG_CONVERTER").map(PathBuf::from).into_iter().collect();
            if cfg!(target_os = "macos") {
                candidates.push("/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter".into());
            }
            if cfg!(windows) {
                for var in ["ProgramFiles", "ProgramFiles(x86)"] {
                    if let Some(dir) = std::env::var_os(var) {
                        candidates.push(Path::new(&dir).join("Adobe/Adobe DNG Converter/Adobe DNG Converter.exe"));
                    }
                }
            }
            candidates.into_iter().find(|p| p.is_file())
        })
        .clone()
}

/// Whether Adobe DNG Converter exists for this platform at all.
pub const AVAILABLE_ON_PLATFORM: bool = cfg!(any(target_os = "macos", windows));

/// The DNG Adobe DNG Converter writes for `file`: next to it, with extension `.dng`.
pub fn output_path(file: &Path) -> PathBuf {
    file.with_extension("dng")
}

pub enum Event {
    /// Files converted so far, of all.
    Progress(usize, usize),
    /// The DNGs, in the order of the input files.
    Finished(Result<Vec<PathBuf>, String>),
}

/// A conversion running in the background.
pub struct Conversion {
    rx: Receiver<Event>,
    cancel: Arc<AtomicBool>,
}

impl Conversion {
    /// Converts `files` to losslessly compressed DNGs next to them; DNGs that already
    /// exist are kept, not converted again.
    pub fn start(ctx: egui::Context, converter: PathBuf, files: Vec<PathBuf>) -> Self {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        std::thread::spawn(move || {
            let send = |event| {
                let _ = tx.send(event);
                ctx.request_repaint();
            };
            let todo: Vec<&PathBuf> = files.iter().filter(|f| !output_path(f).exists()).collect();
            let mut done = files.len() - todo.len();
            send(Event::Progress(done, files.len()));
            for chunk in todo.chunks(CHUNK) {
                if cancelled.load(Ordering::Relaxed) {
                    return send(Event::Finished(Err("Conversion cancelled.".into())));
                }
                if let Err(e) = convert(&converter, chunk) {
                    return send(Event::Finished(Err(e)));
                }
                done += chunk.len();
                send(Event::Progress(done, files.len()));
            }
            send(Event::Finished(Ok(files.iter().map(|f| output_path(f)).collect())));
        });
        Self { rx, cancel }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelling(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn poll(&self) -> Vec<Event> {
        self.rx.try_iter().collect()
    }
}

/// One converter run for files of one folder.
fn convert(converter: &Path, files: &[&PathBuf]) -> Result<(), String> {
    let dir = files[0].parent().unwrap_or(Path::new("."));
    // -c: lossless compressed raw data (not linear), -d: output folder.
    let mut command = Command::new(converter);
    command.arg("-c").arg("-d").arg(dir).args(files);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command.output().map_err(|e| format!("Could not run Adobe DNG Converter: {e}"))?;
    if !output.status.success() {
        return Err(format!("Adobe DNG Converter failed ({})", output.status));
    }
    match files.iter().find(|f| !output_path(f).exists()) {
        Some(f) => Err(format!("Adobe DNG Converter wrote no DNG for {}", f.display())),
        None => Ok(()),
    }
}
