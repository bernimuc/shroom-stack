//! Background loading of file information and full-resolution previews, so the UI never
//! blocks on decoding.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use eframe::egui;
use shroom_core::align::{Affine, warp_rgb};
use shroom_core::io::{self, FrameInfo};
use shroom_core::render::render_display;
use shroom_core::{DisplayImage, Rect, RgbImage};

/// A full-resolution rendering of an input frame.
#[derive(Clone, Debug, PartialEq)]
pub struct FullRequest {
    pub path: PathBuf,
    /// Warp into the stack's output geometry (transform, crop), for comparing with a result.
    pub aligned: Option<(Affine, Rect)>,
}

/// A rendered frame; for aligned requests also the linear image (the brush source).
pub struct FullImage {
    pub display: DisplayImage,
    pub linear: Option<Arc<RgbImage>>,
}

pub enum Loaded {
    Info(PathBuf, Result<FrameInfo, String>),
    Full(FullRequest, Result<FullImage, String>),
}

pub struct Loader {
    tx: Sender<Loaded>,
    rx: Receiver<Loaded>,
    ctx: egui::Context,
    /// Most recent full-preview request; older ones are skipped.
    wanted_full: Arc<Mutex<Option<FullRequest>>>,
    full_worker_running: Arc<Mutex<bool>>,
}

impl Loader {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, ctx, wanted_full: Arc::new(Mutex::new(None)), full_worker_running: Arc::new(Mutex::new(false)) }
    }

    pub fn request_info(&self, path: PathBuf) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let result = io::read_frame_info(&path).map_err(|e| e.to_string());
            let _ = tx.send(Loaded::Info(path, result));
            ctx.request_repaint();
        });
    }

    /// Requests a full-resolution rendering; only the latest request is served.
    pub fn request_full(&self, request: FullRequest) {
        *self.wanted_full.lock().unwrap() = Some(request);
        let mut running = self.full_worker_running.lock().unwrap();
        if *running {
            return;
        }
        *running = true;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        let (wanted, running) = (self.wanted_full.clone(), self.full_worker_running.clone());
        std::thread::spawn(move || {
            loop {
                let next = {
                    let mut running = running.lock().unwrap();
                    let next = wanted.lock().unwrap().take();
                    if next.is_none() {
                        *running = false;
                    }
                    next
                };
                let Some(request) = next else { break };
                let result = io::load_linear(&request.path)
                    .map(|frame| match &request.aligned {
                        Some((t, crop)) => {
                            let image = warp_rgb(&frame.image, t, *crop);
                            FullImage {
                                display: render_display(&image, &frame.color, None),
                                linear: Some(Arc::new(image)),
                            }
                        }
                        None => FullImage { display: render_display(&frame.image, &frame.color, None), linear: None },
                    })
                    .map_err(|e| e.to_string());
                let _ = tx.send(Loaded::Full(request, result));
                ctx.request_repaint();
            }
        });
    }

    pub fn poll(&self) -> Vec<Loaded> {
        self.rx.try_iter().collect()
    }
}
