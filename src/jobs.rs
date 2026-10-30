//! Run a Job's steps, report progress, clean up on failure.
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::convert::{Job, Step, plan_conversion};
use crate::formats::{Category, Fmt, detect, name};
use crate::media::{gpu_h264, output_with_timeout, probe};

// Images and documents run this many at a time: measured 2.5-2.8x faster for 16
// screenshots on 8 cores, and 8 at a time adds little. Video and audio run one
// at a time, since ffmpeg and the GPU already use the whole machine.
const PARALLEL: usize = 4;

// how often a running command checks for Cancel
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// One file per running job, listing the outputs it is writing. A job killed
/// mid-write (pkill, logout, out of memory) leaves its file here, and
/// sweep_leftovers() removes the half-written outputs on the next start.
pub fn pending_dir() -> PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"));
    cache.join("shiftdrop").join("pending")
}

pub fn pdf_pages(path: &Path) -> u32 {
    let mut cmd = Command::new("pdfinfo");
    cmd.arg(path);
    let Some(out) = output_with_timeout(cmd, Duration::from_secs(20)) else { return 1 };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("Pages:"))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(1)
}

/// ffmpeg `-progress` line -> fraction done, or None if the line isn't a time.
pub fn parse_progress(line: &str, duration: Option<f64>) -> Option<f64> {
    let duration = duration.filter(|d| *d != 0.0)?;
    // "out_time_us=N/A" before the first frame doesn't parse
    let us: i64 = line.strip_prefix("out_time_us=")?.parse().ok()?;
    Some((us as f64 / 1_000_000.0 / duration).clamp(0.0, 1.0))
}

/// One job per dropped file, with names reserved across the whole batch.
pub fn plan_convert(paths: &[PathBuf], target: Fmt) -> Result<Vec<Job>, String> {
    let mut taken = HashSet::new();
    let mut jobs = vec![];
    for path in paths {
        let source = detect(path).ok_or_else(|| format!("{}: unknown file type", name(path)))?;
        let media = matches!(source.category(), Category::Video | Category::Audio);
        let streams = media.then(|| probe(path));
        let gpu = media && matches!(target, Fmt::Mp4 | Fmt::Mov | Fmt::Mkv) && gpu_h264();
        let pages = if source == Fmt::Pdf { pdf_pages(path) } else { 1 };
        let job = plan_conversion(path, source, target, streams.as_ref(), gpu, pages, &taken)
            .map_err(|e| format!("{}: {e}", name(path)))?;
        taken.extend(job.outputs.iter().map(|p| name(p)));
        jobs.push(job);
    }
    Ok(jobs)
}

/// Run every step in order. If one fails and the job has a fallback (GPU -> CPU),
/// start over with that. On final failure, remove what this job wrote and return the error.
pub fn run(job: &mut Job, on_progress: &dyn Fn(f64), cancel: &AtomicBool) -> Result<Vec<PathBuf>, String> {
    run_with_pending(&pending_dir(), job, on_progress, cancel)
}

/// `run` with the folder of pending records given, for tests.
pub fn run_with_pending(pending: &Path, job: &mut Job, on_progress: &dyn Fn(f64),
                        cancel: &AtomicBool) -> Result<Vec<PathBuf>, String> {
    let record = mark_pending(pending, job);
    let result = run_all_steps(job, on_progress, cancel);
    if result.is_err() {
        remove_outputs(job);
    }
    for tmp in &job.cleanup {
        let _ = fs::remove_dir_all(tmp);
    }
    if let Ok(record) = record {
        let _ = fs::remove_file(record);
    }
    result
}

fn run_all_steps(job: &mut Job, on_progress: &dyn Fn(f64), cancel: &AtomicBool) -> Result<Vec<PathBuf>, String> {
    if let Err(e) = run_steps(&job.steps, job.duration, on_progress, cancel) {
        if job.fallback.is_none() || cancel.load(Ordering::Relaxed) {
            return Err(e);
        }
        remove_outputs(job);
        job.used_fallback = true;
        run_steps(job.fallback.as_deref().unwrap_or_default(), job.duration, on_progress, cancel)?;
    }
    if let Some(missing) = job.outputs.iter().find(|p| !p.exists()) {
        return Err(format!("{} was not created", name(missing)));
    }
    Ok(job.outputs.clone())
}

/// Remove outputs of jobs whose process died mid-write. Returns what was removed.
pub fn sweep_leftovers() -> Vec<PathBuf> {
    sweep_leftovers_in(&pending_dir())
}

/// `sweep_leftovers` in a given folder of pending records, for tests.
pub fn sweep_leftovers_in(pending: &Path) -> Vec<PathBuf> {
    let mut removed = vec![];
    let Ok(entries) = fs::read_dir(pending) else { return removed };
    for record in entries.flatten().map(|e| e.path()) {
        if record.extension().is_none_or(|e| e != "txt") {
            continue;
        }
        let pid = name(&record).split('-').next().and_then(|p| p.parse().ok());
        if pid.is_none_or(alive) {
            continue;
        }
        for line in fs::read_to_string(&record).unwrap_or_default().lines() {
            let out = PathBuf::from(line);
            if out.is_dir() {
                let _ = fs::remove_dir_all(&out);
            } else if !out.exists() || fs::remove_file(&out).is_err() {
                continue;
            }
            removed.push(out);
        }
        let _ = fs::remove_file(&record);
    }
    removed
}

