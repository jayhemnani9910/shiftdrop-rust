//! Turn "convert this file to that format" into a Job: the commands to run.
//!
//! Planning is pure, so tests can check commands without running them.
//! `jobs::run()` executes a Job.
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::formats::{Category, Fmt, common_targets, extension, output_path, stem};
use crate::media::{RENDER_NODE, Streams};

/// A command, or a bit of Rust (move, mkdir). A function's error becomes the job's error.
pub enum Step {
    Cmd(Vec<String>),
    Func(Box<dyn Fn() -> Result<(), String> + Send + Sync>),
}

impl Step {
    pub fn func(f: impl Fn() -> Result<(), String> + Send + Sync + 'static) -> Step {
        Step::Func(Box::new(f))
    }

    /// The command's arguments; empty for a function step.
    pub fn args(&self) -> &[String] {
        match self {
            Step::Cmd(cmd) => cmd,
            Step::Func(_) => &[],
        }
    }
}

pub struct Job {
    pub title: String,              // "Converting to JPG"
    pub sources: Vec<PathBuf>,
    pub outputs: Vec<PathBuf>,
    pub steps: Vec<Step>,
    pub duration: Option<f64>,      // seconds of media, for ffmpeg progress
    pub cleanup: Vec<PathBuf>,
    pub fallback: Option<Vec<Step>>, // run instead if a step fails (GPU encode -> CPU)
    pub used_fallback: bool,
}

impl Job {
    pub fn new(title: &str, sources: Vec<PathBuf>, outputs: Vec<PathBuf>, steps: Vec<Step>) -> Job {
        Job { title: title.into(), sources, outputs, steps, duration: None, cleanup: vec![],
              fallback: None, used_fallback: false }
    }
}

pub const FFMPEG: [&str; 8] = ["ffmpeg", "-hide_banner", "-nostdin", "-loglevel", "error", "-n", "-progress", "pipe:1"];

/// A command line from string pieces and paths.
#[macro_export]
macro_rules! cmd {
    ($($part:expr),* $(,)?) => {{
        let mut v: Vec<String> = Vec::new();
        $( $crate::convert::Extend::extend_into($part, &mut v); )*
        $crate::convert::Step::Cmd(v)
    }};
}

/// What `cmd!` accepts: a string, a path, or a list of strings.
pub trait Extend {
    fn extend_into(self, v: &mut Vec<String>);
}
fn text(s: impl AsRef<OsStr>) -> String {
    s.as_ref().to_string_lossy().into_owned()
}
impl Extend for &str {
    fn extend_into(self, v: &mut Vec<String>) { v.push(self.into()) }
}
impl Extend for String {
    fn extend_into(self, v: &mut Vec<String>) { v.push(self) }
}
impl Extend for &String {
    fn extend_into(self, v: &mut Vec<String>) { v.push(self.clone()) }
}
impl Extend for &Path {
    fn extend_into(self, v: &mut Vec<String>) { v.push(text(self)) }
}
impl Extend for &PathBuf {
    fn extend_into(self, v: &mut Vec<String>) { v.push(text(self)) }
}
impl<T: AsRef<OsStr>> Extend for &[T] {
    fn extend_into(self, v: &mut Vec<String>) { v.extend(self.iter().map(text)) }
}
impl<T: AsRef<OsStr>, const N: usize> Extend for &[T; N] {
    fn extend_into(self, v: &mut Vec<String>) { v.extend(self.iter().map(text)) }
}
impl<T: AsRef<OsStr>> Extend for &Vec<T> {
    fn extend_into(self, v: &mut Vec<String>) { v.extend(self.iter().map(text)) }
}

// which external program each kind of work needs
fn needs(source: Fmt, target: Fmt) -> &'static [&'static str] {
    match source.category() {
        Category::Video | Category::Audio | Category::Subtitle if target == Fmt::Txt => &[],
        Category::Video | Category::Audio | Category::Subtitle => &["ffmpeg"],
        _ if source == Fmt::Pdf => match target {
            Fmt::Docx => &["soffice"],
            Fmt::Txt => &["pdftotext"],
            _ => &["pdftoppm", "pdfinfo"],
        },
        _ if source == Fmt::Txt => &["soffice"],
        _ if target == Fmt::Heic => &["magick", "heif-enc"],
        _ => &["magick"],
    }
}

