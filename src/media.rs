//! What's inside a media file (one ffprobe call), and whether the GPU encoder works.
use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

pub const RENDER_NODE: &str = "/dev/dri/renderD128";
const HDR_TRANSFERS: [&str; 2] = ["smpte2084", "arib-std-b67"]; // PQ (HDR10) and HLG

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Streams {
    pub duration: Option<f64>,
    pub video: Option<String>,      // codec name of the main picture, e.g. "h264"
    pub video_index: Option<i64>,   // its stream index (skips cover art)
    pub audio: Option<String>,      // codec of the first audio track
    pub transfer: Option<String>,   // color transfer, for spotting HDR
}

impl Streams {
    pub fn hdr(&self) -> bool {
        self.transfer.as_deref().is_some_and(|t| HDR_TRANSFERS.contains(&t))
    }
}

/// `Streams::default()` when ffprobe can't read the file.
pub fn probe(path: &Path) -> Streams {
    let mut cmd = Command::new("ffprobe");
    cmd.args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"]).arg(path);
    let Some(out) = output_with_timeout(cmd, Duration::from_secs(20)) else { return Streams::default() };
    match serde_json::from_slice(&out.stdout) {
        Ok(info) => parse(&info),
        Err(_) => Streams::default(),
    }
}

/// ffprobe JSON -> Streams. Separate from probe() so tests can feed it JSON.
pub fn parse(info: &Value) -> Streams {
    let streams = info["streams"].as_array().map(Vec::as_slice).unwrap_or_default();
    let main = streams
        .iter()
        .find(|s| s["codec_type"] == "video" && !is_set(&s["disposition"]["attached_pic"]));
    let sound = streams.iter().find(|s| s["codec_type"] == "audio");
    let text = |v: &Value| v.as_str().map(str::to_owned);
    let duration = match &info["format"]["duration"] {
        Value::String(s) => s.parse().ok(),
        v => v.as_f64(),
    };
    Streams {
        duration,
        video: main.and_then(|s| text(&s["codec_name"])),
        video_index: main.and_then(|s| s["index"].as_i64()),
        audio: sound.and_then(|s| text(&s["codec_name"])),
        transfer: main.and_then(|s| text(&s["color_transfer"])),
    }
}

fn is_set(flag: &Value) -> bool {
    flag.as_i64().is_some_and(|n| n != 0) || flag.as_bool() == Some(true)
}

/// True when VAAPI H.264 encoding actually works here. Checked once per run with a tiny encode.
pub fn gpu_h264() -> bool {
    static WORKS: OnceLock<bool> = OnceLock::new();
    *WORKS.get_or_init(|| {
        if !Path::new(RENDER_NODE).exists() {
            return false;
        }
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-vaapi_device", RENDER_NODE,
                  "-f", "lavfi", "-i", "color=black:size=64x64:duration=0.1",
                  "-vf", "format=nv12,hwupload", "-c:v", "h264_vaapi", "-f", "null", "-"]);
        output_with_timeout(cmd, Duration::from_secs(15)).is_some_and(|out| out.status.success())
    })
}

/// Run a short command and collect its output; None if it can't start or takes too long.
pub fn output_with_timeout(mut cmd: Command, timeout: Duration) -> Option<Output> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    Some(Output { status, stdout: stdout.join().ok()?, stderr: stderr.join().ok()? })
}
