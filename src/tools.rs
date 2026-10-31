//! The tools wheel (Shift+Ctrl while dragging): compress, trim, merge, ...
//!
//! Only tools that need no canvas. Crop, redact and annotate would need an
//! editor window and are not here yet.
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::cmd;
use crate::convert::{FFMPEG, HDR_TO_SDR, Job, Step, audio_codec_args, audio_steps, video_steps, which};
use crate::formats::{Category, Fmt, extension, name, output_path};
use crate::media::Streams;
use crate::sys::{self, GHOSTSCRIPT};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arity {
    Single,
    Multiple,
    Any,
}

/// What the app asks for before running the tool.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Asks {
    Trim,
    Resize,
}

#[derive(Debug, PartialEq)]
pub struct Tool {
    pub id: &'static str,
    pub label: &'static str,              // short wheel label
    pub title: &'static str,              // progress / dialog title
    pub categories: &'static [Category],
    pub arity: Arity,
    pub needs: &'static [&'static str],   // programs that must be installed
    pub asks: Option<Asks>,
}

use Category::{Audio as AUDIO, Document as DOC, Image as IMAGE, Video as VIDEO};

pub const TOOLS: &[Tool] = &[
    Tool { id: "compress", label: "COMPRESS", title: "Compressing", categories: &[IMAGE, VIDEO, AUDIO, DOC],
           arity: Arity::Any, needs: &[], asks: None },
    Tool { id: "strip", label: "METADATA", title: "Removing metadata", categories: &[IMAGE, VIDEO, AUDIO],
           arity: Arity::Any, needs: &[], asks: None },
    Tool { id: "resize", label: "RESIZE", title: "Resizing", categories: &[IMAGE],
           arity: Arity::Any, needs: &["magick"], asks: Some(Asks::Resize) },
    Tool { id: "rotate", label: "ROTATE", title: "Rotating", categories: &[IMAGE],
           arity: Arity::Any, needs: &["magick"], asks: None },
    Tool { id: "trim", label: "TRIM", title: "Trimming", categories: &[VIDEO, AUDIO],
           arity: Arity::Single, needs: &["ffmpeg", "ffprobe"], asks: Some(Asks::Trim) },
    Tool { id: "normalize", label: "NORMALIZE", title: "Normalizing", categories: &[AUDIO],
           arity: Arity::Any, needs: &["ffmpeg"], asks: None },
    Tool { id: "make_pdf", label: "PDF", title: "Creating PDF", categories: &[IMAGE],
           arity: Arity::Any, needs: &["magick"], asks: None },
    Tool { id: "merge_pdf", label: "MERGE", title: "Merging", categories: &[DOC],
           arity: Arity::Multiple, needs: &["pdfunite"], asks: None },
];

pub fn by_id(id: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.id == id)
}

/// Values a tool needs: trim's range, resize's longest side.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub start: f64,
    pub end: f64,
    pub long_edge: u32,
}

impl Default for Params {
    fn default() -> Params {
        Params { start: 0.0, end: 0.0, long_edge: 1920 }
    }
}

// compress and strip use whichever program fits the file
fn category_program(category: Category) -> &'static str {
    match category {
        IMAGE => "magick",
        VIDEO | AUDIO => "ffmpeg",
        _ => GHOSTSCRIPT,
    }
}

pub fn available_tools(formats: &[Fmt], which: impl Fn(&str) -> bool) -> Vec<&'static Tool> {
    let Some(first) = formats.first() else { return vec![] };
    let category = first.category();
    if formats.iter().any(|f| f.category() != category) {
        return vec![];
    }
    if formats.contains(&Fmt::Gif) {
        return vec![]; // GIF counts as video, but the video tools assume a real video stream
    }
    TOOLS
        .iter()
        .filter(|tool| tool.categories.contains(&category))
        .filter(|_| category != DOC || formats.iter().all(|&f| f == Fmt::Pdf))
        .filter(|tool| match tool.arity {
            Arity::Single => formats.len() == 1,
            Arity::Multiple => formats.len() >= 2,
            Arity::Any => true,
        })
        .filter(|tool| {
            let program = [category_program(category)];
            let needs = if tool.needs.is_empty() { &program[..] } else { tool.needs };
            needs.iter().all(|p| which(p))
        })
        .collect()
}