/// Targets for the selection that this machine can actually produce.
pub fn available_targets(sources: &[Fmt], which: impl Fn(&str) -> bool) -> Vec<Fmt> {
    common_targets(sources)
        .into_iter()
        .filter(|&t| needs(sources[0], t).iter().all(|tool| which(tool)))
        .collect()
}

/// True when `program` is on PATH, like `shutil.which`.
pub fn which(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| {
        fs::metadata(dir.join(program)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

// --- video and audio ----------------------------------------------------------

// What each container can hold as it is. When the source already fits, the
// stream is copied, not re-encoded: instant, and bit-for-bit the same picture.
fn can_hold_video(target: Fmt) -> &'static [&'static str] {
    match target {
        Fmt::Mp4 => &["h264", "hevc", "av1"],
        Fmt::Mov => &["h264", "hevc"],
        Fmt::Mkv => &["h264", "hevc", "av1", "vp9", "vp8", "mpeg4"],
        Fmt::Webm => &["av1", "vp9", "vp8"],
        _ => &[],
    }
}

fn can_hold_audio(target: Fmt) -> &'static [&'static str] {
    match target {
        Fmt::Mp4 => &["aac", "mp3", "alac"],
        Fmt::Mov => &["aac", "alac"],
        Fmt::Mkv => &["aac", "mp3", "opus", "vorbis", "flac", "ac3", "eac3", "alac"],
        Fmt::Webm => &["opus", "vorbis"],
        Fmt::Mp3 => &["mp3"],
        Fmt::M4a => &["aac", "alac"],
        Fmt::Ogg => &["vorbis"],
        Fmt::Opus => &["opus"],
        Fmt::Flac => &["flac"],
        Fmt::Wav => &["pcm_s16le"],
        Fmt::Aiff => &["pcm_s16be"],
        Fmt::Wma => &["wmav2"],
        _ => &[],
    }
}

fn holds(codecs: &[&str], codec: &Option<String>) -> bool {
    codec.as_deref().is_some_and(|c| codecs.contains(&c))
}

// When re-encoding is needed. Settings were picked by measuring speed and SSIM
// on the busiest 20s of a 1080x1920 60 fps clip (see README, "Speed"):
//   H.264 on the GPU at qp 26: 6.0s, 31.5 MB, SSIM 0.989, against 36.4s,
//   25.6 MB, 0.990 for CPU x264. The Intel driver only offers fixed-qp for
//   H.264, so it can't adapt to busy footage; qp 26 keeps sizes sane.
//   CPU x264 is the fallback when the GPU encoder fails.
//   AV1 (SVT, preset 8) for WebM: half the time of VP9, smaller and sharper.
const H264_GPU: &[&str] = &["-c:v", "h264_vaapi", "-qp", "26"];
const H264_CPU: &[&str] = &["-c:v", "libx264", "-preset", "fast", "-crf", "21"];
const VP9: &[&str] = &["-c:v", "libvpx-vp9", "-crf", "33", "-b:v", "0", "-deadline", "good", "-cpu-used", "4", "-row-mt", "1"];
const H264_FAMILY: [Fmt; 3] = [Fmt::Mp4, Fmt::Mov, Fmt::Mkv];

fn video_codec(target: Fmt) -> &'static [&'static str] {
    match target {
        Fmt::Mp4 | Fmt::Mov | Fmt::Mkv => H264_CPU,
        Fmt::Webm => &["-c:v", "libsvtav1", "-preset", "8", "-crf", "35"],
        Fmt::Avi => &["-c:v", "libxvid", "-q:v", "3"],
        Fmt::Wmv => &["-c:v", "wmv2", "-b:v", "4M"],
        _ => unreachable!("{target:?} is not a video format"),
    }
}

fn sound_in_video(target: Fmt) -> &'static [&'static str] {
    match target {
        Fmt::Mp4 | Fmt::Mov | Fmt::Mkv => &["-c:a", "aac", "-b:a", "160k"],
        Fmt::Webm => &["-c:a", "libopus", "-b:a", "128k"],
        Fmt::Avi => &["-c:a", "libmp3lame", "-b:a", "192k"],
        Fmt::Wmv => &["-c:a", "wmav2", "-b:a", "192k"],
        _ => unreachable!("{target:?} is not a video format"),
    }
}

