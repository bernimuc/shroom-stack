//! Command line front end for Shroom Stack, used for development and batch processing.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use clap::{Parser, Subcommand};
use shroom_core::DisplayImage;
use shroom_core::io::{self, FrameInfo};
use shroom_core::render::{render_depth_map, render_display};
use shroom_core::stack::{self, CancelToken, FileFrames, Method, Stage};

#[derive(Parser)]
#[command(name = "shroom", version, about = "Shroom Stack focus stacking (command line)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum MethodArg {
    DepthMap,
    Pyramid,
}

#[derive(Subcommand)]
enum Command {
    /// Show metadata of input files in shooting order.
    Info { files: Vec<PathBuf> },
    /// Demosaic a raw DNG and write it as Linear DNG.
    Convert { input: PathBuf, output: PathBuf },
    /// Focus-stack DNG files into a Linear DNG.
    Stack {
        files: Vec<PathBuf>,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long, value_enum, default_value_t = MethodArg::DepthMap)]
        method: MethodArg,
        /// Focus window radius (depth map method).
        #[arg(long, default_value_t = 8)]
        radius: u32,
        #[arg(long, default_value_t = 4)]
        smoothing: u32,
        /// Also write a JPEG preview of the result.
        #[arg(long)]
        preview: Option<PathBuf>,
        /// Also write the depth map (or pyramid source map) as false-colour JPEG.
        #[arg(long)]
        depth_map: Option<PathBuf>,
    },
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
        Command::Stack {
            files,
            output,
            method,
            radius,
            smoothing,
            preview,
            depth_map,
        } => {
            let method = match method {
                MethodArg::DepthMap => Method::DepthMap { radius, smoothing },
                MethodArg::Pyramid => Method::Pyramid { smoothing },
            };
            stack_files(&files, &output, method, preview.as_deref(), depth_map.as_deref())
        }
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

fn stack_files(
    files: &[PathBuf],
    output: &Path,
    method: Method,
    preview: Option<&Path>,
    depth_map: Option<&Path>,
) -> anyhow::Result<()> {
    let mut frames: Vec<FrameInfo> = files.iter().map(|p| io::read_frame_info(p)).collect::<Result<_, _>>()?;
    io::sort_frames(&mut frames);
    let paths: Vec<PathBuf> = frames.into_iter().map(|f| f.path).collect();
    eprintln!("stacking {} frames with {method:?}", paths.len());

    let t = Instant::now();
    let progress = |p: stack::Progress| {
        let stage = match p.stage {
            Stage::Analysing => "analysing",
            Stage::Compositing => "compositing",
        };
        eprint!("\r{stage} {}/{}   ", p.frame + 1, p.total);
    };
    let result = stack::stack(
        &FileFrames { paths: paths.clone() },
        method,
        &progress,
        &CancelToken::default(),
    )?;
    eprintln!("\nstacked in {:.2?}", t.elapsed());

    let first = io::load_linear(&paths[0])?;
    io::write_linear_dng(
        output,
        &result.image,
        &first.template,
        concat!("Shroom Stack ", env!("CARGO_PKG_VERSION")),
    )?;
    if let Some(path) = preview {
        save_jpeg(&render_display(&result.image, &first.color, Some(2048)), path)?;
    }
    if let Some(path) = depth_map {
        save_jpeg(
            &render_depth_map(&result.depth_map, (paths.len() - 1) as f32, Some(2048)),
            path,
        )?;
    }
    Ok(())
}

fn save_jpeg(img: &DisplayImage, path: &Path) -> anyhow::Result<()> {
    image::RgbImage::from_raw(img.width as u32, img.height as u32, img.data.clone())
        .context("invalid image size")?
        .save(path)?;
    Ok(())
}

fn preview(input: &Path, output: &Path, size: usize) -> anyhow::Result<()> {
    let frame = io::load_linear(input).with_context(|| format!("loading {}", input.display()))?;
    save_jpeg(&render_display(&frame.image, &frame.color, Some(size)), output)
}
