//! The decode: symphonia's demuxers and decoders (with the system's AAC decoder ahead of symphonia's for
//! [`crate::Backend::Platform`] on Apple platforms), run untrimmed where the file's statements give the trim
//! (MP3, AAC in MP4), with every problem symphonia meets turned into an issue instead of an end.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_AAC, CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3};
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::{Hint, Probe};
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardVisualKey, Tag};

use crate::aac::{AacConfig, AacProfile};
use crate::mp4::Mp4Audio;
use crate::mpeg::Mp3Statements;
use crate::report::{Container, IssueKind, Issues, Position, Report, TrimSource};
use crate::trim::{self, Trim};
use crate::{Error, Picture};

/// `Arc<[u8]>` as a cursor source.
struct Bytes(Arc<[u8]>);

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// What a portable pass gives.
pub(crate) struct Pass {
    pub(crate) samples: Vec<f32>,
    pub(crate) tags: BTreeMap<String, String>,
    pub(crate) pictures: Vec<Picture>,
    pub(crate) report: Report,
}

/// What the caller knows of the file before the pass.
pub(crate) struct Known<'a> {
    pub(crate) ext: Option<&'a str>,
    /// Decode AAC with the system's decoder where there is one.
    pub(crate) platform: bool,
    pub(crate) mp4: Option<&'a Mp4Audio>,
    pub(crate) keep_samples: bool,
    pub(crate) metadata: bool,
    pub(crate) max_seconds: Option<f64>,
}

/// symphonia's decoders with the system's AAC decoder ahead of its own (the same as symphonia's alone where the
/// system has none).
fn platform_codecs() -> &'static CodecRegistry {
    static CODECS: OnceLock<CodecRegistry> = OnceLock::new();
    CODECS.get_or_init(|| {
        let mut codecs = CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut codecs);
        symphonia_adapter_applecoreaudio::register(&mut codecs);
        codecs
    })
}

/// symphonia's container readers with the ADTS reader of `symphonia-adapter-applecoreaudio` ahead of its own, on
/// every platform: the same reader everywhere, and the only one where symphonia's AAC support is left out (DESIGN
/// D12).
fn probe() -> &'static Probe {
    static PROBE: OnceLock<Probe> = OnceLock::new();
    PROBE.get_or_init(|| {
        let mut probe = Probe::default();
        symphonia::default::register_enabled_formats(&mut probe);
        symphonia_adapter_applecoreaudio::register_formats(&mut probe);
        probe
    })
}

/// The container reader of `data`.
pub(crate) fn open(data: Arc<[u8]>, ext: Option<&str>) -> Result<Box<dyn FormatReader>, Error> {
    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(Bytes(data))), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(ext) = ext {
        hint.with_extension(ext);
    }
    probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default()).map_err(|e| match e {
        SymError::Unsupported(what) => Error::UnknownFormat(what.to_string()),
        SymError::IoError(e) => Error::UnknownFormat(e.to_string()),
        other => Error::UnknownFormat(other.to_string()),
    })
}

/// A tag's key: the standard tag's name when symphonia maps it (`TrackTitle`, `Artist`, `Bpm`, ...), else the key as
/// the file writes it.
fn tag_key(t: &Tag) -> String {
    match &t.std {
        Some(std) => {
            let name = format!("{std:?}");
            name.split_once('(').map_or_else(|| name.clone(), |(n, _)| n.to_owned())
        }
        None => t.raw.key.clone(),
    }
}

/// Every tag and picture of the reader: a leading tag first, the container's own after it, so they win.
pub(crate) fn metadata(fmt: &mut dyn FormatReader) -> (BTreeMap<String, String>, Vec<Picture>) {
    let mut revs: Vec<MetadataRevision> = Vec::new();
    let mut log = fmt.metadata();
    while let Some(rev) = log.pop() {
        revs.push(rev);
    }
    if let Some(rev) = log.current() {
        revs.push(rev.clone());
    }
    let mut tags = BTreeMap::new();
    let mut pictures = Vec::new();
    for rev in &revs {
        for t in &rev.media.tags {
            let v = t.raw.value.to_string();
            if !v.trim().is_empty() {
                tags.insert(tag_key(t), v);
            }
        }
        for v in &rev.media.visuals {
            pictures.push(Picture {
                front_cover: v.usage == Some(StandardVisualKey::FrontCover),
                media_type: v.media_type.clone(),
                data: v.data.to_vec(),
            });
        }
    }
    (tags, pictures)
}

fn container(fmt: &dyn FormatReader) -> Container {
    match fmt.format_info().short_name {
        "isomp4" => Container::Mp4,
        "mp3" | "mpa" | "mpeg" => Container::Mpeg,
        "flac" => Container::Flac,
        "wave" | "wav" | "riff" => Container::Wav,
        "aiff" => Container::Aiff,
        "ogg" => Container::Ogg,
        "adts" | "aac" => Container::Adts,
        _ => Container::Other,
    }
}