fn mark_pending(pending: &Path, job: &Job) -> std::io::Result<PathBuf> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    fs::create_dir_all(pending)?;
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let record = pending.join(format!("{}-{nanos:x}{n}.txt", std::process::id()));
    let lines: String = job.outputs.iter().map(|out| format!("{}\n", out.display())).collect();
    fs::write(&record, lines)?;
    Ok(record)
}

fn alive(pid: i32) -> bool {
    // signal 0 only checks; EPERM means it exists but belongs to someone else
    unsafe { libc::kill(pid, 0) == 0 || *libc::__errno_location() != libc::ESRCH }
}

/// Run all jobs; one failing doesn't stop the others.
///
/// on_progress(done, fraction of the whole batch); on_job(job, error or None)
/// after each job. Returns (outputs in job order, "name: error" lines).
pub fn run_batch(jobs: Vec<Job>, on_progress: &(dyn Fn(usize, f64) + Sync),
                 on_job: &(dyn Fn(&Job, Option<&str>) + Sync), cancel: &AtomicBool) -> (Vec<PathBuf>, Vec<String>) {
    let total = jobs.len();
    let media = jobs.iter().any(|job| job.duration.is_some_and(|d| d != 0.0));
    let cores = thread::available_parallelism().map_or(1, |n| n.get());
    let workers = if media { 1 } else { PARALLEL.min(cores) };
    let jobs: Vec<Mutex<Job>> = jobs.into_iter().map(Mutex::new).collect();
    let results: Vec<Mutex<Result<Vec<PathBuf>, String>>> = (0..total).map(|_| Mutex::new(Ok(vec![]))).collect();
    let next = AtomicUsize::new(0);
    let finished = Mutex::new(0);

    let one = |i: usize| {
        let mut job = jobs[i].lock().unwrap();
        // one at a time: show progress inside the running job. In parallel the
        // count of finished jobs says more than any single job's progress.
        let within = |f: f64| {
            if workers == 1 {
                on_progress(i, (i as f64 + f) / total as f64);
            }
        };
        let result = run(&mut job, &within, cancel);
        on_job(&job, result.as_ref().err().map(String::as_str));
        *results[i].lock().unwrap() = result.map_err(|e| format!("{}: {e}", name(&job.sources[0])));
        let mut done = finished.lock().unwrap();
        *done += 1;
        on_progress(*done, *done as f64 / total as f64);
    };

    thread::scope(|s| {
        for _ in 0..workers.min(total) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= total {
                        break;
                    }
                    one(i);
                }
            });
        }
    });

    let mut outputs = vec![];
    let mut errors = vec![];
    for result in results {
        match result.into_inner().unwrap() {
            Ok(paths) => outputs.extend(paths),
            Err(e) => errors.push(e),
        }
    }
    (outputs, errors)
}

fn run_steps(steps: &[Step], duration: Option<f64>, on_progress: &dyn Fn(f64), cancel: &AtomicBool) -> Result<(), String> {
    for (i, step) in steps.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }
        match step {
            Step::Func(f) => f()?,
            Step::Cmd(cmd) => {
                run_command(cmd, duration, on_progress, cancel)?;
                let has_duration = duration.is_some_and(|d| d != 0.0);
                on_progress(if has_duration { 1.0 } else { (i + 1) as f64 / steps.len() as f64 });
            }
        }
    }
    Ok(())
}

fn remove_outputs(job: &Job) {
    for out in &job.outputs {
        // only ever paths this job reserved as new, so removing them is safe
        if out.is_dir() {
            let _ = fs::remove_dir_all(out);
        } else {
            let _ = fs::remove_file(out);
        }
    }
}

fn run_command(cmd: &[String], duration: Option<f64>, on_progress: &dyn Fn(f64), cancel: &AtomicBool) -> Result<(), String> {
    let spawned = Command::new(&cmd[0]).args(&cmd[1..])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(format!("{} is not installed", cmd[0])),
        Err(e) => return Err(e.to_string()),
    };
    // drain stderr on its own thread so a chatty tool can't block on a full pipe
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let (send, err_text) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        let _ = send.send(String::from_utf8_lossy(&buf).into_owned());
    });
    // stdout too: most tools print nothing, and Cancel must not wait for a line that never comes
    let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
    let (send, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in stdout.lines().map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let progress = |line: String| {
        if let Some(fraction) = parse_progress(line.trim(), duration) {
            on_progress(fraction);
        }
    };
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            break child.wait().map_err(|e| e.to_string())?;
        }
        let closed = match lines.recv_timeout(CANCEL_POLL) {
            Ok(line) => {
                progress(line);
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => true,
            Err(RecvTimeoutError::Timeout) => false,
        };
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if closed {
            thread::sleep(CANCEL_POLL); // stdout closed but still running: recv no longer waits
        }
    };
    lines.try_iter().for_each(progress);
    let err_text = err_text.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".into());
    }
    if !status.success() {
        let lines: Vec<&str> = err_text.trim().lines().collect();
        let tail = lines[lines.len().saturating_sub(3)..].join(" ");
        let why = if tail.is_empty() {
            format!("exit code {}", status.code().map_or("?".into(), |c| c.to_string()))
        } else {
            tail
        };
        return Err(format!("{} failed: {why}", cmd[0]));
    }
    Ok(())
}
