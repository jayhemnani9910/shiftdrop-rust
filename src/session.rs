//! What the wheel offers and what happens after a pick, apart from any window
//! toolkit: the GTK app on Linux and the Win32 app on Windows both drive this.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::convert::{Job, available_targets, which};
use crate::formats::{Category, Fmt, detect, fallback_dir, name};
use crate::jobs::{plan_convert, run_batch};
use crate::media::{gpu_h264, probe};
use crate::tools::{Params, Tool, available_tools, plan_tool};

/// Longest-side choices the resize dialog offers.
pub const RESIZE_EDGES: [u32; 7] = [3840, 2560, 1920, 1280, 1080, 720, 512];
pub const BUSY: &str = "Still working on the last batch. Try again when it's done.";
pub const MOVE_ONLY: &str = "That drag only allowed moving the file, so nothing was done.";

pub fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("{n} {word}") } else { format!("{n} {word}s") }
}

pub fn unreadable_line(names: &[String]) -> String {
    let more = if names.len() > 3 { " …" } else { "" };
    format!("Skipped {}{more}: not a file on this computer (phone, network or trash). Copy it to a folder first.",
            names[..names.len().min(3)].join(", "))
}

/// What a wheel segment does.
#[derive(Clone, Copy)]
pub enum Choice {
    Convert(Fmt),
    Tool(&'static Tool),
}

impl Choice {
    pub fn title(self) -> String {
        match self {
            Choice::Convert(fmt) => format!("Converting to {}", fmt.label()),
            Choice::Tool(tool) => tool.title.into(),
        }
    }
}

/// Ring of formats (or tools). Pick one by dropping files on it, or by clicking.
#[derive(Default)]
pub struct Wheel {
    pub files: Vec<PathBuf>,
    pub unreadable: Vec<String>, // dropped, but not on this computer's disk
    pub tools: bool,
    pub items: Vec<(String, Choice)>,
    pub hover: Option<usize>,
    pub picking: bool, // opened with a known selection: click instead of drop
}

impl Wheel {
    pub fn new(files: Vec<PathBuf>, tools: bool, picking: bool, unreadable: Vec<String>) -> Wheel {
        let mut wheel = Wheel { files, unreadable, tools, picking, ..Default::default() };
        wheel.refresh();
        wheel
    }

    /// Recompute the segments from the files and the mode.
    pub fn refresh(&mut self) {
        let formats: Option<Vec<Fmt>> = self.files.iter().map(detect).collect();
        self.items = match formats {
            Some(formats) if !formats.is_empty() && self.tools => available_tools(&formats, which)
                .into_iter().map(|t| (t.label.to_string(), Choice::Tool(t))).collect(),
            Some(formats) if !formats.is_empty() => available_targets(&formats, which)
                .into_iter().map(|f| (f.label(), Choice::Convert(f))).collect(),
            _ => vec![],
        };
    }

    pub fn set_tools(&mut self, tools: bool) {
        self.tools = tools;
        self.hover = None;
        self.refresh();
    }

    pub fn labels(&self) -> Vec<String> {
        self.items.iter().map(|(label, _)| label.clone()).collect()
    }

