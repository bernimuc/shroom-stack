//! Command line front end for Shroom Stack, used for development and batch processing.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use clap::{Parser, Subcommand};
use shroom_core::io::{self, FrameInfo};
use shroom_core::render::{render_depth_map, render_display};
use shroom_core::stack::{self, BokehFrame, CancelToken, FileFrames, Method, SoftBokeh, StackSettings, Stage};
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

#[derive(Clone, Copy, clap::ValueEnum)]
enum BokehArg {
    MostBlurred,
    NearestFocus,
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
        /// Compute on the CPU even if a GPU is available.
        #[arg(long)]
        cpu: bool,
        /// Also write a JPEG preview of the result.
        #[arg(long)]
        preview: Option<PathBuf>,
        /// Also write the depth map (or pyramid source map) as false-colour JPEG.
        #[arg(long)]
        depth_map: Option<PathBuf>,
        /// Keep out-of-focus backgrounds soft, taken from this frame.
        #[arg(long, value_enum)]
        bokeh: Option<BokehArg>,
        /// Soft bokeh detection threshold (1–10, higher detects more).
        #[arg(long, default_value_t = SoftBokeh::DEFAULT.threshold)]
        bokeh_threshold: u32,
        /// Also write a JPEG preview with the soft bokeh areas tinted.
        #[arg(long)]
        bokeh_mask: Option<PathBuf>,
    },
    /// Compare detail energy of DNGs (development aid for tuning the stackers). Prints the
    /// mean over the whole image and, per tile, the ratio to the first file.
    Sharpness {
        files: Vec<PathBuf>,
        /// Number of tiles horizontally (vertically scaled by aspect ratio).
        #[arg(long, default_value_t = 6)]
        tiles: usize,
    },
    /// Colour statistics of the dark areas (development aid): uses the pixels that are dark
    /// in the first file, assuming all files share its geometry.
    Shadows {
        files: Vec<PathBuf>,
        /// Luminance below which a pixel counts as dark (0..1 of white).
        #[arg(long, default_value_t = 0.01)]
        threshold: f32,
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
        /// Exposure adjustment in stops (e.g. 3 to inspect shadows).
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        ev: f32,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Info { files } => info(&files),
        Command::Convert { input, output } => convert(&input, &output),
        Command::Sharpness { files, tiles } => sharpness(&files, tiles),
        Command::Shadows { files, threshold } => shadows(&files, threshold),
        Command::Preview { input, output, size, crop, ev } => preview(&input, &output, size, crop, ev),
        Command::Stack {
            files,
            output,
            method,
            radius,
            smoothing,
            no_align,
            cpu,
            preview,
            depth_map,
            bokeh,
            bokeh_threshold,
            bokeh_mask,
        } => {
            let method = match method {
                MethodArg::DepthMap => Method::DepthMap { radius, smoothing },
                MethodArg::Pyramid => Method::Pyramid { smoothing },
            };
            let bokeh = bokeh.map(|frame| SoftBokeh {
                frame: match frame {
                    BokehArg::MostBlurred => BokehFrame::MostBlurred,
                    BokehArg::NearestFocus => BokehFrame::NearestFocus,
                },
                threshold: bokeh_threshold,
            });
            let settings = StackSettings { method, align: !no_align, use_gpu: !cpu, bokeh };
            let outputs = Outputs {
                preview: preview.as_deref(),
                depth_map: depth_map.as_deref(),
                bokeh_mask: bokeh_mask.as_deref(),
            };
            stack_files(&files, &output, &settings, &outputs)
        }
    }
}

fn info(files: &[PathBuf]) -> anyhow::Result<()> {
    let mut frames: Vec<FrameInfo> = files.iter().map(|p| io::read_frame_info(p)).collect::<Result<_, _>>()?;
    io::sort_frames(&mut frames);
    for f in &frames {
        let time = f.capture_time.map(|t| t.format("%Y-%m-%d %H:%M:%S%.3f").to_string()).unwrap_or("-".into());
        let preview = f.preview.as_ref().map(|p| format!("{}x{}", p.width, p.height)).unwrap_or("-".into());
        println!(
            "{}  {}  {}  {}x{}  {}°  preview {}",
            f.file_name,
            time,
            f.camera,
            f.width,
            f.height,
            f.orientation.degrees(),
            preview
        );
    }
    Ok(())
}

fn convert(input: &Path, output: &Path) -> anyhow::Result<()> {
    let t = Instant::now();
    let frame = io::load_linear(input).with_context(|| format!("loading {}", input.display()))?;
    eprintln!("decoded + demosaiced in {:.2?}", t.elapsed());
    let t = Instant::now();
    io::write_linear_dng(output, &frame.image, &frame.template, concat!("Shroom Stack ", env!("CARGO_PKG_VERSION")))?;
    eprintln!("written in {:.2?}", t.elapsed());
    Ok(())
}

