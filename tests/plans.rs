use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use shiftdrop::convert::{Job, Step, available_targets, plan_conversion, subtitles_to_text};
use shiftdrop::formats::{Fmt, common_targets, detect, output_path, output_path_in, targets};
use shiftdrop::jobs::{parse_progress, run};
use shiftdrop::media::{Streams, parse};
use shiftdrop::tools::{Params, available_tools, plan_tool};

fn everything(_tool: &str) -> bool {
    true
}

fn none() -> HashSet<String> {
    HashSet::new()
}

/// Plan converting `name` in an empty temp folder.
fn convert(name: &str, source: Fmt, target: Fmt, streams: Option<&Streams>, gpu: bool) -> Job {
    let tmp = tempfile::tempdir().unwrap();
    plan_conversion(&tmp.path().join(name), source, target, streams, gpu, 1, &none()).unwrap()
}

/// The argument right after `flag`.
fn after<'a>(cmd: &'a [String], flag: &str) -> &'a str {
    &cmd[cmd.iter().position(|a| a == flag).unwrap_or_else(|| panic!("no {flag} in {cmd:?}")) + 1]
}

fn has(cmd: &[String], arg: &str) -> bool {
    cmd.iter().any(|a| a == arg)
}

fn tool_ids(formats: &[Fmt]) -> Vec<&'static str> {
    available_tools(formats, everything).iter().map(|t| t.id).collect()
}

fn h264_aac() -> Streams {
    Streams { duration: Some(10.0), video: Some("h264".into()), video_index: Some(0), audio: Some("aac".into()),
              transfer: None }
}

fn streams(video: &str, audio: &str) -> Streams {
    Streams { video: Some(video.into()), video_index: Some(0), audio: Some(audio.into()), ..Default::default() }
}

#[test]
fn detect_aliases() {
    assert_eq!(detect("a/b/Photo.JPEG"), Some(Fmt::Jpg));
    assert_eq!(detect("clip.m4v"), Some(Fmt::Mp4));
    assert_eq!(detect("notes.md"), Some(Fmt::Txt));
    assert_eq!(detect("archive.zip"), None);
    assert_eq!(detect("noext"), None);
}

#[test]
fn targets_never_include_source() {
    for fmt in Fmt::all() {
        assert!(!targets(fmt).contains(&fmt));
    }
}

#[test]
fn video_targets_other_videos_then_gif_then_audio() {
    use Fmt::*;
    assert_eq!(targets(Mp4), [Mov, Mkv, Webm, Avi, Wmv, Gif, Mp3, M4a, Wav]);
}

#[test]
fn gif_becomes_video_only() {
    use Fmt::*;
    assert_eq!(targets(Gif), [Mp4, Mov, Mkv, Webm, Avi, Wmv]);
}

#[test]
fn svg_is_read_never_written() {
    assert_eq!(detect("logo.svg"), Some(Fmt::Svg));
    assert!(Fmt::all().all(|f| !targets(f).contains(&Fmt::Svg)));
}

#[test]
fn video_to_m4a_drops_the_picture() {
    let job = convert("a.mp4", Fmt::Mp4, Fmt::M4a, None, false);
    let cmd = job.steps[0].args();
    let maps: Vec<&str> = cmd.windows(2).filter(|w| w[0] == "-map").map(|w| w[1].as_str()).collect();
    assert_eq!(maps, ["0:a:0"]); // only the sound is taken
    assert!(has(cmd, "aac"));
}

#[test]
fn common_targets_mixed_selection() {
    use Fmt::*;
    assert_eq!(common_targets(&[Png, Jpg]), [Webp, Avif, Heic, Tiff, Bmp, Pdf]);
    assert_eq!(common_targets(&[Png, Mp4]), []);
}

#[test]
fn heic_only_offered_with_heif_enc() {
    let no_heif = |tool: &str| tool != "heif-enc";
    assert!(!available_targets(&[Fmt::Png], no_heif).contains(&Fmt::Heic));
    assert!(available_targets(&[Fmt::Png], everything).contains(&Fmt::Heic));
}