fn mux(target: Fmt) -> &'static [&'static str] {
    match target {
        Fmt::Mp4 => &["-movflags", "+faststart", "-f", "mp4"],
        Fmt::Mov => &["-f", "mov"],
        Fmt::Mkv => &["-f", "matroska"],
        Fmt::Webm => &["-f", "webm"],
        Fmt::Avi => &["-f", "avi"],
        Fmt::Wmv => &["-f", "asf"],
        Fmt::Mp3 => &["-f", "mp3"],
        Fmt::M4a => &["-f", "ipod"],
        Fmt::Wav => &["-f", "wav"],
        Fmt::Flac => &["-f", "flac"],
        Fmt::Ogg => &["-f", "ogg"],
        Fmt::Opus => &["-f", "opus"],
        Fmt::Aiff => &["-f", "aiff"],
        Fmt::Wma => &["-f", "asf"],
        _ => unreachable!("{target:?} is not a media format"),
    }
}

fn audio_codec(target: Fmt) -> &'static [&'static str] {
    match target {
        Fmt::Mp3 => &["-c:a", "libmp3lame", "-b:a", "192k"],
        Fmt::M4a => &["-c:a", "aac", "-b:a", "192k"],
        Fmt::Wav => &["-c:a", "pcm_s16le"],
        Fmt::Flac => &["-c:a", "flac", "-compression_level", "8"],
        Fmt::Ogg => &["-c:a", "libvorbis", "-q:a", "6"],
        Fmt::Opus => &["-c:a", "libopus", "-b:a", "128k"],
        Fmt::Aiff => &["-c:a", "pcm_s16be"],
        Fmt::Wma => &["-c:a", "wmav2", "-b:a", "192k"],
        _ => unreachable!("{target:?} is not an audio format"),
    }
}

// HDR (PQ/HLG) squeezed into 8-bit SDR looks grey and flat; tone-map it first
pub const HDR_TO_SDR: &str = "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,\
                              tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv";
const EVEN_SIZE: &str = "scale=trunc(iw/2)*2:trunc(ih/2)*2";
// 15 fps, at most 640 wide, palette built from what changes between frames
const GIF_FILTER: &str = "fps=15,scale=w='min(640,iw)':h=-2:flags=lanczos,split[s0][s1];\
                          [s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=3";

/// Re-encode to an audio format (used when a filter, e.g. loudnorm, rules out copying).
pub fn audio_codec_args(target: Fmt) -> Vec<String> {
    [audio_codec(target), mux(target)].concat().into_iter().map(String::from).collect()
}

/// Audio file out: copy the track when the target can hold it, else encode.
pub fn audio_steps(src: &Path, dst: &Path, target: Fmt, streams: &Streams, cut: &[String]) -> Vec<Step> {
    let codec: &[&str] = if cut.is_empty() && holds(can_hold_audio(target), &streams.audio) {
        &["-c:a", "copy"]
    } else {
        audio_codec(target)
    };
    vec![cmd![&FFMPEG, "-i", src, cut, "-map", "0:a:0", "-map_metadata", "0", codec, mux(target), dst]]
}

