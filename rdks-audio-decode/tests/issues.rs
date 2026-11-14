//! Damage classes, made from the clean fixtures in the test (DESIGN D1, D7 to D9): each is found, classified, and
//! never stops the decode short of no audio at all; `check` reports what `decode` reports.

mod common;

use common::fixture;
use rdks_audio_decode::{Error, IssueKind, Options, Severity, check, decode};

fn bytes(name: &str) -> Vec<u8> {
    std::fs::read(fixture(name)).unwrap()
}

fn kinds(r: &rdks_audio_decode::Report) -> Vec<IssueKind> {
    r.issues.iter().map(|i| i.kind).collect()
}

/// Offset of the first `mdat` payload.
fn mdat(b: &[u8]) -> usize {
    b.windows(4).position(|w| w == b"mdat").expect("mdat") + 4
}

#[test]
fn mp3_junk_inside_the_stream_is_a_gap() {
    // DESIGN D7
    let mut b = bytes("mp3_lame_cbr.mp3");
    let at = b.len() / 2;
    b.splice(at..at, [0x5a_u8; 333]);
    let d = decode(b.clone(), Some("mp3"), &Options::default()).unwrap();
    assert!(!d.report.is_valid());
    assert!(kinds(&d.report).contains(&IssueKind::StreamGap), "{:?}", d.report.issues);
    assert_eq!(check(b, Some("mp3"), &Options::default()).unwrap().issues, d.report.issues);
}

#[test]
fn mp3_cut_is_truncated_and_decodes_what_is_there() {
    // DESIGN D7
    let mut b = bytes("mp3_lame_cbr.mp3");
    b.truncate(b.len() * 2 / 3);
    let d = decode(b, Some("mp3"), &Options::default()).unwrap();
    assert!(kinds(&d.report).contains(&IssueKind::Truncated), "{:?}", d.report.issues);
    assert!(d.frames() > 10_000);
}

#[test]
fn mp4_garbage_in_a_frame_is_undecodable_and_keeps_the_length() {
    // DESIGN D3, D7, D8: on the default backend (the system's decoder on Apple platforms)
    let clean = bytes("a_itunes.m4a");
    let mut b = clean.clone();
    let at = mdat(&b) + (b.len() - mdat(&b)) / 2;
    for (i, x) in b[at..at + 600].iter_mut().enumerate() {
        *x = (i as u8).wrapping_mul(97) ^ 0xa5;
    }
    let opts = Options::default();
    let r = check(b, Some("m4a"), &opts).unwrap();
    assert!(!r.is_valid(), "{:?}", r.issues);
    assert!(r.issues.iter().all(|i| i.severity() == Severity::Damage), "{:?}", r.issues);
    assert_eq!(r.frames, check(clean, Some("m4a"), &opts).unwrap().frames);
}

#[test]
fn mp4_cut_inside_its_samples_decodes_up_to_the_cut() {
    // DESIGN D9
    let mut b = bytes("a_itunes.m4a");
    b.truncate(mdat(&b) + (b.len() - mdat(&b)) / 2);
    for options in [Options::default(), Options::default().backend(rdks_audio_decode::Backend::Portable)] {
        let d = decode(b.clone(), Some("m4a"), &options).unwrap();
        assert!(kinds(&d.report).contains(&IssueKind::Truncated), "{:?}", d.report.issues);
        assert!(d.frames() > 5_000 && d.frames() < 20_000, "{}", d.frames());
    }
}

#[test]
fn mp4_with_its_head_destroyed_is_an_error() {
    // DESIGN D1
    let mut b = bytes("a_itunes.m4a");
    b[..64].fill(0x33);
    assert!(matches!(decode(b, Some("m4a"), &Options::default()), Err(Error::UnknownFormat(_) | Error::NoAudioTrack)));
}

#[test]
fn limits_stop_a_service_early() {
    // DESIGN D10
    let b = bytes("mp3_lame_cbr.mp3");
    assert!(matches!(check(b.clone(), Some("mp3"), &Options::default().max_bytes(100)), Err(Error::TooLarge { .. })));
    assert!(matches!(check(b, Some("mp3"), &Options::default().max_seconds(0.1)), Err(Error::TooLong { .. })));
}

