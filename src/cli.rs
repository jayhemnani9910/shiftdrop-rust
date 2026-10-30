//! The command line, shared by `shiftdrop` on Linux and `shiftdrop-cli.exe` on Windows:
//!
//! shiftdrop convert --to FMT FILE...
//! shiftdrop tool NAME FILE... [--start S --end E] [--edge PX]
//! shiftdrop targets FILE...          what this machine can turn FILE into

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use clap::{Parser, Subcommand};

use crate::convert::{Job, available_targets, which};
use crate::formats::{Category, Fmt, detect, name};
use crate::jobs::{plan_convert, run_batch};
use crate::media::{gpu_h264, probe};
use crate::tools::{Params, TOOLS, available_tools, plan_tool};

#[derive(Parser)]
#[command(name = "shiftdrop", about = "run the app (the extension starts it on demand)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Convert {
        /// target format, e.g. jpg
        #[arg(long, value_parser = parse_fmt)]
        to: Fmt,
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    Tool {
        #[arg(value_parser = TOOLS.iter().map(|t| t.id).collect::<Vec<_>>())]
        name: String,
        #[arg(required = true)]
        files: Vec<PathBuf>,
        #[arg(long)]
        start: Option<f64>,
        #[arg(long)]
        end: Option<f64>,
        #[arg(long, default_value_t = 1920)]
        edge: u32,
    },
    Targets {
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
}

fn parse_fmt(value: &str) -> Result<Fmt, String> {
    Fmt::from_value(value).ok_or_else(|| format!("{value:?} is not a format"))
}

fn formats(paths: &[PathBuf]) -> Result<Vec<Fmt>, String> {
    paths.iter().map(|p| detect(p).ok_or_else(|| format!("{}: unknown file type", name(p)))).collect()
}

fn run_all(jobs: Vec<Job>) -> ExitCode {
    let total = jobs.len();
    let finished = |job: &Job, error: Option<&str>| match error {
        Some(e) => eprintln!("{}: {e}", name(&job.sources[0])),
        None => job.outputs.iter().for_each(|out| println!("{} -> {}", name(&job.sources[0]), out.display())),
    };
    let progress = |done: usize, fraction: f64| {
        let end = if fraction >= 1.0 { "\n" } else { "" };
        print!("\r{}/{total} {:>3.0}%{end}", (done + 1).min(total), fraction * 100.0);
        let _ = std::io::stdout().flush();
    };
    let (_outputs, errors) = run_batch(jobs, &progress, &finished, &AtomicBool::new(false));
    if errors.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn cli(args: Cli) -> Result<ExitCode, String> {
    match args.cmd {
        Cmd::Targets { files } => {
            let formats = formats(&files)?;
            let list = |names: Vec<&str>| if names.is_empty() { "-".to_string() } else { names.join(" ") };
            println!("formats: {}", list(available_targets(&formats, which).iter().map(|f| f.value()).collect()));
            println!("tools:   {}", list(available_tools(&formats, which).iter().map(|t| t.id).collect()));
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Convert { to, files } => {
            let formats = formats(&files)?;
            if !available_targets(&formats, which).contains(&to) {
                let mut from: Vec<&str> = formats.iter().map(|f| f.value()).collect();
                from.sort();
                from.dedup();
                return Err(format!("can't convert {} to {} on this machine", from.join(", "), to.value()));
            }
            Ok(run_all(plan_convert(&files, to)?))
        }
        Cmd::Tool { name: tool, files, start, end, edge } => {
            let formats = formats(&files)?;
            if !available_tools(&formats, which).iter().any(|t| t.id == tool) {
                return Err(format!("{tool} doesn't apply to these files"));
            }
            let pairs: Vec<(PathBuf, Fmt)> = files.iter().cloned().zip(formats.iter().copied()).collect();
            let media: HashMap<PathBuf, _> = pairs.iter()
                .filter(|(_, f)| matches!(f.category(), Category::Video | Category::Audio))
                .map(|(p, _)| (p.clone(), probe(p)))
                .collect();
            let mut params = Params { long_edge: edge, ..Default::default() };
            if tool == "trim" {
                let length = media.get(&files[0]).and_then(|s| s.duration);
                params.start = start.unwrap_or(0.0);
                params.end = end.or(length).ok_or_else(|| format!("{}: can't read its length", name(&files[0])))?;
            }
            let gpu = tool == "trim" && formats[0].category() == Category::Video && gpu_h264();
            Ok(run_all(plan_tool(&tool, &pairs, &params, &media, gpu)?))
        }
    }
}

/// Whether these arguments are a command, rather than files for the app.
pub fn wanted(argv: &[String]) -> bool {
    argv.get(1).is_some_and(|a| ["convert", "tool", "targets", "-h", "--help"].contains(&a.as_str()))
}

pub fn main() -> ExitCode {
    cli(Cli::parse()).unwrap_or_else(|e| {
        eprintln!("{e}");
        ExitCode::FAILURE
    })
}