#[test]
fn nothing_offered_without_ffmpeg() {
    assert_eq!(available_targets(&[Fmt::Mp4], |_| false), []);
}

#[test]
fn output_path_never_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("photo.png");
    fs::write(&src, "").unwrap();
    fs::write(tmp.path().join("photo.jpg"), "").unwrap();
    let name = |p: PathBuf| p.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(name(output_path(&src, "jpg", "", &none())), "photo 2.jpg");
    assert_eq!(name(output_path(&src, "jpg", "", &HashSet::from(["photo 2.jpg".into()]))), "photo 3.jpg");
    assert_eq!(name(output_path(&src, "png", "compressed", &none())), "photo compressed.png");
}

#[test]
fn h264_to_webm_encodes_av1() {
    let job = convert("a.mp4", Fmt::Mp4, Fmt::Webm, Some(&h264_aac()), false);
    let cmd = job.steps[0].args();
    assert!(has(cmd, "libsvtav1") && has(cmd, "libopus") && has(cmd, "-n"));
    assert!(cmd.last().unwrap().ends_with("a.webm"));
    assert_eq!(job.duration, Some(10.0));
    let fallback = job.fallback.as_ref().expect("a fallback for frames AV1 won't take");
    assert!(has(fallback[0].args(), "libvpx-vp9"));
}

#[test]
fn vp9_in_mp4_to_webm_copies_the_picture() {
    let job = convert("a.mp4", Fmt::Mp4, Fmt::Webm, Some(&streams("vp9", "aac")), false);
    let cmd = job.steps[0].args();
    assert_eq!(after(cmd, "-c:v"), "copy");
    assert_eq!(after(cmd, "-c:a"), "libopus"); // WebM can't hold AAC
}

#[test]
fn mp4_to_mkv_copies_everything() {
    let job = convert("a.mp4", Fmt::Mp4, Fmt::Mkv, Some(&h264_aac()), true);
    let cmd = job.steps[0].args();
    assert_eq!(after(cmd, "-c:v"), "copy");
    assert_eq!(after(cmd, "-c:a"), "copy");
    assert!(!has(cmd, "h264_vaapi"));
}

#[test]
fn hevc_copied_into_mp4_gets_the_apple_tag() {
    let job = convert("a.mkv", Fmt::Mkv, Fmt::Mp4, Some(&streams("hevc", "aac")), false);
    assert_eq!(after(job.steps[0].args(), "-tag:v"), "hvc1");
}

#[test]
fn reencode_uses_gpu_with_cpu_fallback() {
    let job = convert("a.webm", Fmt::Webm, Fmt::Mp4, Some(&streams("vp9", "opus")), true);
    assert!(has(job.steps[0].args(), "h264_vaapi") && has(job.steps[0].args(), "-vaapi_device"));
    assert!(has(job.fallback.as_ref().unwrap()[0].args(), "libx264"));
}

#[test]
fn cover_art_is_not_taken_as_the_picture() {
    let info = serde_json::json!({"format": {"duration": "3.5"}, "streams": [
        {"index": 0, "codec_type": "video", "codec_name": "mjpeg", "disposition": {"attached_pic": 1}},
        {"index": 1, "codec_type": "audio", "codec_name": "aac"},
        {"index": 2, "codec_type": "video", "codec_name": "h264", "color_transfer": "arib-std-b67"},
    ]});
    let s = parse(&info);
    assert_eq!((s.video.as_deref(), s.video_index, s.audio.as_deref(), s.duration),
               (Some("h264"), Some(2), Some("aac"), Some(3.5)));
    assert!(s.hdr());
}

#[test]
fn hdr_is_tone_mapped_for_h264_but_kept_in_webm() {
    let hlg = Streams { transfer: Some("arib-std-b67".into()), ..streams("hevc", "aac") };
    let avi = convert("a.mov", Fmt::Mov, Fmt::Avi, Some(&hlg), false);
    assert!(after(avi.steps[0].args(), "-vf").contains("tonemap"));
    let webm = convert("a.mov", Fmt::Mov, Fmt::Webm, Some(&hlg), false);
    assert!(!has(webm.steps[0].args(), "-vf"));
}