    /// The centre disc: big word and the hint under it.
    pub fn center(&self) -> (String, String) {
        let mode = if self.tools { "TOOLS" } else { "CONVERT" };
        if let Some(i) = self.hover {
            return (self.items[i].0.clone(), plural(self.files.len(), "file"));
        }
        let (center, subtitle) = if self.files.is_empty() && !self.unreadable.is_empty() {
            ("NOTHING", "not a local file")
        } else if self.files.is_empty() {
            (mode, "move onto the wheel")
        } else if self.items.is_empty() {
            ("NOTHING", "can't handle this file")
        } else if self.picking {
            (mode, "right-click: switch")
        } else if !self.tools {
            (mode, "Ctrl: tools")
        } else {
            (mode, "release Ctrl: formats")
        };
        (center.into(), subtitle.into())
    }
}

/// What the pop-up says when a batch ends.
pub struct Summary {
    pub title: String,
    pub detail: String,
    pub failed: bool,
}

pub fn summary(outputs: &[PathBuf], errors: &[String]) -> Summary {
    if !errors.is_empty() {
        let done = if outputs.is_empty() { String::new() } else { format!("{} done, ", plural(outputs.len(), "file")) };
        return Summary {
            title: format!("{done}{}", plural(errors.len(), "problem")),
            detail: errors[..errors.len().min(4)].join("\n"),
            failed: true,
        };
    }
    let fallback = fallback_dir();
    let where_ = if outputs.iter().any(|p| p.parent() == Some(fallback.as_path())) {
        format!(" (in {}: the original's folder is read-only)", shown(&fallback))
    } else {
        String::new()
    };
    let names: Vec<String> = outputs.iter().take(3).map(|p| name(p)).collect();
    let more = if outputs.len() > 3 { " …" } else { "" };
    Summary {
        title: format!("✓ {} ready", plural(outputs.len(), "file")),
        detail: format!("{}{more}{where_}", names.join(", ")),
        failed: false,
    }
}

/// `~/ShiftDrop` on Linux; Windows users know their folders by full path.
fn shown(folder: &Path) -> String {
    if cfg!(unix) { format!("~/{}", name(folder)) } else { folder.display().to_string() }
}

/// Plan and run a batch; call from a work thread. `progress(done, total, fraction)`
/// as it goes, `log` one line per finished job. Returns (outputs, error lines).
pub fn work(files: &[PathBuf], choice: Choice, params: &Params, cancel: &AtomicBool, unreadable: &[String],
            progress: &(dyn Fn(usize, usize, f64) + Sync), log: &(dyn Fn(&str) + Sync)) -> (Vec<PathBuf>, Vec<String>) {
    let planned = match choice {
        Choice::Convert(fmt) => plan_convert(files, fmt),
        Choice::Tool(tool) => plan_tool_for(tool, files, params),
    };
    let jobs = match planned {
        Ok(jobs) => jobs,
        Err(e) => return (vec![], vec![e]),
    };
    let total = jobs.len();
    let on_progress = |done: usize, fraction: f64| progress(done.min(total.saturating_sub(1)), total, fraction);
    let finished = |job: &Job, error: Option<&str>| {
        let how = if job.used_fallback { " (GPU failed, used CPU)" } else { "" };
        let result = match error {
            Some(e) => format!("failed: {e}"),
            None => format!("ok{how} -> {:?}", job.outputs.iter().map(|p| name(p)).collect::<Vec<_>>()),
        };
        log(&format!("{} {}: {result}", job.title, name(&job.sources[0])));
    };

    let started = Instant::now();
    let (outputs, mut errors) = run_batch(jobs, &on_progress, &finished, cancel);
    log(&format!("batch of {total} took {:.2}s", started.elapsed().as_secs_f64()));
    // the drop was a copy, so every original must still be there
    errors.extend(files.iter().filter(|f| !f.exists())
        .map(|f| format!("{}: the original is gone. Check the folder you dragged from.", name(f))));
    if cancel.load(Ordering::Relaxed) {
        errors.retain(|e| !e.ends_with(": Cancelled"));
        errors.push("Cancelled".into());
    }
    if !unreadable.is_empty() {
        errors.push(unreadable_line(unreadable));
    }
    (outputs, errors)
}

fn plan_tool_for(tool: &Tool, files: &[PathBuf], params: &Params) -> Result<Vec<Job>, String> {
    let pairs: Vec<(PathBuf, Fmt)> = files.iter()
        .map(|f| detect(f).map(|fmt| (f.clone(), fmt)).ok_or_else(|| format!("{}: unknown file type", name(f))))
        .collect::<Result<_, _>>()?;
    let is = |fmt: Fmt, category| fmt.category() == category;
    let media: HashMap<PathBuf, _> = pairs.iter()
        .filter(|(_, fmt)| is(*fmt, Category::Video) || is(*fmt, Category::Audio))
        .map(|(f, _)| (f.clone(), probe(f)))
        .collect();
    let gpu = tool.id == "trim" && pairs.iter().any(|(_, fmt)| is(*fmt, Category::Video)) && gpu_h264();
    plan_tool(tool.id, &pairs, params, &media, gpu)
}