/// Video file out. Returns (steps, fallback): the fallback is the CPU encode
/// to use if the GPU encode fails. A cut (trim) always re-encodes, so it can
/// start on any frame rather than the nearest keyframe.
pub fn video_steps(src: &Path, dst: &Path, target: Fmt, streams: &Streams, gpu: bool,
                   cut: &[String]) -> (Vec<Step>, Option<Vec<Step>>) {
    let picture = match streams.video_index {
        Some(i) => format!("0:{i}"),
        None => "0:v:0".into(),
    };
    let maps = ["-map", &picture, "-map", "0:a?", "-map_metadata", "0"];
    let sound: &[&str] = if cut.is_empty() && holds(can_hold_audio(target), &streams.audio) {
        &["-c:a", "copy"]
    } else {
        sound_in_video(target)
    };
    let mux = mux(target);

    if cut.is_empty() && holds(can_hold_video(target), &streams.video) {
        let apple_tag: &[&str] = if streams.video.as_deref() == Some("hevc") && matches!(target, Fmt::Mp4 | Fmt::Mov) {
            &["-tag:v", "hvc1"]
        } else {
            &[]
        };
        return (vec![cmd![&FFMPEG, "-i", src, &maps, "-c:v", "copy", apple_tag, sound, mux, dst]], None);
    }

    let mut filters = vec![];
    if streams.video.as_deref() == Some("gif") {
        filters.push(EVEN_SIZE); // H.264 and friends need even sizes; GIFs often aren't
    }
    if streams.hdr() && target != Fmt::Webm {
        filters.push(HDR_TO_SDR); // AV1 in WebM can carry HDR as it is
    }

    let command = |before_input: &[&str], last_filters: &[&str], codec: &[&str]| {
        let chain = [filters.as_slice(), last_filters].concat().join(",");
        let vf: Vec<String> = if chain.is_empty() { vec![] } else { vec!["-vf".into(), chain] };
        cmd![&FFMPEG, before_input, "-i", src, cut, &maps, &vf, codec, sound, mux, dst]
    };

    let eight_bit: &[&str] = if target == Fmt::Webm && streams.hdr() { &[] } else { &["format=yuv420p"] };
    let cpu = command(&[], eight_bit, video_codec(target));
    if gpu && H264_FAMILY.contains(&target) {
        let gpu_cmd = command(&["-vaapi_device", RENDER_NODE], &["format=nv12", "hwupload"], H264_GPU);
        return (vec![gpu_cmd], Some(vec![cpu]));
    }
    if target == Fmt::Webm {
        // SVT-AV1 refuses frames under 64 px on a side; VP9 takes anything
        return (vec![cpu], Some(vec![command(&[], eight_bit, VP9)]));
    }
    (vec![cpu], None)
}

fn magick(src: &Path, dst: &Path, target: Fmt) -> Step {
    let input = if target == Fmt::Pdf { src.display().to_string() } else { format!("{}[0]", src.display()) };
    let mut args = vec!["magick".to_string(), input, "-auto-orient".into()];
    let mut add = |a: &[&str]| args.extend(a.iter().map(|s| s.to_string()));
    if matches!(target, Fmt::Jpg | Fmt::Bmp) {
        add(&["-background", "white", "-alpha", "remove", "-alpha", "off"]);
    }
    if matches!(target, Fmt::Jpg | Fmt::Webp | Fmt::Avif) {
        add(&["-quality", "90"]);
    }
    args.push(format!("{}:{}", target.value(), dst.display()));
    Step::Cmd(args)
}

/// LibreOffice writes `<stem>.<ext>` into --outdir; convert in a private dir, then move.
fn soffice(src: &Path, dst: &Path, filter: &str, workdir: &Path, infilter: Option<&str>) -> Vec<Step> {
    let profile = workdir.join("profile");
    let mut args = vec!["soffice".to_string(), format!("-env:UserInstallation=file://{}", profile.display()),
                        "--headless".into()];
    if let Some(infilter) = infilter {
        args.push(format!("--infilter={infilter}"));
    }
    args.extend(["--convert-to".into(), filter.into(), "--outdir".into(), workdir.display().to_string(),
                 src.display().to_string()]);
    let ext = filter.split(':').next().unwrap_or(filter);
    let produced = workdir.join(format!("{}.{ext}", stem(src)));
    let dst = dst.to_path_buf();
    vec![Step::Cmd(args), Step::func(move || move_file(&produced, &dst).map_err(|e| e.to_string()))]
}

/// Rename, or copy and remove when `to` is on another filesystem (like `shutil.move`).
pub fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to)?;
    fs::remove_file(from)
}

/// Keep the spoken lines, drop cue numbers, timings and the WEBVTT header.
pub fn subtitles_to_text(src: &Path, dst: &Path) -> io::Result<()> {
    let text = String::from_utf8_lossy(&fs::read(src)?).into_owned();
    let mut lines = vec![];
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.chars().all(|c| c.is_ascii_digit()) || line.contains("-->") || line.starts_with("WEBVTT") {
            continue;
        }
        lines.push(strip_tags(line));
    }
    fs::write(dst, lines.join("\n") + "\n")
}