#[test]
fn audio_copied_when_the_target_holds_it() {
    let m4a = convert("a.mp4", Fmt::Mp4, Fmt::M4a, Some(&h264_aac()), false);
    assert_eq!(after(m4a.steps[0].args(), "-c:a"), "copy");
    let mp3 = convert("a.mp4", Fmt::Mp4, Fmt::Mp3, Some(&h264_aac()), false);
    assert_eq!(after(mp3.steps[0].args(), "-c:a"), "libmp3lame");
}

#[test]
fn trim_never_copies() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("a.mp4");
    let params = Params { start: 1.0, end: 2.0, ..Default::default() };
    let media = HashMap::from([(src.clone(), h264_aac())]);
    let jobs = plan_tool("trim", &[(src, Fmt::Mp4)], &params, &media, true).unwrap();
    let [job] = jobs.as_slice() else { panic!("one job") };
    assert!(!has(job.steps[0].args(), "copy") && has(job.steps[0].args(), "h264_vaapi"));
    assert_eq!(job.duration, Some(1.0));
}

#[test]
fn failed_gpu_step_falls_back() {
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("out.txt");
    let mut job = Job::new("x", vec![tmp.path().join("in")], vec![dst.clone()], vec![shiftdrop::cmd!["false"]]);
    let to = dst.clone();
    job.fallback = Some(vec![Step::func(move || fs::write(&to, "cpu").map_err(|e| e.to_string()))]);
    assert_eq!(run(&mut job, &|_| {}, &AtomicBool::new(false)), Ok(vec![dst.clone()]));
    assert!(job.used_fallback);
    assert_eq!(fs::read_to_string(&dst).unwrap(), "cpu");
}

#[test]
fn no_fallback_means_the_error_stands() {
    let tmp = tempfile::tempdir().unwrap();
    let mut job = Job::new("x", vec![tmp.path().join("in")], vec![tmp.path().join("out.txt")],
                           vec![shiftdrop::cmd!["false"]]);
    assert!(run(&mut job, &|_| {}, &AtomicBool::new(false)).is_err());
}

#[test]
fn plan_png_to_jpg_flattens_alpha() {
    let tmp = tempfile::tempdir().unwrap();
    let job = plan_conversion(&tmp.path().join("a.png"), Fmt::Png, Fmt::Jpg, None, false, 1, &none()).unwrap();
    let cmd = job.steps[0].args();
    assert!(cmd[0] == "magick" && has(cmd, "remove"));
    assert_eq!(cmd.last().unwrap(), &format!("jpg:{}", tmp.path().join("a.jpg").display()));
}

#[test]
fn plan_multipage_pdf_goes_to_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("doc.pdf");
    let job = plan_conversion(&src, Fmt::Pdf, Fmt::Png, None, false, 3, &none()).unwrap();
    assert_eq!(job.outputs[0].file_name().unwrap(), "doc pages");
    let one = plan_conversion(&src, Fmt::Pdf, Fmt::Png, None, false, 1, &none()).unwrap();
    assert_eq!(one.outputs[0].file_name().unwrap(), "doc.png");
    assert!(has(one.steps[0].args(), "-singlefile"));
}

#[test]
fn subtitles_become_text() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("a.vtt");
    fs::write(&src, "WEBVTT\n\n1\n00:00:00.000 --> 00:00:01.000\n<i>Hello</i>\n\n2\n00:00:01.000 --> 00:00:02.000\nworld\n").unwrap();
    let dst = tmp.path().join("a.txt");
    subtitles_to_text(&src, &dst).unwrap();
    assert_eq!(fs::read_to_string(&dst).unwrap(), "Hello\nworld\n");
}