// --- compress ----------------------------------------------------------------

fn compress(src: &Path, fmt: Fmt, taken: &HashSet<String>, streams: &Streams) -> Job {
    let (dst, steps) = match fmt.category() {
        IMAGE => {
            let out_fmt = if matches!(fmt, Fmt::Jpg | Fmt::Png | Fmt::Webp | Fmt::Avif) { fmt } else { Fmt::Jpg };
            let dst = output_path(src, out_fmt.value(), "compressed", taken);
            let options: &[&str] = match out_fmt {
                // lossless re-packing rarely beats the original; 256 colours (dithered) does
                Fmt::Png => &["-colors", "256", "-define", "png:compression-level=9"],
                Fmt::Jpg => &["-background", "white", "-alpha", "remove", "-quality", "75"],
                _ => &["-quality", "75"],
            };
            let step = cmd!["magick", format!("{}[0]", src.display()), "-auto-orient", "-strip", options,
                            format!("{}:{}", out_fmt.value(), dst.display())];
            (dst, step)
        }
        VIDEO => {
            let out_fmt = match fmt {
                Fmt::Webm | Fmt::Mp4 | Fmt::Mov | Fmt::Mkv => fmt,
                _ => Fmt::Mp4,
            };
            let dst = output_path(src, out_fmt.value(), "compressed", taken);
            // size is the point here, so CPU encoders: at equal SSIM the GPU's H.264 is
            // ~40% bigger. x264 "faster" vs "medium": 30% quicker for 4% more size.
            let codec: &[&str] = if out_fmt == Fmt::Webm {
                &["-c:v", "libsvtav1", "-preset", "8", "-crf", "42", "-c:a", "libopus", "-b:a", "96k"]
            } else {
                &["-c:v", "libx264", "-preset", "faster", "-crf", "28", "-c:a", "aac", "-b:a", "128k"]
            };
            let look = if streams.hdr() && out_fmt != Fmt::Webm { format!("{HDR_TO_SDR},") } else { String::new() };
            let picture = match streams.video_index {
                Some(i) => format!("0:{i}"),
                None => "0:v:0".into(),
            };
            let step = cmd![&FFMPEG, "-i", src, "-map", picture, "-map", "0:a?", "-map_metadata", "0",
                            "-vf", format!("{look}format=yuv420p"), codec, &dst];
            (dst, step)
        }
        AUDIO => {
            let out_fmt = if matches!(fmt, Fmt::Mp3 | Fmt::Ogg | Fmt::Opus) { fmt } else { Fmt::M4a };
            let dst = output_path(src, out_fmt.value(), "compressed", taken);
            let bitrate: &[&str] = match out_fmt {
                Fmt::Mp3 => &["-c:a", "libmp3lame", "-b:a", "128k"],
                Fmt::Ogg => &["-c:a", "libvorbis", "-q:a", "3"],
                Fmt::Opus => &["-c:a", "libopus", "-b:a", "64k"],
                _ => &["-c:a", "aac", "-b:a", "128k", "-f", "ipod"],
            };
            let step = cmd![&FFMPEG, "-i", src, "-vn", bitrate, &dst];
            (dst, step)
        }
        _ => {
            let dst = output_path(src, "pdf", "compressed", taken);
            let step = cmd![GHOSTSCRIPT, "-sDEVICE=pdfwrite", "-dPDFSETTINGS=/ebook", "-dCompatibilityLevel=1.6",
                            "-dNOPAUSE", "-dBATCH", "-dQUIET", "-dSAFER", format!("-sOutputFile={}", dst.display()), src];
            (dst, step)
        }
    };
    let (from, to) = (src.to_path_buf(), dst.clone());
    Job::new("Compressing", vec![src.into()], vec![dst], vec![steps, Step::func(move || must_be_smaller(&from, &to))])
}