/// Remove `<i>`, `</b>`, `<font ...>` and the like.
fn strip_tags(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(open) = rest.find('<') {
        match rest[open + 1..].find('>') {
            Some(len) if len > 0 => {
                out.push_str(&rest[..open]);
                rest = &rest[open + 1 + len + 1..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = &rest[open + 1..];
            }
        }
    }
    out + rest
}

/// A private folder under /tmp, removed by `jobs::run` when the job ends.
fn work_dir() -> io::Result<PathBuf> {
    Ok(tempfile::Builder::new().prefix("shiftdrop-").tempdir()?.keep())
}

/// `streams` is what probe() found inside a media file; without it, nothing is copied.
pub fn plan_conversion(src: &Path, source: Fmt, target: Fmt, streams: Option<&Streams>, gpu: bool,
                       pages: u32, taken: &HashSet<String>) -> io::Result<Job> {
    let none = Streams::default();
    let streams = streams.unwrap_or(&none);
    let title = format!("Converting to {}", target.label());
    let multi_page = source == Fmt::Pdf && matches!(target, Fmt::Jpg | Fmt::Png) && pages > 1;
    let dst = if multi_page {
        output_path(src, "", "pages", taken)
    } else {
        output_path(src, target.value(), "", taken)
    };
    let mut job = Job::new(&title, vec![src.into()], vec![dst.clone()], vec![]);
    job.duration = streams.duration;

    if source.category() == Category::Video && target == Fmt::Gif {
        job.steps = vec![cmd![&FFMPEG, "-i", src, "-filter_complex", GIF_FILTER, "-loop", "0", "-f", "gif", &dst]];
    } else if target.category() == Category::Audio {
        job.steps = audio_steps(src, &dst, target, streams, &[]);
    } else if source.category() == Category::Video {
        (job.steps, job.fallback) = video_steps(src, &dst, target, streams, gpu, &[]);
    } else if source.category() == Category::Subtitle && target == Fmt::Txt {
        let src = src.to_path_buf();
        job.steps = vec![Step::func(move || subtitles_to_text(&src, &dst).map_err(|e| e.to_string()))];
    } else if source.category() == Category::Subtitle {
        let kind = if target == Fmt::Vtt { "webvtt" } else { "srt" };
        job.steps = vec![cmd![&FFMPEG, "-i", src, "-f", kind, &dst]];
    } else if source == Fmt::Pdf && target == Fmt::Txt {
        job.steps = vec![cmd!["pdftotext", "-layout", src, &dst]];
    } else if source == Fmt::Pdf && matches!(target, Fmt::Jpg | Fmt::Png) {
        let kind = if target == Fmt::Jpg { "-jpeg" } else { "-png" };
        if multi_page {
            let folder = dst.clone();
            job.steps = vec![Step::func(move || fs::create_dir(&folder).map_err(|e| e.to_string())),
                             cmd!["pdftoppm", "-r", "150", kind, src, &dst.join(stem(src))]];
        } else {
            job.steps = vec![cmd!["pdftoppm", "-r", "150", kind, "-singlefile", src, &dst.with_extension("")]];
        }
    } else if target == Fmt::Docx || source == Fmt::Txt {
        let work = work_dir()?;
        job.cleanup.push(work.clone());
        let infilter = (source == Fmt::Pdf).then_some("writer_pdf_import");
        let src_for_office = if source == Fmt::Txt && extension(src).to_lowercase() != "txt" {
            // LibreOffice picks its importer from the extension; give .md/.log a .txt name
            let staged = work.join(format!("{}.txt", stem(src)));
            let (from, to) = (src.to_path_buf(), staged.clone());
            job.steps = vec![Step::func(move || fs::copy(&from, &to).map(drop).map_err(|e| e.to_string()))];
            staged
        } else {
            src.to_path_buf()
        };
        let filter = if target == Fmt::Docx { "docx:MS Word 2007 XML" } else { "pdf" };
        job.steps.extend(soffice(&src_for_office, &dst, filter, &work, infilter));
    } else if target == Fmt::Heic {
        let work = work_dir()?;
        job.cleanup.push(work.clone());
        let staged = work.join("staged.png");
        job.steps = vec![magick(src, &staged, Fmt::Png), cmd!["heif-enc", "-q", "85", "-o", &dst, &staged]];
    } else {
        job.steps = vec![magick(src, &dst, target)];
    }
    Ok(job)
}
