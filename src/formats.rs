//! What a file is, and what it can become.
//!
//! Each format is one row: its category, the extensions that mean it, and whether
//! we can write it. What a file converts into comes from a few rules about
//! categories, not from a per-format table.
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::sys;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Image,
    Audio,
    Video,
    Document,
    Subtitle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fmt {
    // order inside a category is the order on the wheel: most wanted first
    Jpg, Png, Webp, Avif, Heic, Tiff, Bmp, Svg,
    Mp4, Mov, Mkv, Webm, Avi, Wmv, Gif,
    Mp3, M4a, Wav, Flac, Ogg, Opus, Aiff, Wma,
    Pdf, Docx, Txt,
    Srt, Vtt,
}

use Category::{Audio as A, Document as D, Image as I, Subtitle as S, Video as V};

//                          category  extensions                        writable
const ROWS: &[(Fmt, Category, &[&str], bool)] = &[
    (Fmt::Jpg,  I, &["jpg", "jpeg", "jpe", "jfif"], true),
    (Fmt::Png,  I, &["png"],                        true),
    (Fmt::Webp, I, &["webp"],                       true),
    (Fmt::Avif, I, &["avif"],                       true),
    (Fmt::Heic, I, &["heic", "heif"],               true),
    (Fmt::Tiff, I, &["tiff", "tif"],                true),
    (Fmt::Bmp,  I, &["bmp"],                        true),
    (Fmt::Svg,  I, &["svg"],                        false), // vector in, never out
    (Fmt::Mp4,  V, &["mp4", "m4v"],                 true),
    (Fmt::Mov,  V, &["mov", "qt"],                  true),
    (Fmt::Mkv,  V, &["mkv"],                        true),
    (Fmt::Webm, V, &["webm"],                       true),
    (Fmt::Avi,  V, &["avi"],                        true),
    (Fmt::Wmv,  V, &["wmv"],                        true),
    (Fmt::Gif,  V, &["gif"],                        true),  // an animation, so it lives with video
    (Fmt::Mp3,  A, &["mp3"],                        true),
    (Fmt::M4a,  A, &["m4a", "aac", "m4b"],          true),
    (Fmt::Wav,  A, &["wav"],                        true),
    (Fmt::Flac, A, &["flac"],                       true),
    (Fmt::Ogg,  A, &["ogg", "oga"],                 true),
    (Fmt::Opus, A, &["opus"],                       true),
    (Fmt::Aiff, A, &["aiff", "aif"],                true),
    (Fmt::Wma,  A, &["wma"],                        true),
    (Fmt::Pdf,  D, &["pdf"],                        true),
    (Fmt::Docx, D, &["docx"],                       true),
    (Fmt::Txt,  D, &["txt", "text", "md", "log"],   true),
    (Fmt::Srt,  S, &["srt"],                        true),
    (Fmt::Vtt,  S, &["vtt"],                        true),
];

// audio pulled out of a video: the three people actually ask for
const FROM_VIDEO_AUDIO: [Fmt; 3] = [Fmt::Mp3, Fmt::M4a, Fmt::Wav];

impl Fmt {
    /// Every format, in wheel order.
    pub fn all() -> impl Iterator<Item = Fmt> {
        ROWS.iter().map(|row| row.0)
    }

    fn row(self) -> &'static (Fmt, Category, &'static [&'static str], bool) {
        ROWS.iter().find(|row| row.0 == self).expect("every format has a row")
    }

    /// "jpg": the extension outputs get, and the name on the command line.
    pub fn value(self) -> &'static str {
        self.row().2[0]
    }

    pub fn label(self) -> String {
        self.value().to_uppercase()
    }

    pub fn category(self) -> Category {
        self.row().1
    }

    pub fn writable(self) -> bool {
        self.row().3
    }

    pub fn from_value(value: &str) -> Option<Fmt> {
        Fmt::all().find(|f| f.value() == value)
    }
}

pub fn detect(path: impl AsRef<Path>) -> Option<Fmt> {
    let ext = path.as_ref().extension()?.to_str()?.to_lowercase();
    ROWS.iter().find(|row| row.2.contains(&ext.as_str())).map(|row| row.0)
}

fn writable(category: Category) -> Vec<Fmt> {
    Fmt::all().filter(|f| f.category() == category && f.writable()).collect()
}

/// Everything `source` can become, before checking which programs are installed.
///
/// Rules: stay inside the category, plus a few crossings that are worth a slot
/// on the wheel (image -> PDF, video -> audio, PDF -> pages and text).
pub fn targets(source: Fmt) -> Vec<Fmt> {
    let found: Vec<Fmt> = match source.category() {
        I => [writable(I), vec![Fmt::Pdf]].concat(),
        V if source == Fmt::Gif => writable(V),
        V => [writable(V), FROM_VIDEO_AUDIO.to_vec()].concat(),
        A => writable(A),
        S => [writable(S), vec![Fmt::Txt]].concat(),
        D if source == Fmt::Pdf => vec![Fmt::Png, Fmt::Jpg, Fmt::Txt, Fmt::Docx],
        D if source == Fmt::Txt => vec![Fmt::Pdf, Fmt::Docx],
        D => vec![], // DOCX in: not supported
    };
    found.into_iter().filter(|&f| f != source).collect()
}

/// Targets every file in a selection shares, in the first file's order.
pub fn common_targets(sources: &[Fmt]) -> Vec<Fmt> {
    let Some(first) = sources.first() else { return vec![] };
    targets(*first)
        .into_iter()
        .filter(|f| sources.iter().all(|s| targets(*s).contains(f)))
        .collect()
}

/// Where outputs go when the source's folder can't be written (a mounted archive, a read-only share).
pub fn fallback_dir() -> PathBuf {
    sys::home().join("ShiftDrop")
}

/// Next to the source (or in `fallback_dir()` if that folder is read-only), never overwriting.
///
/// `photo.png` -> `photo.jpg`, then `photo 2.jpg`. With a suffix:
/// `photo compressed.png`, then `photo compressed 2.png`.
pub fn output_path(source: &Path, ext: &str, suffix: &str, taken: &HashSet<String>) -> PathBuf {
    output_path_in(&fallback_dir(), source, ext, suffix, taken)
}

/// `output_path` with the read-only fallback folder given, for tests.
pub fn output_path_in(fallback: &Path, source: &Path, ext: &str, suffix: &str, taken: &HashSet<String>) -> PathBuf {
    // a bare `photo.png` has an empty parent, so outputs stay bare too: `photo.jpg`
    let mut folder = source.parent().map(Path::to_path_buf).unwrap_or_default();
    if !sys::can_write(&folder) {
        folder = fallback.to_path_buf();
        let _ = fs::create_dir(&folder);
    }
    let stem = stem(source);
    let base = if suffix.is_empty() { stem } else { format!("{stem} {suffix}") };
    let name = |counter: Option<u32>| {
        let numbered = match counter {
            Some(n) => format!("{base} {n}"),
            None => base.clone(),
        };
        if ext.is_empty() { numbered } else { format!("{numbered}.{ext}") }
    };
    let mut candidate = name(None);
    let mut counter = 2;
    while taken.contains(&candidate) || folder.join(&candidate).exists() {
        candidate = name(Some(counter));
        counter += 1;
    }
    folder.join(candidate)
}

/// The file name without its last extension: `photo.png` -> `photo`.
pub fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The last extension without the dot, as written: `photo.JPG` -> `JPG`.
pub fn extension(path: &Path) -> String {
    path.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The file name as text, for messages.
pub fn name(path: &Path) -> String {
    path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}