#[test]
fn a_cut_counts_once() {
    // DESIGN D7, D9
    let mut b = bytes("a_itunes.m4a");
    b.truncate(mdat(&b) + (b.len() - mdat(&b)) / 2);
    let r = check(b, Some("m4a"), &Options::default()).unwrap();
    assert_eq!(r.issue(IssueKind::Truncated).map(|i| i.count), Some(1), "{:?}", r.issues);
}

#[test]
fn he_aac_without_sbr_is_a_note() {
    // DESIGN D11: the portable decoder gives the core of HE-AAC; the file stays valid
    let portable = Options::default().backend(portable_backend());
    for (name, profile) in [("he1.m4a", "he-aac"), ("he2.m4a", "he-aac-v2")] {
        let r = check(bytes(name), Some("m4a"), &portable).unwrap();
        assert!(r.is_valid());
        assert_eq!(r.profile.as_deref(), Some(profile), "{name}");
        let note = r.issue(IssueKind::ReducedQuality).expect("reduced quality");
        assert!(note.detail.contains("22050 Hz"), "{}", note.detail);
    }
    for name in ["a_itunes.m4a", "b_ffmpeg.m4a"] {
        let r = check(bytes(name), Some("m4a"), &portable).unwrap();
        assert_eq!((r.profile.as_deref(), r.issue(IssueKind::ReducedQuality)), (Some("aac-lc"), None), "{name}");
    }
}

/// HE-AAC through the platform decoder: decoded in full, nothing to say.
#[cfg(target_vendor = "apple")]
#[test]
fn he_aac_through_the_platform_decoder_is_full() {
    // DESIGN D11
    for name in ["he1.m4a", "he2.m4a"] {
        let d = decode(bytes(name), Some("m4a"), &Options::default()).unwrap();
        assert_eq!(d.rate, 44100, "{name}");
        assert_eq!(d.report.issue(IssueKind::ReducedQuality), None, "{name}");
    }
}

fn portable_backend() -> rdks_audio_decode::Backend {
    rdks_audio_decode::Backend::Portable
}

/// Byte offset of ADTS frame `n`.
fn adts_frame(b: &[u8], n: usize) -> usize {
    let mut at = 0;
    for _ in 0..n {
        at += usize::from(b[at + 3] & 3) << 11 | usize::from(b[at + 4]) << 3 | usize::from(b[at + 5] >> 5);
    }
    at
}

#[test]
fn adts_junk_inside_is_a_gap_and_a_cut_is_truncated() {
    // DESIGN D7, D9, D12
    let clean = bytes("adts_lc.aac");
    let opts = Options::default();
    let whole = check(clean.clone(), Some("aac"), &opts).unwrap();
    assert_eq!(kinds(&whole), [IssueKind::NoGaplessInfo]);
    assert!(whole.issues[0].detail.contains("2112 frames"), "{}", whole.issues[0].detail);
    let mut b = clean.clone();
    let at = adts_frame(&b, 10);
    b.splice(at..at, [0x5a_u8; 333]);
    let r = check(b, Some("aac"), &opts).unwrap();
    assert_eq!(kinds(&r), [IssueKind::NoGaplessInfo, IssueKind::StreamGap], "{:?}", r.issues);
    assert_eq!(r.frames, whole.frames);
    let r = check(clean[..clean.len() - 5].to_vec(), Some("aac"), &opts).unwrap();
    assert!(kinds(&r).contains(&IssueKind::Truncated), "{:?}", r.issues);
}

/// The default trim of an HE-AAC ADTS stream is counted at its core rate.
#[cfg(target_vendor = "apple")]
#[test]
fn adts_he_aac_default_trim_is_doubled() {
    // DESIGN D12
    let r = check(bytes("adts_he.aac"), Some("aac"), &Options::default()).unwrap();
    assert_eq!((r.rate, r.trim_start, r.issue(IssueKind::ReducedQuality)), (44100, 4224, None));
}
