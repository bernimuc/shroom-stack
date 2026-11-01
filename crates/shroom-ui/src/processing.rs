//! Running a stacking job on a background thread.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use eframe::egui;
use shroom_core::DisplayImage;
use shroom_core::io::{self, DngTemplate};
use shroom_core::render::{ColorInfo, render_display};
use shroom_core::stack::{CancelToken, FileFrames, Progress, Snapshot, StackResult, StackSettings, Stage};

pub enum JobEvent {
    Progress(Progress),
    Snapshot(Snapshot),
    Done(Result<Box<Finished>, JobError>),
}

pub enum JobError {
    Cancelled,
    Failed(String),
}

/// A completed stack with everything needed to show and save it.
pub struct Finished {
    pub result: StackResult,
    pub template: DngTemplate,
    /// Full-resolution display rendering of the result.
    pub display: DisplayImage,
    pub settings: StackSettings,
    pub paths: Vec<PathBuf>,
    pub elapsed: Duration,
}

pub struct Job {
    rx: Receiver<JobEvent>,
    cancel: CancelToken,
}

impl Job {
    pub fn start(ctx: egui::Context, paths: Vec<PathBuf>, settings: StackSettings, color: ColorInfo) -> Self {
        let (tx, rx) = channel();
        let cancel = CancelToken::default();
        let token = cancel.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let send = |event| {
                let _ = tx.send(event);
                ctx.request_repaint();
            };
            let progress = |p| send(JobEvent::Progress(p));
            let snapshot = |s| send(JobEvent::Snapshot(s));
            let source = FileFrames { paths: paths.clone() };
            let outcome =
                shroom_core::stack::stack_with_snapshots(&source, &settings, &progress, Some(&snapshot), &token)
                    .and_then(|result| {
                        let template = io::read_template(&paths[0])?;
                        let display = render_display(&result.image, &color, None);
                        Ok(Box::new(Finished {
                            result,
                            template,
                            display,
                            settings,
                            paths,
                            elapsed: started.elapsed(),
                        }))
                    })
                    .map_err(|e| match e {
                        shroom_core::Error::Cancelled => JobError::Cancelled,
                        other => JobError::Failed(other.to_string()),
                    });
            send(JobEvent::Done(outcome));
        });
        Self { rx, cancel }
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_cancelling(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn poll(&self) -> Vec<JobEvent> {
        self.rx.try_iter().collect()
    }
}

/// Overall progress in `0..=1` and a description, given the stages the job runs through.
pub fn overall_progress(p: &Progress, aligning: bool, compositing: bool) -> (f32, String) {
    let stages: Vec<Stage> =
        [aligning.then_some(Stage::Aligning), Some(Stage::Analysing), compositing.then_some(Stage::Compositing)]
            .into_iter()
            .flatten()
            .collect();
    let index = stages.iter().position(|s| *s == p.stage).unwrap_or(0);
    let within = (p.frame + 1) as f32 / p.total.max(1) as f32;
    let name = match p.stage {
        Stage::Aligning => "Aligning",
        Stage::Analysing => "Analysing",
        Stage::Compositing => "Compositing",
    };
    let fraction = (index as f32 + within) / stages.len() as f32;
    (fraction.clamp(0.0, 1.0), format!("{name} frame {} of {}", p.frame + 1, p.total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_spans_all_stages() {
        let p = |stage, frame| Progress { stage, frame, total: 10 };
        let (f, text) = overall_progress(&p(Stage::Aligning, 4), true, true);
        assert!((f - 5.0 / 30.0).abs() < 1e-6);
        assert_eq!(text, "Aligning frame 5 of 10");
        let (f, _) = overall_progress(&p(Stage::Compositing, 9), true, true);
        assert_eq!(f, 1.0);
        let (f, _) = overall_progress(&p(Stage::Analysing, 9), false, false);
        assert_eq!(f, 1.0);
        let (f, _) = overall_progress(&p(Stage::Analysing, 4), true, false);
        assert!((f - 0.75).abs() < 1e-6);
    }
}
