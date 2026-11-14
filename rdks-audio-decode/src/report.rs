//! What a decode found wrong in a file, classified so that a caller can decide what to do with it: a player plays
//! the file and warns, a checker rejects it when the audio is damaged.

use std::fmt;

/// How bad an issue is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// The audio is whole: metadata problems, missing timing statements, bytes outside the audio.
    Note,
    /// The audio decoded, but is not what was encoded: bad frames, a skip inside the stream, a cut, a format change.
    Damage,
}

/// What kind of problem an [`Issue`] is. New kinds may be added; match with a wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IssueKind {
    /// A tag frame or item is malformed and was skipped (or the whole tag was unreadable). [`Severity::Note`]
    TagDamaged,
    /// A gapless statement is present but was not used (inconsistent with the stream). [`Severity::Note`]
    GaplessIgnored,
    /// A gapless statement's checksum does not match; its values were used because they are plausible.
    /// [`Severity::Note`]
    GaplessChecksum,
    /// Nothing states the encoder delay: sample 0 is where the decoder puts it, not where the encoded audio
    /// started. [`Severity::Note`]
    NoGaplessInfo,
    /// Bytes that are neither audio nor a tag before the first frame or after the last. [`Severity::Note`]
    JunkOutsideAudio,
    /// The file is whole, but the decoder gives less than it holds: HE-AAC without its SBR / PS layers (half the
    /// bandwidth, mono for v2). A player may want to show it. [`Severity::Note`]
    ReducedQuality,
    /// Frames that failed to decode; each was replaced by silence of its length. [`Severity::Damage`]
    UndecodableFrames,
    /// Bytes that are not audio inside the stream, between two frames: a skip in the audio. [`Severity::Damage`]
    StreamGap,
    /// The stream ends before its end: a cut last frame, or fewer frames than the file states. [`Severity::Damage`]
    Truncated,
    /// The sample rate, channel count or codec layer changes inside the stream. [`Severity::Damage`]
    FormatChange,
}

impl IssueKind {
    /// The severity of this kind.
    pub fn severity(self) -> Severity {
        match self {
            IssueKind::TagDamaged
            | IssueKind::GaplessIgnored
            | IssueKind::GaplessChecksum
            | IssueKind::NoGaplessInfo
            | IssueKind::JunkOutsideAudio
            | IssueKind::ReducedQuality => Severity::Note,
            IssueKind::UndecodableFrames | IssueKind::StreamGap | IssueKind::Truncated | IssueKind::FormatChange => {
                Severity::Damage
            }
        }
    }

    /// A stable lower-case name (`tag_damaged`, `stream_gap`, ...), for logs and JSON.
    pub fn name(self) -> &'static str {
        match self {
            IssueKind::TagDamaged => "tag_damaged",
            IssueKind::GaplessIgnored => "gapless_ignored",
            IssueKind::GaplessChecksum => "gapless_checksum",
            IssueKind::NoGaplessInfo => "no_gapless_info",
            IssueKind::JunkOutsideAudio => "junk_outside_audio",
            IssueKind::ReducedQuality => "reduced_quality",
            IssueKind::UndecodableFrames => "undecodable_frames",
            IssueKind::StreamGap => "stream_gap",
            IssueKind::Truncated => "truncated",
            IssueKind::FormatChange => "format_change",
        }
    }
}

impl fmt::Display for IssueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Where in the file an issue was first seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    /// A byte offset in the file.
    Byte(u64),
    /// A frame of the decoded output (before the start trim).
    Frame(u64),
}

/// One kind of problem: how often it happened and where first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    /// What it is.
    pub kind: IssueKind,
    /// How many times it happened (frames, gaps, tag frames, ...).
    pub count: u32,
    /// Where it was first seen.
    pub at: Option<Position>,
    /// A short human-readable description of the first occurrence.
    pub detail: String,
}