fn must_be_smaller(src: &Path, dst: &Path) -> Result<(), String> {
    let size = |p: &Path| fs::metadata(p).map(|m| m.len()).map_err(|e| e.to_string());
    if size(dst)? >= size(src)? {
        return Err("already as small as it gets".into());
    }
    Ok(())
}

fn strip(src: &Path, fmt: Fmt, taken: &HashSet<String>) -> Job {
    let dst = output_path(src, &extension(src), "clean", taken);
    let mut job = Job::new("Removing metadata", vec![src.into()], vec![dst.clone()], vec![]);
    let (from, to) = (src.to_path_buf(), dst.clone());
    if fmt == Fmt::Jpg && which("jpegtran") {
        // lossless: the compressed picture is copied as it is, only the colour profile kept.
        // Falls back to re-encoding when jpegtran can't (e.g. a rotated photo, see below).
        let (from2, to2) = (from.clone(), to.clone());
        job.steps = vec![Step::func(move || jpegtran_upright(&from, &to))];
        job.fallback = Some(vec![Step::func(move || strip_keep_colour(&from2, &to2))]);
    } else if fmt.category() == IMAGE {
        job.steps = vec![Step::func(move || strip_keep_colour(&from, &to))];
    } else {
        // stream copy: no re-encode, only the tags go. Data tracks go too: phones and
        // action cameras keep GPS and sensor readings there (Apple mebx, GoPro gpmd).
        job.steps = vec![cmd![&FFMPEG, "-i", src, "-map", "0", "-map", "-0:d", "-map_metadata", "-1",
                              "-map_chapters", "-1", "-c", "copy", &dst]];
    }
    job
}

fn jpegtran_upright(src: &Path, dst: &Path) -> Result<(), String> {
    // Removing EXIF also removes the "rotate me" flag, so a sideways-stored photo
    // would show sideways. Leave those to the re-encoding path, which turns them.
    let orientation = sh(cmd!["magick", "identify", "-format", "%[orientation]", src])?;
    let orientation = String::from_utf8_lossy(&orientation).trim().to_string();
    if !matches!(orientation.as_str(), "TopLeft" | "Undefined" | "") {
        return Err(format!("stored rotated ({orientation})"));
    }
    sh(cmd!["jpegtran", "-copy", "icc", "-optimize", "-outfile", dst, src]).map(drop)
}

/// Drop EXIF, GPS, XMP and comments; keep the ICC colour profile, so colours
/// (e.g. Display P3 phone photos) don't shift.
fn strip_keep_colour(src: &Path, dst: &Path) -> Result<(), String> {
    let tmp = tempfile::Builder::new().prefix("shiftdrop-").tempdir().map_err(|e| e.to_string())?;
    let icc = tmp.path().join("colour.icc");
    let has_icc = sys::command("magick").arg(src).arg(format!("icc:{}", icc.display()))
        .output().is_ok_and(|out| out.status.success());
    let profile: Vec<String> = if has_icc { vec!["-profile".into(), icc.display().to_string()] } else { vec![] };
    let png: &[&str] = if extension(src).to_lowercase() == "png" {
        &["-define", "png:exclude-chunks=date,time"] // ImageMagick would add its own dates
    } else {
        &[]
    };
    sh(cmd!["magick", src, "-auto-orient", "-strip", &profile, png, dst]).map(drop)
}

/// Run a command for a function step; its last error line becomes the job's error.
fn sh(step: Step) -> Result<Vec<u8>, String> {
    let args = step.args();
    let done = sys::command(&args[0]).args(&args[1..]).output()
        .map_err(|e| format!("{}: {e}", args[0]))?;
    if !done.status.success() {
        let stderr = String::from_utf8_lossy(&done.stderr);
        let last = stderr.trim().lines().last().map(str::to_owned)
            .unwrap_or_else(|| done.status.code().map_or("?".into(), |c| c.to_string()));
        return Err(format!("{} failed: {last}", args[0]));
    }
    Ok(done.stdout)
}

