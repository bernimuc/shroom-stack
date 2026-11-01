//! Command line front end for Shroom Stack, used for development and batch processing.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use clap::{Parser, Subcommand};
use shroom_core::io::{self, FrameInfo};
use shroom_core::render::{render_depth_map, render_display};
use shroom_core::stack::{self, CancelToken, FileFrames, Method, StackSettings, Stage};
use shroom_core::{DisplayImage, Rect};

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
        /// Skip frame alignment.
        #[arg(long)]
        no_align: bool,
        /// Also write a JPEG preview of the result.
        #[arg(long)]
        preview: Option<PathBuf>,
        /// Also write the depth map (or pyramid source map) as false-colour JPEG.
        #[arg(long)]
        depth_map: Option<PathBuf>,
    },
    /// Compare detail energy of DNGs (development aid for tuning the stackers). Prints the
    /// mean over the whole image and, per tile, the ratio to the first file.
    Sharpness {
        files: Vec<PathBuf>,
        /// Number of tiles horizontally (vertically scaled by aspect ratio).
        #[arg(long, default_value_t = 6)]
        tiles: usize,
    },
    /// Render a DNG to a JPEG preview.
    Preview {
        input: PathBuf,
        output: PathBuf,
        /// Longest side of the output in pixels.
        #[arg(long, default_value_t = 2048)]
        size: usize,
        /// Only render this region at 1:1, as `x,y,width,height`.
        #[arg(long, value_parser = parse_rect)]
        crop: Option<Rect>,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Info { files } => info(&files),
        Command::Convert { input, output } => convert(&input, &output),
        Command::Sharpness { files, tiles } => sharpness(&files, tiles),
        Command::Preview {
            input,
            output,
            size,
            crop,
        } => preview(&input, &output, size, crop),
        Command::Stack {
            files,
            output,
            method,
            radius,
            smoothing,
            no_align,
            preview,
            depth_map,
        } => {
            let method = match method {
                MethodArg::DepthMap => Method::DepthMap { radius, smoothing },
                MethodArg::Pyramid => Method::Pyramid { smoothing },
            };
            let settings = StackSettings {
                method,
                align: !no_align,
            };
            stack_files(&files, &output, &settings, preview.as_deref(), depth_map.as_deref())
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
    settings: &StackSettings,
    preview: Option<&Path>,
    depth_map: Option<&Path>,
) -> anyhow::Result<()> {
    let mut frames: Vec<FrameInfo> = files.iter().map(|p| io::read_frame_info(p)).collect::<Result<_, _>>()?;
    io::sort_frames(&mut frames);
    let paths: Vec<PathBuf> = frames.into_iter().map(|f| f.path).collect();
    eprintln!("stacking {} frames with {settings:?}", paths.len());

    let t = Instant::now();
    let progress = |p: stack::Progress| {
        let stage = match p.stage {
            Stage::Aligning => "aligning",
            Stage::Analysing => "analysing",
            Stage::Compositing => "compositing",
        };
        eprint!("\r{stage} {}/{}   ", p.frame + 1, p.total);
    };
    let result = stack::stack(
        &FileFrames { paths: paths.clone() },
        settings,
        &progress,
        &CancelToken::default(),
    )?;
    eprintln!("\nstacked in {:.2?}", t.elapsed());
    if let Some(alignment) = &result.alignment {
        let last = alignment.transforms.last().unwrap();
        eprintln!(
            "aligned: scale first→last {:.4}, output {}×{} at ({}, {})",
            last.scale_factor(),
            alignment.crop.width,
            alignment.crop.height,
            alignment.crop.x,
            alignment.crop.y
        );
    }

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

fn sharpness(files: &[PathBuf], tiles: usize) -> anyhow::Result<()> {
    use shroom_core::filter::{gaussian_blur, laplacian, luminance};
    let mut grids: Vec<Vec<f64>> = Vec::new();
    let tw = tiles;
    for path in files {
        let frame = io::load_linear(path).with_context(|| format!("loading {}", path.display()))?;
        let lap = laplacian(&gaussian_blur(&luminance(&frame.image), 1.0));
        let (w, h) = (lap.width(), lap.height());
        let th = (tiles * h / w).max(1);
        let mut grid = vec![0.0f64; tw * th];
        for y in 0..h {
            for x in 0..w {
                let v = lap.get(x, y) as f64;
                grid[(y * th / h) * tw + x * tw / w] += v * v;
            }
        }
        let n = (w * h) as f64 / (tw * th) as f64;
        grid.iter_mut().for_each(|g| *g /= n);
        let mean = grid.iter().sum::<f64>() / grid.len() as f64;
        println!("{:.4e}  {}", mean, path.display());
        grids.push(grid);
    }
    for (path, grid) in files.iter().zip(&grids).skip(1) {
        println!("\nper-tile ratio {} / {}:", path.display(), files[0].display());
        for row in grid.chunks(tw).zip(grids[0].chunks(tw)) {
            println!(
                "  {}",
                row.0
                    .iter()
                    .zip(row.1)
                    .map(|(a, b)| format!("{:5.2}", a / b))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
    }
    Ok(())
}

fn parse_rect(s: &str) -> Result<Rect, String> {
    let v: Vec<usize> = s
        .split(',')
        .map(|p| p.trim().parse().map_err(|_| format!("invalid number {p:?}")))
        .collect::<Result<_, _>>()?;
    let [x, y, width, height] = v[..] else {
        return Err("expected x,y,width,height".into());
    };
    Ok(Rect::new(x, y, width, height))
}

fn preview(input: &Path, output: &Path, size: usize, crop: Option<Rect>) -> anyhow::Result<()> {
    let frame = io::load_linear(input).with_context(|| format!("loading {}", input.display()))?;
    let (image, size) = match crop {
        Some(rect) => (frame.image.crop(rect), None),
        None => (frame.image, Some(size)),
    };
    save_jpeg(&render_display(&image, &frame.color, size), output)
}
