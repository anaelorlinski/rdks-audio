#![doc = include_str!("../README.md")]

pub mod aac;
pub mod mp4;
pub mod mpeg;
mod portable;
mod report;
pub mod trim;

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

pub use report::{Container, Issue, IssueKind, Position, Report, Severity, TrimSource};

use report::Issues;

/// Which decoder reads AAC (in MP4 / M4A or ADTS).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Backend {
    /// The system's own on Apple platforms (HE-AAC and xHE-AAC in full, through
    /// `symphonia-adapter-applecoreaudio`), else the portable one.
    #[default]
    Platform,
    /// symphonia's everywhere (AAC-LC; the core of HE-AAC).
    Portable,
}

/// How to decode.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Options {
    /// The AAC decoder.
    pub backend: Backend,
    /// Read tags and pictures.
    pub metadata: bool,
    /// Refuse inputs larger than this many bytes.
    pub max_bytes: Option<u64>,
    /// Stop with [`Error::TooLong`] past this many seconds of audio.
    pub max_seconds: Option<f64>,
}

impl Default for Options {
    fn default() -> Self {
        Options { backend: Backend::Platform, metadata: true, max_bytes: None, max_seconds: None }
    }
}

impl Options {
    /// The AAC decoder.
    #[must_use]
    pub fn backend(mut self, backend: Backend) -> Self {
        self.backend = backend;
        self
    }

    /// Read tags and pictures.
    #[must_use]
    pub fn metadata(mut self, metadata: bool) -> Self {
        self.metadata = metadata;
        self
    }

    /// Refuse inputs larger than `bytes`.
    #[must_use]
    pub fn max_bytes(mut self, bytes: u64) -> Self {
        self.max_bytes = Some(bytes);
        self
    }

    /// Stop past `seconds` of audio.
    #[must_use]
    pub fn max_seconds(mut self, seconds: f64) -> Self {
        self.max_seconds = Some(seconds);
        self
    }
}

/// An embedded picture, as the file holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// Marked as the front cover.
    pub front_cover: bool,
    /// Its media type, when stated (`image/jpeg`, `image/png`).
    pub media_type: Option<String>,
    /// The encoded image.
    pub data: Vec<u8>,
}

/// A decoded file.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    /// Sample rate.
    pub rate: u32,
    /// Interleaved stereo (a mono file on both channels, a multichannel one's first two).
    pub samples: Vec<f32>,
    /// Tags: standard keys by name (`TrackTitle`, `Artist`, `Album`, `Bpm`, ...), others by their key in the file.
    pub tags: BTreeMap<String, String>,
    /// Embedded pictures.
    pub pictures: Vec<Picture>,
    /// What the decode found.
    pub report: Report,
}

impl Decoded {
    /// Frames of audio (one sample per channel).
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    /// The front cover, else the first picture.
    pub fn cover(&self) -> Option<&Picture> {
        self.pictures.iter().find(|p| p.front_cover).or_else(|| self.pictures.first())
    }
}

/// Why nothing usable came out of a file.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The file could not be read.
    Io(std::io::Error),
    /// The input is larger than [`Options::max_bytes`].
    TooLarge {
        /// The limit.
        limit: u64,
    },
    /// The audio is longer than [`Options::max_seconds`].
    TooLong {
        /// The limit.
        limit: f64,
    },
    /// No reader recognises the container.
    UnknownFormat(String),
    /// The container has no audio track.
    NoAudioTrack,
    /// No decoder for the track's codec.
    UnsupportedCodec(String),
    /// The track gave no audio at all.
    NoAudio(String),
    /// A decoder library failed on the input (a bug it has on hostile data).
    DecoderFailure(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "cannot read the file: {e}"),
            Error::TooLarge { limit } => write!(f, "larger than {limit} bytes"),
            Error::TooLong { limit } => write!(f, "longer than {limit} s"),
            Error::UnknownFormat(e) => write!(f, "unknown format: {e}"),
            Error::NoAudioTrack => f.write_str("no audio track"),
            Error::UnsupportedCodec(e) => write!(f, "unsupported codec: {e}"),
            Error::NoAudio(e) => write!(f, "no audio decoded: {e}"),
            Error::DecoderFailure(e) => write!(f, "decoder failure: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Decodes a whole file held in memory to interleaved stereo f32 at its own rate, from its exact first sample, as far
/// as it can be decoded. `ext` (the file's extension) helps find the container.
///
/// # Errors
///
/// [`Error`] when nothing usable comes out; any damage short of that is in the report.
pub fn decode(data: impl Into<Arc<[u8]>>, ext: Option<&str>, options: &Options) -> Result<Decoded, Error> {
    let data = data.into();
    guarded(|| run(data, ext, options, true))
}

/// [`decode`] of a file on disk; its extension is the hint.
///
/// # Errors
///
/// [`Error::Io`] when the file cannot be read, else as [`decode`].
pub fn decode_file(path: impl AsRef<Path>, options: &Options) -> Result<Decoded, Error> {
    let path = path.as_ref();
    let data = read(path, options)?;
    decode(data, extension(path).as_deref(), options)
}

/// Decodes the whole file without keeping the samples, to find what is wrong with it.
///
/// # Errors
///
/// As [`decode`].
pub fn check(data: impl Into<Arc<[u8]>>, ext: Option<&str>, options: &Options) -> Result<Report, Error> {
    let data = data.into();
    guarded(|| run(data, ext, options, false)).map(|d| d.report)
}

/// [`check`] of a file on disk.
///
/// # Errors
///
/// As [`decode_file`].
pub fn check_file(path: impl AsRef<Path>, options: &Options) -> Result<Report, Error> {
    let path = path.as_ref();
    let data = read(path, options)?;
    check(data, extension(path).as_deref(), options)
}

fn read(path: &Path, options: &Options) -> Result<Vec<u8>, Error> {
    if let Some(limit) = options.max_bytes
        && std::fs::metadata(path)?.len() > limit
    {
        return Err(Error::TooLarge { limit });
    }
    Ok(std::fs::read(path)?)
}

fn extension(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase)
}