/// One job per file, except merge/make_pdf which make one output from all inputs.
/// `streams` is probe() per media file; `gpu` lets trim encode on the GPU.
pub fn plan_tool(tool_id: &str, files: &[(PathBuf, Fmt)], params: &Params,
                 streams: &HashMap<PathBuf, Streams>, gpu: bool) -> Result<Vec<Job>, String> {
    let none = Streams::default();
    let info = |p: &Path| streams.get(p).unwrap_or(&none);
    let mut taken = HashSet::new();
    let mut jobs = vec![];
    let mut add = |mut job: Job, taken: &mut HashSet<String>| {
        if job.duration.is_none_or(|d| d == 0.0) {
            job.duration = info(&job.sources[0]).duration;
        }
        taken.extend(job.outputs.iter().map(|p| name(p)));
        jobs.push(job);
    };
    let sources = || files.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>();

    if tool_id == "merge_pdf" || tool_id == "make_pdf" {
        let first = &files.first().ok_or("no files")?.0;
        let job = if tool_id == "merge_pdf" {
            let dst = output_path(first, "pdf", "merged", &taken);
            Job::new("Merging", sources(), vec![dst.clone()], vec![cmd!["pdfunite", &sources(), &dst]])
        } else {
            let dst = output_path(first, "pdf", "", &taken);
            let step = cmd!["magick", &sources(), "-auto-orient", format!("pdf:{}", dst.display())];
            Job::new("Creating PDF", sources(), vec![dst], vec![step])
        };
        add(job, &mut taken);
        return Ok(jobs);
    }

    for (src, fmt) in files {
        let (src, fmt) = (src.as_path(), *fmt);
        let ext = extension(src);
        let job = match tool_id {
            "compress" => compress(src, fmt, &taken, info(src)),
            "strip" => strip(src, fmt, &taken),
            "resize" => {
                let edge = params.long_edge;
                let dst = output_path(src, &ext, &format!("{edge}px"), &taken);
                let step = cmd!["magick", src, "-auto-orient", "-resize", format!("{edge}x{edge}>"), &dst];
                Job::new("Resizing", vec![src.into()], vec![dst], vec![step])
            }
            "rotate" => {
                let dst = output_path(src, &ext, "rotated", &taken);
                let step = cmd!["magick", src, "-auto-orient", "-rotate", "90", &dst];
                Job::new("Rotating", vec![src.into()], vec![dst], vec![step])
            }
            "trim" => {
                let (start, end) = (params.start, params.end);
                if !(0.0 <= start && start < end) {
                    return Err(format!("trim needs 0 <= start < end, got {start}..{end}"));
                }
                let dst = output_path(src, &ext, "trimmed", &taken);
                // -ss/-to after -i: frame-accurate, at the cost of a re-encode
                let cut = ["-ss".into(), format!("{start:.3}"), "-to".into(), format!("{end:.3}")];
                let mut job = Job::new("Trimming", vec![src.into()], vec![dst.clone()], vec![]);
                job.duration = Some(end - start);
                if fmt.category() == AUDIO {
                    job.steps = audio_steps(src, &dst, fmt, info(src), &cut);
                } else {
                    (job.steps, job.fallback) = video_steps(src, &dst, fmt, info(src), gpu, &cut);
                }
                job
            }
            "normalize" => {
                let dst = output_path(src, &ext, "normalized", &taken);
                let step = cmd![&FFMPEG, "-i", src, "-vn", "-af", "loudnorm=I=-16:TP=-1.5:LRA=11",
                                &audio_codec_args(fmt), &dst];
                Job::new("Normalizing", vec![src.into()], vec![dst], vec![step])
            }
            _ => return Err(format!("unknown tool {tool_id:?}")),
        };
        add(job, &mut taken);
    }
    Ok(jobs)
}