/// Decodes the default audio track of `data`.
pub(crate) fn pass(data: Arc<[u8]>, known: &Known<'_>) -> Result<Pass, Error> {
    let mut fmt = open(Arc::clone(&data), known.ext)?;
    let (tags, pictures) = if known.metadata { metadata(fmt.as_mut()) } else { Default::default() };
    let container = container(fmt.as_ref());
    let t = fmt.default_track(TrackType::Audio).ok_or(Error::NoAudioTrack)?;
    let id = t.id;
    let params = t.codec_params.as_ref().and_then(|p| p.audio()).ok_or(Error::NoAudioTrack)?.clone();
    let (track_frames, track_delay) = (t.num_frames, t.delay);
    let mpeg = matches!(params.codec, CODEC_ID_MP3 | CODEC_ID_MP2 | CODEC_ID_MP1);
    let aac = params.codec == CODEC_ID_AAC;
    let mut issues = Issues::default();
    let statements: Option<Mp3Statements> = if mpeg { crate::mpeg::read(&data) } else { None };
    if let Some(s) = &statements {
        issues.merge(s.issues.clone());
    }
    // MPEG audio runs untrimmed (its trim comes from the statements); everything else in the codec's gapless mode
    let config = if aac { known.mp4.and_then(|m| m.aac) } else { None };
    let profile = config.map(|c| c.profile());
    let opts = AudioDecoderOptions::default().gapless(!mpeg);
    let codecs = if known.platform { platform_codecs() } else { symphonia::default::get_codecs() };
    let mut dec = codecs.make_audio_decoder(&params, &opts).map_err(|e| {
        let what = match e {
            SymError::Unsupported(what) => what.to_string(),
            other => other.to_string(),
        };
        // name the profile the decoder lacks (xHE-AAC, AAC Main, ...)
        Error::UnsupportedCodec(
            profile.filter(|&p| p != AacProfile::Lc).map_or(what.clone(), |p| format!("{p}: {what}")),
        )
    })?;
    let codec = dec.codec_info().short_name.to_owned();
    // what the container counts (packet durations, track length) is at the track's rate; a decoder that applies SBR
    // outputs twice as many frames (DESIGN D3)
    let track_rate = params.sample_rate.unwrap_or(0);
    let given = dec.codec_params();
    let mut rate = given.sample_rate.unwrap_or(track_rate);
    let mut channels = given.channels.as_ref().or(params.channels.as_ref()).map_or(0, |c| c.count());
    let out_rate = rate;
    let at_out = |n: u64| match (track_rate, out_rate) {
        (0, _) | (_, 0) => n,
        (t, o) => n * u64::from(o) / u64::from(t),
    };
    let track_frames = track_frames.map(at_out);
    // the start trim is known before decoding: the statements' (MP3), AudioToolbox's rule (AAC without a codec delay)
    let mut trim = match &statements {
        Some(s) => trim::mp3(s, s.audio_frames * u64::from(s.samples_per_frame)),
        // without MP4 statements (ADTS) the default is at the stream's rate, the core rate of HE-AAC
        None if aac && track_delay.is_none() && known.mp4.is_none() => {
            let t = trim::aac(None, f64::from(rate));
            Trim { start: at_out(t.start), ..t }
        }
        None if aac && track_delay.is_none() => trim::aac(known.mp4, f64::from(rate)),
        None if track_delay.is_some() => Trim { start: 0, frames: None, source: TrimSource::Codec },
        None => Trim::NONE,
    };
    if aac && trim.source == TrimSource::Default {
        let detail = format!("no edit list or iTunSMPB: {} frames dropped, Apple's default", trim.start);
        issues.add(IssueKind::NoGaplessInfo, 1, None, detail);
    }
    let mut skip = trim.start;
    let mut raw = 0u64;
    let mut out: Vec<f32> = Vec::new();
    let mut buf: Vec<f32> = Vec::new();
    let mut kept = 0u64;
    let mut format: Option<(u32, usize)> = None;
    // the longest packet: a stated length shorter than the decode by less than one is rounding, not a cut
    let mut longest = 0u64;
    loop {
        let pkt = match fmt.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::ResetRequired) => break,
            // the ADTS reader skips junk up to the next frame and says so once (DESIGN D12)
            Err(SymError::DecodeError(e)) if container == Container::Adts => {
                issues.add(IssueKind::StreamGap, 1, Some(Position::Frame(raw)), e);
                continue;
            }
            Err(e) => {
                if !mpeg {
                    issues.add(
                        IssueKind::Truncated,
                        1,
                        Some(Position::Frame(raw)),
                        format!("the stream ends early: {e}"),
                    );
                }
                break;
            }
        };
        if pkt.track_id != id {
            continue;
        }
        longest = longest.max(at_out(pkt.dur.get()));
        let frames_in: u64;
        match dec.decode(&pkt) {
            Ok(a) => {
                let (r, ch) = (a.spec().rate(), a.spec().channels().count().max(1));
                match format {
                    Some(f) if f != (r, ch) && !mpeg => issues.add(
                        IssueKind::FormatChange,
                        1,
                        Some(Position::Frame(raw)),
                        format!("from {} Hz {} ch to {r} Hz {ch} ch", f.0, f.1),
                    ),
                    _ => {}
                }
                format.get_or_insert((r, ch));
                (rate, channels) = (r, ch);
                buf.resize(a.samples_interleaved(), 0.0);
                a.copy_to_slice_interleaved(&mut buf);
                frames_in = (buf.len() / ch) as u64;
                let lo = skip.min(frames_in);
                skip -= lo;
                if known.keep_samples {
                    for fr in buf[lo as usize * ch..].chunks_exact(ch) {
                        out.push(fr[0]);
                        out.push(if ch > 1 { fr[1] } else { fr[0] });
                    }
                }
                kept += frames_in - lo;
            }
            Err(SymError::DecodeError(e)) => {
                // a bad frame becomes silence of its length, so the rest keeps its place in time
                frames_in = at_out(pkt.dur.get());
                issues.add(IssueKind::UndecodableFrames, 1, Some(Position::Frame(raw)), e.to_string());
                let lo = skip.min(frames_in);
                skip -= lo;
                if known.keep_samples {
                    out.resize(out.len() + 2 * (frames_in - lo) as usize, 0.0);
                }
                kept += frames_in - lo;
            }
            Err(SymError::ResetRequired) => break,
            Err(e) => {
                issues.add(IssueKind::UndecodableFrames, 1, Some(Position::Frame(raw)), e.to_string());
                break;
            }
        }
        raw += frames_in;
        if let Some(max) = known.max_seconds
            && rate > 0
            && raw as f64 / f64::from(rate) > max
        {
            return Err(Error::TooLong { limit: max });
        }
    }
    if raw == 0 {
        return Err(Error::NoAudio(issues.0.first().map_or_else(|| "no frame decoded".into(), |i| i.to_string())));
    }
    // the end trim: MP3 from the statements against what was decoded, others as the file states
    let mut declared = None;
    if let Some(s) = &statements {
        trim = trim::mp3(s, raw);
        if !s.end_is_stated() {
            trim.frames = None;
        }
        declared = s.stated_frames.filter(|_| trim.frames.is_some()).and(trim.frames);
    } else if aac && trim.source != TrimSource::Codec {
        declared = trim.frames.or(track_frames.map(|n| n.saturating_sub(trim.start)));
    } else {
        declared = declared.or(track_frames);
    }
    let end_keep = trim.frames.unwrap_or(kept).min(kept);
    if known.keep_samples {
        out.truncate(end_keep as usize * 2);
    }
    if statements.is_none()
        && let Some(d) = declared
        && end_keep.saturating_add(longest) <= d
        && !issues.has(IssueKind::Truncated)
    {
        issues.add(IssueKind::Truncated, 1, Some(Position::Frame(raw)), format!("{end_keep} of {d} stated frames"));
    }
    if aac && let Some(note) = reduced_quality(config, rate) {
        issues.add(IssueKind::ReducedQuality, 1, None, note);
    }
    let report = Report {
        container,
        codec,
        profile: profile.map(|p| p.to_string()),
        rate,
        source_channels: u16::try_from(channels).unwrap_or(u16::MAX),
        frames: end_keep,
        trim_start: trim.start.min(raw),
        trim_end: kept - end_keep,
        trim_source: trim.source,
        declared_frames: declared,
        issues: issues.0,
    };
    Ok(Pass { samples: out, tags, pictures, report })
}

