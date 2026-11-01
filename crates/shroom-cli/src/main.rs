//! Command line front end for Shroom Stack, used for development and batch processing.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use clap::{Parser, Subcommand};
use shroom_core::io::{self, FrameInfo};
use shroom_core::render::render_display;

#[derive(Parser)]
#[command(name = "shroom", version, about = "Shroom Stack focus stacking (command line)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show metadata of input files in shooting order.
    Info { files: Vec<PathBuf> },
    /// Demosaic a raw DNG and write it as Linear DNG.
    Convert { input: PathBuf, output: PathBuf },
    /// Render a DNG to a JPEG preview.
    Preview {
        input: PathBuf,
        output: PathBuf,
        /// Longest side of the output in pixels.
        #[arg(long, default_value_t = 2048)]
        size: usize,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Info { files } => info(&files),
        Command::Convert { input, output } => convert(&input, &output),
        Command::Preview { input, output, size } => preview(&input, &output, size),
    }
}

fn info(files: &[PathBuf]) -> anyhow::Result<()> {
    let mut frames: Vec<FrameInfo> = files.iter().map(|p| io::read_frame_info(p)).collect::<Result<_, _>>()?;
    io::sort_frames(&mut frames);
    for f in &frames {
        let time = f
            .capture_time
            .map(|t| t.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
            .unwrap_or("-".into());
        let preview = f
            .preview
            .as_ref()
            .map(|p| format!("{}x{}", p.width, p.height))
            .unwrap_or("-".into());
        println!(
            "{}  {}  {}  {}x{}  preview {}",
            f.file_name, time, f.camera, f.width, f.height, preview
        );
    }
    Ok(())
}

fn convert(input: &Path, output: &Path) -> anyhow::Result<()> {
    let t = Instant::now();
    let frame = io::load_linear(input).with_context(|| format!("loading {}", input.display()))?;
    eprintln!("decoded + demosaiced in {:.2?}", t.elapsed());
    let t = Instant::now();
    io::write_linear_dng(
        output,
        &frame.image,
        &frame.template,
        concat!("Shroom Stack ", env!("CARGO_PKG_VERSION")),
    )?;
    eprintln!("written in {:.2?}", t.elapsed());
    Ok(())
}

fn preview(input: &Path, output: &Path, size: usize) -> anyhow::Result<()> {
    let frame = io::load_linear(input).with_context(|| format!("loading {}", input.display()))?;
    let d = render_display(&frame.image, &frame.color, Some(size));
    image::RgbImage::from_raw(d.width as u32, d.height as u32, d.data)
        .context("invalid preview size")?
        .save(output)?;
    Ok(())
}
