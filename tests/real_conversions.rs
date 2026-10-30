//! Actually run conversions on tiny files made on the spot. Skips a case when its tool is missing.
//! Unix for now: the helpers call Unix tool names and commands (`true`, `find`).
#![cfg(unix)]
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use shiftdrop::convert::{Job, available_targets, which};
use shiftdrop::formats::{Fmt, detect};
use shiftdrop::jobs::{plan_convert, run, run_batch, run_with_pending, sweep_leftovers_in};
use shiftdrop::tools::{Params, plan_tool};

fn sh(args: &[&str]) {
    let ok = Command::new(args[0]).args(&args[1..]).status().is_ok_and(|s| s.success());
    assert!(ok, "{args:?} failed");
}

fn out(args: &[&str]) -> String {
    let done = Command::new(args[0]).args(&args[1..]).output().unwrap();
    String::from_utf8_lossy(&done.stdout).into_owned()
}

fn make(tmp: &Path, kind: &str) -> PathBuf {
    let q = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y"];
    let path = |name: &str| tmp.join(name);
    let s = |p: &Path| p.to_string_lossy().into_owned();
    match kind {
        "png" => {
            let p = path("pic.png");
            sh(&["magick", "-size", "48x32", "gradient:red-blue", &s(&p)]);
            p
        }
        "mp4" => {
            let p = path("clip.mp4");
            sh(&[&q[..], &["-f", "lavfi", "-i", "testsrc=size=64x48:rate=10:duration=1",
                           "-f", "lavfi", "-i", "sine=duration=1", "-shortest", "-c:v", "libx264",
                           "-pix_fmt", "yuv420p", "-c:a", "aac", &s(&p)]].concat());
            p
        }
        "wav" => {
            let p = path("tone.wav");
            sh(&[&q[..], &["-f", "lavfi", "-i", "sine=duration=1", &s(&p)]].concat());
            p
        }
        "pdf" => {
            let p = path("doc.pdf");
            sh(&["magick", "-size", "60x80", "xc:white", "xc:gray", &s(&p)]);
            p
        }
        "mp4-big" => {
            // lossless, so compress has something to remove
            let p = path("big.mp4");
            sh(&[&q[..], &["-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=2",
                           "-c:v", "libx264", "-crf", "0", "-pix_fmt", "yuv420p", &s(&p)]].concat());
            p
        }
        "pdf-big" => {
            // uncompressed image inside
            let p = path("big.pdf");
            sh(&["magick", "-size", "600x600", "plasma:", "-compress", "None", &s(&p)]);
            p
        }
        "txt" => {
            let p = path("notes.md");
            fs::write(&p, "# Hello\n\nplain text\n").unwrap();
            p
        }
        "srt" => {
            let p = path("subs.srt");
            fs::write(&p, "1\n00:00:00,000 --> 00:00:01,000\nHello\n").unwrap();
            p
        }
        _ => panic!("{kind}"),
    }
}

fn no() -> AtomicBool {
    AtomicBool::new(false)
}

fn run_one(job: &mut Job) -> Result<PathBuf, String> {
    let outputs = run(job, &|_| {}, &no())?;
    assert_eq!(outputs.len(), 1);
    Ok(outputs[0].clone())
}

fn convert_one(src: &Path, target: Fmt) -> Job {
    plan_convert(&[src.to_path_buf()], target).unwrap().remove(0)
}

fn tool_one(tool: &str, src: &Path, params: &Params) -> Job {
    let fmt = detect(src).unwrap();
    plan_tool(tool, &[(src.to_path_buf(), fmt)], params, &HashMap::new(), false).unwrap().remove(0)
}