/// What an AAC decode without SBR / PS gave up, from the declared configuration (None: ADTS, no MP4 statement) and
/// the decoded rate (DESIGN D11). A decode above the declared core rate applied SBR (and PS): nothing was given up.
fn reduced_quality(config: Option<AacConfig>, rate: u32) -> Option<String> {
    use crate::aac::IMPLICIT_SBR_MAX_CORE_RATE;
    if config.is_some_and(|c| rate > c.rate) {
        return None;
    }
    let full = rate.saturating_mul(2);
    match config.map(|c| (c.profile(), c.may_hide_sbr())) {
        Some((AacProfile::He, _)) => {
            Some(format!("HE-AAC decoded without SBR: its {rate} Hz core, half the bandwidth (full: {full} Hz)"))
        }
        Some((AacProfile::HeV2, _)) => {
            Some(format!("HE-AAC v2 decoded without SBR and PS: its {rate} Hz mono core (full: {full} Hz stereo)"))
        }
        Some((_, true)) => Some(format!("possibly HE-AAC (SBR not stated): decoded at {rate} Hz without SBR")),
        None if rate <= IMPLICIT_SBR_MAX_CORE_RATE => {
            Some(format!("possibly HE-AAC (SBR not stated): decoded at {rate} Hz without SBR"))
        }
        _ => None,
    }
}