/// Optional extra outputs of `stack`.
struct Outputs<'a> {
    preview: Option<&'a Path>,
    depth_map: Option<&'a Path>,
    bokeh_mask: Option<&'a Path>,
}

fn stack_files(files: &[PathBuf], output: &Path, settings: &StackSettings, outputs: &Outputs) -> anyhow::Result<()> {
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
    let result = stack::stack(&FileFrames { paths: paths.clone() }, settings, &progress, &CancelToken::default())?;
    eprintln!("\nstacked in {:.2?} ({})", t.elapsed(), result.backend);
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
    let template = first.template.with_sources(&paths);
    io::write_linear_dng(output, &result.image, &template, concat!("Shroom Stack ", env!("CARGO_PKG_VERSION")))?;
    if let Some(path) = outputs.preview {
        save_jpeg(&render_display(&result.image, &first.color, Some(2048)), path)?;
    }
    if let Some(path) = outputs.depth_map {
        save_jpeg(&render_depth_map(&result.depth_map, (paths.len() - 1) as f32, Some(2048)), path)?;
    }
    if let Some(path) = outputs.bokeh_mask {
        let mut img = render_display(&result.image, &first.color, Some(2048));
        if let Some(mask) = &result.bokeh_mask {
            let (sx, sy) = (mask.width() as f32 / img.width as f32, mask.height() as f32 / img.height as f32);
            for (i, px) in img.data.chunks_mut(3).enumerate() {
                let (x, y) = ((i % img.width) as f32 * sx, (i / img.width) as f32 * sy);
                let m = mask.get((x as usize).min(mask.width() - 1), (y as usize).min(mask.height() - 1));
                px[0] = (px[0] as f32 * (1.0 - 0.6 * m) + 255.0 * 0.6 * m) as u8;
            }
        } else {
            eprintln!("no soft bokeh areas found");
        }
        save_jpeg(&img, path)?;
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
                row.0.iter().zip(row.1).map(|(a, b)| format!("{:5.2}", a / b)).collect::<Vec<_>>().join(" ")
            );
        }
    }
    Ok(())
}

fn shadows(files: &[PathBuf], threshold: f32) -> anyhow::Result<()> {
    let mut mask: Option<Vec<bool>> = None;
    println!(
        "{:>10} {:>10} {:>10}   {:>9} {:>9} {:>9}   {:>7} {:>7} {:>7}  file",
        "mean R", "mean G", "mean B", "std R", "std G", "std B", "0 R", "0 G", "0 B"
    );
    for path in files {
        let img = io::load_linear(path).with_context(|| format!("loading {}", path.display()))?.image;
        let px: Vec<[f32; 3]> = img.data().chunks(3).map(|p| [p[0], p[1], p[2]]).collect();
        let mask =
            mask.get_or_insert_with(|| px.iter().map(|p| (p[0] + 2.0 * p[1] + p[2]) / 4.0 < threshold).collect());
        let n = mask.len().min(px.len());
        let dark: Vec<[f32; 3]> = (0..n).filter(|&i| mask[i]).map(|i| px[i]).collect();
        let count = dark.len().max(1) as f64;
        let mean: [f64; 3] = std::array::from_fn(|c| dark.iter().map(|p| p[c] as f64).sum::<f64>() / count);
        let std: [f64; 3] =
            std::array::from_fn(|c| (dark.iter().map(|p| (p[c] as f64 - mean[c]).powi(2)).sum::<f64>() / count).sqrt());
        let zeros: [f64; 3] =
            std::array::from_fn(|c| dark.iter().filter(|p| p[c] <= 0.0).count() as f64 / count * 100.0);
        println!(
            "{:10.6} {:10.6} {:10.6}   {:9.6} {:9.6} {:9.6}   {:6.2}% {:6.2}% {:6.2}%  {}",
            mean[0],
            mean[1],
            mean[2],
            std[0],
            std[1],
            std[2],
            zeros[0],
            zeros[1],
            zeros[2],
            path.display()
        );
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

fn preview(input: &Path, output: &Path, size: usize, crop: Option<Rect>, ev: f32) -> anyhow::Result<()> {
    let frame = io::load_linear(input).with_context(|| format!("loading {}", input.display()))?;
    let (mut image, size) = match crop {
        Some(rect) => (frame.image.crop(rect), None),
        None => (frame.image, Some(size)),
    };
    let gain = 2f32.powf(ev);
    image.data_mut().iter_mut().for_each(|v| *v *= gain);
    save_jpeg(&render_display(&image, &frame.color, size), output)
}
