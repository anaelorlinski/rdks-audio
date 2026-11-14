//! Where sample 0 of a decoded stream sits: the frames dropped at the start and kept after them, from what the file
//! states (gapless statements).
//!
//! - AAC in MP4 follows AudioToolbox's rule, measured on macOS 15.8: the edit list when the track also has a `roll`
//!   sample group, else the `iTunSMPB` statement, else a fixed 2112 at the start and nothing at the end. The same rule
//!   on every platform gives every platform the same timeline.
//! - MP3 follows the LAME rule: the header frame (Xing / Info / VBRI) is never audio; with a LAME / ffmpeg extension
//!   the encoder delay plus the decoder's own delay (529) goes at the start and the padding minus 529 at the end.

use crate::mp4::Mp4Audio;
use crate::mpeg::Mp3Statements;
use crate::report::TrimSource;

/// Apple's AAC encoder delay: what AudioToolbox drops when the file states nothing.
pub const APPLE_AAC_PRIMING: u64 = 2112;

/// The delay of an MPEG audio layer III decoder's synthesis filter bank, in frames: the LAME extension's encoder
/// delay does not count it.
pub const MP3_DECODER_DELAY: u64 = 529;

/// Frames to drop at the start of a decode, then how many to keep (None: all), at the decoded rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trim {
    /// Frames dropped at the start.
    pub start: u64,
    /// Frames kept after them; None keeps everything.
    pub frames: Option<u64>,
    /// What stated it.
    pub source: TrimSource,
}

impl Trim {
    /// Nothing dropped.
    pub const NONE: Trim = Trim { start: 0, frames: None, source: TrimSource::None };
}

/// AudioToolbox's trim of an AAC track whose decode runs at `rate`: the edit list when a `roll` group comes with it,
/// else the `iTunSMPB` statement, else [`APPLE_AAC_PRIMING`] and no end trim. `None` (no MP4 statements: ADTS, a
/// broken file) is the last case. Every count is at the track's rate (the core rate of HE-AAC) and scaled to `rate`:
/// HE-AAC decoded with SBR drops twice the stated priming (the SBR delay is already gone from the system decoder's
/// output), decoded at its core rate the stated counts.
pub fn aac(a: Option<&Mp4Audio>, rate: f64) -> Trim {
    let default = Trim { start: APPLE_AAC_PRIMING, frames: None, source: TrimSource::Default };
    let Some(a) = a else { return default };
    let frames = |n: u64, scale: u32| (scale > 0).then(|| (n as f64 * rate / f64::from(scale)).round() as u64);
    if let Some(e) = a.edit.filter(|_| a.roll)
        && let Some(start) = frames(e.media_time, a.media_scale)
    {
        let kept = frames(e.duration, a.movie_scale).filter(|&n| n > 0);
        return Trim { start, frames: kept, source: TrimSource::EditList };
    }
    if let Some(s) = a.smpb {
        let start = frames(s.priming, a.media_scale).unwrap_or(s.priming);
        let kept = frames(s.valid, a.media_scale).unwrap_or(s.valid);
        return Trim { start, frames: Some(kept).filter(|&n| n > 0), source: TrimSource::Tag };
    }
    let start = frames(APPLE_AAC_PRIMING, a.media_scale).unwrap_or(APPLE_AAC_PRIMING);
    Trim { start, ..default }
}

/// The trim of an MP3 decoded without its header frame (the decoder never outputs it) from its statements and the
/// frames the decoder gives (`decoded`): the LAME extension's delay and padding when it is usable, else nothing.
pub fn mp3(s: &Mp3Statements, decoded: u64) -> Trim {
    match s.gapless() {
        Some((delay, padding)) => {
            let start = delay + MP3_DECODER_DELAY;
            let end = padding.saturating_sub(MP3_DECODER_DELAY);
            let kept = decoded.saturating_sub(start).saturating_sub(end);
            Trim { start, frames: Some(kept), source: TrimSource::Tag }
        }
        None => Trim::NONE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4::{Edit, Smpb};

    fn aac_track(edit: Option<(u64, u64)>, roll: bool, smpb: Option<(u64, u64)>) -> Mp4Audio {
        Mp4Audio {
            codec: *b"mp4a",
            movie_scale: 1000,
            media_scale: 44100,
            edit: edit.map(|(media_time, duration)| Edit { media_time, duration }),
            roll,
            smpb: smpb.map(|(priming, valid)| Smpb { priming, remainder: 0, valid }),
            aac: None,
        }
    }

    #[test]
    fn audiotoolbox_order() {
        // DESIGN D2: the edit list only with a roll group, then the tag, then 2112
        let t = aac(Some(&aac_track(Some((1024, 1000)), true, Some((2112, 9)))), 44100.0);
        assert_eq!((t.start, t.frames, t.source), (1024, Some(44100), TrimSource::EditList));
        let t = aac(Some(&aac_track(Some((1024, 1000)), false, Some((2112, 9)))), 44100.0);
        assert_eq!((t.start, t.frames, t.source), (2112, Some(9), TrimSource::Tag));
        let t = aac(Some(&aac_track(Some((1024, 1000)), false, None)), 44100.0);
        assert_eq!((t.start, t.frames, t.source), (2112, None, TrimSource::Default));
        assert_eq!(aac(None, 44100.0).start, APPLE_AAC_PRIMING);
        // an edit in another rate: 2298 at 48 kHz is 2111.2 at 44.1 kHz
        let mut a = aac_track(Some((2298, 1000)), true, None);
        a.media_scale = 48000;
        assert_eq!(aac(Some(&a), 44100.0).start, 2111);
    }
}