impl Issue {
    /// The severity of the issue's kind.
    pub fn severity(&self) -> Severity {
        self.kind.severity()
    }
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        if self.count > 1 {
            write!(f, " x{}", self.count)?;
        }
        match self.at {
            Some(Position::Byte(b)) => write!(f, " at byte {b}")?,
            Some(Position::Frame(n)) => write!(f, " at frame {n}")?,
            None => {}
        }
        if !self.detail.is_empty() {
            write!(f, ": {}", self.detail)?;
        }
        Ok(())
    }
}

/// The container of a decoded file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Container {
    /// MPEG audio elementary stream (MP3, MP2, MP1), with its tags.
    Mpeg,
    /// MP4 / M4A (ISO base media).
    Mp4,
    /// FLAC.
    Flac,
    /// RIFF WAVE.
    Wav,
    /// AIFF / AIFC.
    Aiff,
    /// Ogg (Vorbis, Opus, FLAC).
    Ogg,
    /// Raw AAC (ADTS).
    Adts,
    /// Another container symphonia reads (Matroska / WebM, CAF, ...).
    Other,
}

/// What stated the start and end trims.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrimSource {
    /// Nothing trimmed (lossless formats, or nothing stated).
    None,
    /// A gapless tag: LAME / ffmpeg extension, iTunes `iTunSMPB`.
    Tag,
    /// An MP4 edit list.
    EditList,
    /// The codec's own rule (Vorbis / Opus pre-skip).
    Codec,
    /// A default delay applied because nothing was stated (AAC: 2112).
    Default,
}

/// Everything a decode learned about a file.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    /// The container.
    pub container: Container,
    /// The codec's short name (`mp3`, `aac`, `alac`, `flac`, `pcm_s16le`, ...).
    pub codec: String,
    /// The profile the file declares, where it matters for decoding (AAC: `aac-lc`, `he-aac`, `he-aac-v2`,
    /// `xhe-aac`).
    pub profile: Option<String>,
    /// The decoded sample rate.
    pub rate: u32,
    /// The file's channel count (the output is always stereo).
    pub source_channels: u16,
    /// Frames of the output, after the trims.
    pub frames: u64,
    /// Frames dropped at the start.
    pub trim_start: u64,
    /// Frames dropped at the end.
    pub trim_end: u64,
    /// What stated the trims.
    pub trim_source: TrimSource,
    /// Frames the file states it has (after its own trims), when it states it.
    pub declared_frames: Option<u64>,
    /// The problems found, one entry per kind.
    pub issues: Vec<Issue>,
}

impl Report {
    /// The output's duration.
    pub fn seconds(&self) -> f64 {
        if self.rate == 0 { 0.0 } else { self.frames as f64 / f64::from(self.rate) }
    }

    /// No [`Severity::Damage`]: the audio is what was encoded.
    pub fn is_valid(&self) -> bool {
        self.issues.iter().all(|i| i.severity() == Severity::Note)
    }

    /// The issues of one severity.
    pub fn issues_of(&self, severity: Severity) -> impl Iterator<Item = &Issue> {
        self.issues.iter().filter(move |i| i.severity() == severity)
    }

    /// The issue of a kind, if found.
    pub fn issue(&self, kind: IssueKind) -> Option<&Issue> {
        self.issues.iter().find(|i| i.kind == kind)
    }
}

/// Collects issues, one entry per kind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Issues(pub(crate) Vec<Issue>);

impl Issues {
    /// Adds `count` occurrences of `kind`; the first one keeps its place and detail.
    pub(crate) fn add(&mut self, kind: IssueKind, count: u32, at: Option<Position>, detail: impl Into<String>) {
        if let Some(i) = self.0.iter_mut().find(|i| i.kind == kind) {
            i.count = i.count.saturating_add(count);
        } else {
            self.0.push(Issue { kind, count, at, detail: detail.into() });
        }
    }

    /// Adds every issue of `other`.
    pub(crate) fn merge(&mut self, other: Issues) {
        for i in other.0 {
            self.add(i.kind, i.count, i.at, i.detail);
        }
    }

    pub(crate) fn has(&self, kind: IssueKind) -> bool {
        self.0.iter().any(|i| i.kind == kind)
    }
}