/// Runs a decode, turning a panic inside a decoder library (on hostile input) into [`Error::DecoderFailure`].
fn guarded<T>(run: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|e| {
        let what = e.downcast_ref::<&str>().map(|s| (*s).to_owned()).or_else(|| e.downcast_ref::<String>().cloned());
        Err(Error::DecoderFailure(what.unwrap_or_default()))
    })
}

/// An MP4 container: by its extension, else by its `ftyp` box.
fn is_mp4(data: &[u8], ext: Option<&str>) -> bool {
    const MP4_EXTENSIONS: [&str; 5] = ["m4a", "mp4", "m4b", "m4v", "mov"];
    ext.is_some_and(|e| MP4_EXTENSIONS.iter().any(|m| e.eq_ignore_ascii_case(m))) || data.get(4..8) == Some(b"ftyp")
}

/// A FLAC stream: by its extension, else by its `fLaC` marker at the start or after an ID3v2 tag.
fn is_flac(data: &[u8], ext: Option<&str>) -> bool {
    const ID3V2_HEADER: usize = 10;
    if ext.is_some_and(|e| e.eq_ignore_ascii_case("flac")) || data.starts_with(b"fLaC") {
        return true;
    }
    let Some(h) = data.get(..ID3V2_HEADER).filter(|h| h.starts_with(b"ID3")) else { return false };
    if h[6..].iter().any(|&b| b >= 0x80) {
        return false;
    }
    let size = h[6..].iter().fold(0usize, |n, &b| (n << 7) | usize::from(b));
    data.get(ID3V2_HEADER + size..).is_some_and(|rest| rest.starts_with(b"fLaC"))
}

/// The tags written after a FLAC stream's last frame, which FLAC does not define (DESIGN D14): an ID3v1 tag, an
/// APEv2 tag, or an APEv2 tag then an ID3v1 tag. Where they start and what they are; None when the data ends with
/// the stream.
fn tags_after_flac(data: &[u8]) -> Option<(usize, &'static str)> {
    const ID3V1: usize = 128;
    const APE_FOOTER: usize = 32;
    /// APEv2 flags: the tag has a header before its items.
    const APE_HAS_HEADER: u32 = 1 << 31;
    let mut end = data.len();
    let id3v1 = end >= ID3V1 && data[end - ID3V1..].starts_with(b"TAG");
    if id3v1 {
        end -= ID3V1;
    }
    let mut ape = false;
    if let Some(f) = end.checked_sub(APE_FOOTER).map(|at| &data[at..end])
        && f.starts_with(b"APETAGEX")
    {
        let word = |at: usize| u32::from_le_bytes([f[at], f[at + 1], f[at + 2], f[at + 3]]) as usize;
        // the footer's size counts the items and the footer
        let header = if word(20) as u32 & APE_HAS_HEADER != 0 { APE_FOOTER } else { 0 };
        if let Some(start) = word(12).checked_add(header).and_then(|n| end.checked_sub(n)) {
            end = start;
            ape = true;
        }
    }
    let what = match (ape, id3v1) {
        (true, true) => "an APEv2 and an ID3v1 tag",
        (true, false) => "an APEv2 tag",
        (false, true) => "an ID3v1 tag",
        (false, false) => return None,
    };
    Some((end, what))
}

fn run(data: Arc<[u8]>, ext: Option<&str>, options: &Options, keep: bool) -> Result<Decoded, Error> {
    if let Some(limit) = options.max_bytes
        && data.len() as u64 > limit
    {
        return Err(Error::TooLarge { limit });
    }
    let mut early = Issues::default();
    let mut data = data;
    let mut mp4_audio = None;
    if is_mp4(&data, ext) {
        if let Some(fixed) = mp4::cut_short(&data) {
            early.add(IssueKind::Truncated, 1, None, "the sample data is cut: the frames that are there are decoded");
            data = fixed.into();
        }
        mp4_audio = mp4::audio(&data);
    } else if is_flac(&data, ext)
        && let Some((end, what)) = tags_after_flac(&data)
    {
        // read as part of the last frame, they would cut it (DESIGN D14)
        early.add(
            IssueKind::JunkOutsideAudio,
            1,
            Some(Position::Byte(end as u64)),
            format!("{what} after the audio ({} bytes), which FLAC does not define: left out", data.len() - end),
        );
        data = data[..end].into();
    }
    let known = portable::Known {
        ext,
        platform: options.backend == Backend::Platform,
        mp4: mp4_audio.as_ref(),
        keep_samples: keep,
        metadata: options.metadata && keep,
        max_seconds: options.max_seconds,
    };
    let pass = portable::pass(data, &known)?;
    let mut report = pass.report;
    if early.has(IssueKind::Truncated) {
        // the cut found before decoding is the same one the decode runs into
        report.issues.retain(|i| i.kind != IssueKind::Truncated);
    }
    early.merge(Issues(report.issues));
    report.issues = early.0;
    Ok(Decoded { rate: report.rate, samples: pass.samples, tags: pass.tags, pictures: pass.pictures, report })
}