#[test]
fn progress_lines() {
    for (line, expected) in [
        ("out_time_us=5000000", Some(0.5)),
        ("out_time_us=N/A", None),
        ("frame=10", None),
        ("out_time_us=99000000", Some(1.0)),
    ] {
        assert_eq!(parse_progress(line, Some(10.0)), expected, "{line}");
    }
}

#[test]
fn tools_for_images() {
    assert_eq!(tool_ids(&[Fmt::Png]), ["compress", "strip", "resize", "rotate", "make_pdf"]);
}

#[test]
fn trim_only_for_one_file() {
    assert!(tool_ids(&[Fmt::Mp4]).contains(&"trim"));
    assert!(!tool_ids(&[Fmt::Mp4, Fmt::Mov]).contains(&"trim"));
}

#[test]
fn merge_needs_two_pdfs() {
    assert!(!tool_ids(&[Fmt::Pdf]).contains(&"merge_pdf"));
    assert!(tool_ids(&[Fmt::Pdf, Fmt::Pdf]).contains(&"merge_pdf"));
    assert!(tool_ids(&[Fmt::Pdf, Fmt::Docx]).is_empty());
}

#[test]
fn no_tools_for_mixed_or_gif() {
    assert!(tool_ids(&[Fmt::Png, Fmt::Mp3]).is_empty());
    assert!(tool_ids(&[Fmt::Gif]).is_empty());
}

#[test]
fn trim_rejects_bad_range() {
    let tmp = tempfile::tempdir().unwrap();
    let params = Params { start: 5.0, end: 2.0, ..Default::default() };
    assert!(plan_tool("trim", &[(tmp.path().join("a.mp4"), Fmt::Mp4)], &params, &HashMap::new(), false).is_err());
}

#[test]
fn merge_is_one_job() {
    let tmp = tempfile::tempdir().unwrap();
    let files = [(tmp.path().join("a.pdf"), Fmt::Pdf), (tmp.path().join("b.pdf"), Fmt::Pdf)];
    let jobs = plan_tool("merge_pdf", &files, &Params::default(), &HashMap::new(), false).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].outputs[0].file_name().unwrap(), "a merged.pdf");
}

#[test]
fn output_goes_to_fallback_when_folder_is_read_only() {
    let tmp = tempfile::tempdir().unwrap();
    let locked = tmp.path().join("locked");
    fs::create_dir(&locked).unwrap();
    let src = locked.join("photo.png");
    fs::write(&src, "").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    let fallback = tmp.path().join("ShiftDrop");
    let out = output_path_in(&fallback, &src, "jpg", "", &none());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out, fallback.join("photo.jpg"));
}

#[test]
fn cancel_stops_a_command_that_prints_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut job = Job::new("x", vec![tmp.path().join("in")], vec![tmp.path().join("out.txt")],
                           vec![shiftdrop::cmd!["sleep", "30"]]);
    let cancel = AtomicBool::new(false);
    let started = std::time::Instant::now();
    std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(200));
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        assert_eq!(run(&mut job, &|_| {}, &cancel), Err("Cancelled".into()));
    });
    assert!(started.elapsed().as_secs() < 2, "took {:?}", started.elapsed());
}

#[test]
fn formats_need_their_ffmpeg_encoder() {
    let no_xvid = |tool: &str| tool != "ffmpeg:libxvid";
    let offered = available_targets(&[Fmt::Mp4], no_xvid);
    assert!(!offered.contains(&Fmt::Avi));
    assert!(offered.contains(&Fmt::Mkv) && offered.contains(&Fmt::Mp3));
    let no_lame = |tool: &str| tool != "ffmpeg:libmp3lame";
    assert!(!available_targets(&[Fmt::Wav], no_lame).contains(&Fmt::Mp3));
}

#[test]
fn this_machines_ffmpeg_lists_its_encoders() {
    if !shiftdrop::convert::which("ffmpeg") {
        return;
    }
    assert!(shiftdrop::convert::which("ffmpeg:aac"));
    assert!(!shiftdrop::convert::which("ffmpeg:no-such-encoder"));
}