#[test]
fn convert_every_offered_target() {
    let cases = [("png", Fmt::Png), ("mp4", Fmt::Mp4), ("wav", Fmt::Wav), ("pdf", Fmt::Pdf), ("txt", Fmt::Txt),
                 ("srt", Fmt::Srt)];
    let mut failures = vec![];
    for (kind, source) in cases {
        for target in available_targets(&[source], which) {
            let tmp = tempfile::tempdir().unwrap();
            let src = make(tmp.path(), kind);
            let mut job = convert_one(&src, target);
            let result = run_one(&mut job);
            let ok = match &result {
                Ok(out) if out.is_dir() => fs::read_dir(out).unwrap().count() == 2, // one image per PDF page
                Ok(out) => fs::metadata(out).unwrap().len() > 0 && detect(out) == Some(target),
                Err(_) => false,
            };
            if !ok || !src.exists() {
                failures.push(format!("{kind} -> {}: {result:?}", target.value()));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn video_progress_reaches_the_end() {
    let tmp = tempfile::tempdir().unwrap();
    let mut job = convert_one(&make(tmp.path(), "mp4"), Fmt::Mkv);
    let seen = Mutex::new(vec![]);
    run(&mut job, &|f| seen.lock().unwrap().push(f), &no()).unwrap();
    assert_eq!(seen.lock().unwrap().last(), Some(&1.0));
}

#[test]
fn batch_gets_separate_outputs() {
    let tmp = tempfile::tempdir().unwrap();
    let a = make(tmp.path(), "png");
    let b = tmp.path().join("pic2.png");
    fs::copy(&a, &b).unwrap();
    let mut names: Vec<String> = plan_convert(&[a, b], Fmt::Jpg).unwrap().iter_mut()
        .map(|j| run_one(j).unwrap().file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["pic.jpg", "pic2.jpg"]);
}

#[test]
fn failed_job_leaves_no_partial_file() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = tmp.path().join("broken.mp4");
    fs::write(&bad, "not a video").unwrap();
    let mut job = convert_one(&bad, Fmt::Webm);
    assert!(run_one(&mut job).is_err());
    assert!(!job.outputs[0].exists());
}

#[test]
fn every_tool() {
    let edge = |px| Params { long_edge: px, ..Default::default() };
    let range = |start, end| Params { start, end, ..Default::default() };
    let cases = [
        ("compress", "png", Params::default()), ("compress", "mp4-big", Params::default()),
        ("compress", "wav", Params::default()), ("compress", "pdf-big", Params::default()),
        ("strip", "png", Params::default()), ("strip", "mp4", Params::default()),
        ("resize", "png", edge(16)), ("rotate", "png", Params::default()),
        ("trim", "mp4", range(0.2, 0.7)), ("trim", "wav", range(0.0, 0.5)),
        ("normalize", "wav", Params::default()), ("make_pdf", "png", Params::default()),
    ];
    let mut failures = vec![];
    for (tool, kind, params) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let src = make(tmp.path(), kind);
        let result = run_one(&mut tool_one(tool, &src, &params));
        let ok = result.as_ref().is_ok_and(|out| out != &src && fs::metadata(out).is_ok_and(|m| m.len() > 0));
        if !ok {
            failures.push(format!("{tool} {kind}: {result:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn resize_really_resizes() {
    let tmp = tempfile::tempdir().unwrap();
    let src = make(tmp.path(), "png");
    let result = run_one(&mut tool_one("resize", &src, &Params { long_edge: 24, ..Default::default() })).unwrap();
    assert_eq!(out(&["magick", "identify", "-format", "%wx%h", &result.to_string_lossy()]), "24x16");
}

#[test]
fn merge_pdfs() {
    let tmp = tempfile::tempdir().unwrap();
    let a = make(tmp.path(), "pdf");
    let b = tmp.path().join("doc2.pdf");
    fs::copy(&a, &b).unwrap();
    let mut job = plan_tool("merge_pdf", &[(a, Fmt::Pdf), (b, Fmt::Pdf)], &Params::default(), &HashMap::new(), false)
        .unwrap().remove(0);
    let merged = run_one(&mut job).unwrap();
    assert!(out(&["pdfinfo", &merged.to_string_lossy()]).contains("Pages:           4"));
}

#[test]
fn compress_refuses_to_grow_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let src = make(tmp.path(), "pdf"); // a few hundred bytes already
    let mut job = tool_one("compress", &src, &Params::default());
    let err = run_one(&mut job).unwrap_err();
    assert!(err.contains("already as small"), "{err}");
    assert!(!job.outputs[0].exists());
}

#[test]
fn strip_keeps_colour_profile_drops_the_rest() {
    let icc = out(&["find", "/usr/share/color/icc", "-name", "*.icc"]);
    let Some(icc) = icc.lines().next() else {
        eprintln!("skipped: no ICC profile on this machine");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("photo.jpg");
    sh(&["magick", "-size", "40x20", "gradient:red-blue", "-profile", icc, "-set", "comment", "secret",
         &src.to_string_lossy()]);
    let cleaned = run_one(&mut tool_one("strip", &src, &Params::default())).unwrap();
    let info = out(&["magick", "identify", "-format", "%c|%[profile:icc]", &cleaned.to_string_lossy()]);
    let (comment, colour) = info.split_once('|').unwrap();
    assert!(comment.is_empty() && !colour.is_empty()); // comment gone, colour profile kept
}

#[test]
fn batch_runs_images_in_parallel_and_keeps_going() {
    let tmp = tempfile::tempdir().unwrap();
    let mut files = vec![make(tmp.path(), "png")];
    for i in 2..6 {
        let copy = tmp.path().join(format!("pic{i}.png"));
        fs::copy(&files[0], &copy).unwrap();
        files.push(copy);
    }
    let bad = tmp.path().join("broken.png");
    fs::write(&bad, "not an image").unwrap();
    files.push(bad);
    let jobs = plan_convert(&files, Fmt::Jpg).unwrap();
    let seen = Mutex::new(vec![]);
    let (outputs, errors) = run_batch(jobs, &|_, f| seen.lock().unwrap().push(f), &|_, _| {}, &no());
    assert_eq!((outputs.len(), errors.len()), (5, 1));
    assert!(errors[0].starts_with("broken.png"));
    assert_eq!(seen.lock().unwrap().last(), Some(&1.0));
}

fn video_tags(path: &Path) -> String {
    out(&["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
          "stream=width,height,color_transfer:stream_side_data=rotation", "-of", "compact=p=0:nk=1",
          &path.to_string_lossy()]).replace('\n', "|")
}

#[test]
fn rotated_phone_video_stays_upright() {
    let tmp = tempfile::tempdir().unwrap();
    let (plain, rotated) = (tmp.path().join("plain.mp4"), tmp.path().join("rot.mp4"));
    let q = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y"];
    sh(&[&q[..], &["-f", "lavfi", "-i", "testsrc=size=320x240:duration=0.5", "-c:v", "libx264",
                   "-pix_fmt", "yuv420p", &plain.to_string_lossy()]].concat());
    sh(&[&q[..], &["-display_rotation", "90", "-i", &plain.to_string_lossy(), "-c", "copy",
                   &rotated.to_string_lossy()]].concat());
    let copied = run_one(&mut convert_one(&rotated, Fmt::Mkv)).unwrap();
    assert!(video_tags(&copied).contains("90")); // copy keeps the "rotate me" flag
    let encoded = run_one(&mut convert_one(&rotated, Fmt::Avi)).unwrap();
    assert!(video_tags(&encoded).starts_with("240|320")); // re-encode turns the picture itself
}

#[test]
fn hdr_kept_on_copy_tone_mapped_on_h264() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("hdr.mkv");
    sh(&["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x240:duration=0.5",
         "-vf", "format=yuv420p10le,setparams=color_primaries=bt2020:color_trc=arib-std-b67:colorspace=bt2020nc",
         "-c:v", "libsvtav1", "-preset", "12", &src.to_string_lossy()]);
    let kept = run_one(&mut convert_one(&src, Fmt::Mp4)).unwrap();
    assert!(video_tags(&kept).contains("arib-std-b67"));
    let sdr = run_one(&mut convert_one(&src, Fmt::Mov)).unwrap();
    assert!(video_tags(&sdr).contains("bt709"));
}

#[test]
fn job_leaves_no_pending_record() {
    let tmp = tempfile::tempdir().unwrap();
    let pending = tmp.path().join("pending");
    let mut job = convert_one(&make(tmp.path(), "png"), Fmt::Jpg);
    run_with_pending(&pending, &mut job, &|_| {}, &no()).unwrap();
    assert_eq!(fs::read_dir(&pending).unwrap().count(), 0);
}

#[test]
fn sweep_removes_outputs_of_killed_jobs_only() {
    let tmp = tempfile::tempdir().unwrap();
    let pending = tmp.path().join("pending");
    fs::create_dir(&pending).unwrap();
    let mut dead = Command::new("true").spawn().unwrap();
    dead.wait().unwrap();
    let (half, running) = (tmp.path().join("clip.webm"), tmp.path().join("other.webm"));
    fs::write(&half, "half").unwrap();
    fs::write(&running, "still being written").unwrap();
    let never = tmp.path().join("never made.webm");
    fs::write(pending.join(format!("{}-a.txt", dead.id())), format!("{}\n{}\n", half.display(), never.display())).unwrap();
    let mine = format!("{}-b.txt", std::process::id());
    fs::write(pending.join(&mine), format!("{}\n", running.display())).unwrap();
    assert_eq!(sweep_leftovers_in(&pending), std::slice::from_ref(&half));
    assert!(!half.exists() && running.exists());
    let left: Vec<String> = fs::read_dir(&pending).unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, [mine]);
}

#[test]
fn cancelled_batch_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let jobs = plan_convert(&[make(tmp.path(), "mp4")], Fmt::Webm).unwrap();
    let planned = jobs[0].outputs[0].clone();
    let (outputs, errors) = run_batch(jobs, &|_, _| {}, &|_, _| {}, &AtomicBool::new(true));
    assert!(outputs.is_empty());
    assert_eq!(errors, ["clip.mp4: Cancelled"]);
    assert!(!planned.exists());
}

#[test]
fn strip_drops_location_and_data_tracks() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("phone.mov");
    // a timecode track stands in for the data tracks phones keep GPS in
    sh(&["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=64x48:duration=0.5",
         "-c:v", "libx264", "-timecode", "01:00:00:00", "-metadata", "location=+37.7749-122.4194/",
         &src.to_string_lossy()]);
    let streams = |p: &Path| out(&["ffprobe", "-v", "error", "-show_entries", "stream=codec_type:format_tags",
                                   "-of", "compact", &p.to_string_lossy()]);
    assert!(streams(&src).contains("data") && streams(&src).contains("location"));
    let cleaned = run_one(&mut tool_one("strip", &src, &Params::default())).unwrap();
    assert!(!streams(&cleaned).contains("data") && !streams(&cleaned).contains("location"));
}
