//! MP3 decoding against the lossless source (DESIGN D4 to D6): each fixture of `tests/fixtures/` (made by
//! `tools/fixtures.py`) starts on the source's first sample and keeps its length when a LAME / ffmpeg extension states
//! the delay and padding, its checksum failing or not; without one the stream decodes untrimmed. Broken ID3 frames do
//! not stop the decode.

mod common;

use common::{decoded, fixture, lag, mono};
use rdks_audio_decode::{Backend, IssueKind, Options, Severity, TrimSource, decode};

/// The delay with nothing stated to remove it: LAME's encoder delay 576 + the decoder's 529.
const RAW_DELAY: i64 = 576 + 529;

#[test]
fn mp3_files_start_on_the_source() {
    // DESIGN D4, D5
    let (src, _) = mono("src.wav", Backend::Platform);
    let (src22, _) = mono("src22.wav", Backend::Platform);
    // (file, source, rate, lag, length = the source's)
    let cases: &[(&str, &[f32], f64, i64, bool)] = &[
        ("mp3_lame_cbr.mp3", &src, 44100.0, 0, true),
        ("mp3_lame_vbr.mp3", &src, 44100.0, 0, true),
        ("mp3_ffmpeg.mp3", &src, 44100.0, 0, true),
        ("mp3_mpeg2.mp3", &src22, 22050.0, 0, true),
        ("mp3_broken_id3.mp3", &src, 44100.0, 0, true),
        ("mp3_bad_checksum.mp3", &src, 44100.0, 0, true),
        // nothing states the delay
        ("mp3_noheader.mp3", &src, 44100.0, RAW_DELAY, false),
        ("mp3_xing_only.mp3", &src, 44100.0, RAW_DELAY, false),
    ];
    for &(name, src, rate, want, exact_len) in cases {
        let (dec, r) = mono(name, Backend::Platform);
        assert_eq!(r, rate, "{name}");
        assert_eq!(lag(src, &dec), want, "{name}: start");
        if exact_len {
            assert_eq!(dec.len(), src.len(), "{name}: length");
        }
    }
}

#[test]
fn clean_files_have_no_issue() {
    // DESIGN D1, D4
    for name in ["mp3_lame_cbr.mp3", "mp3_lame_vbr.mp3", "mp3_ffmpeg.mp3", "mp3_mpeg2.mp3"] {
        let r = decoded(name, Backend::Platform).report;
        assert!(r.issues.is_empty(), "{name}: {:?}", r.issues);
        assert_eq!((r.trim_source, r.trim_start), (TrimSource::Tag, 576 + 529), "{name}");
        assert_eq!(r.codec, "mp3");
    }
}

#[test]
fn statement_problems_are_notes() {
    // DESIGN D5: a failed checksum keeps the values; D4: no extension, no trim
    let r = decoded("mp3_bad_checksum.mp3", Backend::Platform).report;
    assert!(r.is_valid());
    assert!(r.issue(IssueKind::GaplessChecksum).is_some(), "{:?}", r.issues);
    for name in ["mp3_noheader.mp3", "mp3_xing_only.mp3"] {
        let r = decoded(name, Backend::Platform).report;
        assert!(r.is_valid(), "{name}");
        assert_eq!(r.issues.iter().map(|i| i.kind).collect::<Vec<_>>(), [IssueKind::NoGaplessInfo], "{name}");
        assert_eq!(r.trim_source, TrimSource::None);
    }
}

#[test]
fn broken_id3_frames_keep_the_good_ones() {
    // DESIGN D6
    let d = decoded("mp3_broken_id3.mp3", Backend::Platform);
    assert_eq!(d.tags.get("TrackTitle").map(String::as_str), Some("Title"));
    let t = d.report.issue(IssueKind::TagDamaged).expect("tag issue");
    assert_eq!((t.count, t.severity()), (3, Severity::Note), "{}", t.detail);
    assert!(d.report.is_valid());
}

/// MPEG-1 layer III frames of a fixture (no CRC, no free format): each frame's offset and its `main_data_begin`.
fn layer3_frames(b: &[u8]) -> Vec<(usize, usize)> {
    const KBPS: [usize; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
    const RATES: [usize; 3] = [44100, 48000, 32000];
    let mut at = (0..b.len()).find(|&i| b[i] == 0xff && b[i + 1] == 0xfb).unwrap();
    let mut frames = Vec::new();
    while at + 6 <= b.len() && b[at] == 0xff && b[at + 1] == 0xfb {
        let len = 144 * KBPS[usize::from(b[at + 2] >> 4)] * 1000 / RATES[usize::from((b[at + 2] >> 2) & 3)]
            + usize::from((b[at + 2] >> 1) & 1);
        frames.push((at, (usize::from(b[at + 4]) << 1) | usize::from(b[at + 5] >> 7)));
        at += len;
    }
    frames
}

#[test]
fn a_stream_cut_at_its_start_is_damage() {
    // DESIGN D13: from a frame whose main data starts in the frames before it
    let b = std::fs::read(fixture("mp3_noheader.mp3")).unwrap();
    let frames = layer3_frames(&b);
    assert!(frames.len() > 10);
    assert_eq!(frames[0].1, 0, "an encoder starts its stream in the first frame");
    let &(at, _) = frames[3..].iter().find(|f| f.1 > 0).expect("a frame using the reservoir");
    let r = decode(b[at..].to_vec(), Some("mp3"), &Options::default()).unwrap().report;
    let cut = r.issue(IssueKind::Truncated).expect("cut start");
    assert_eq!(cut.severity(), Severity::Damage, "{}", cut.detail);
    assert!(!r.is_valid());
    // from a frame that starts in itself, the stream is whole
    if let Some(&(at, _)) = frames[3..].iter().find(|f| f.1 == 0) {
        let r = decode(b[at..].to_vec(), Some("mp3"), &Options::default()).unwrap().report;
        assert!(r.is_valid(), "{:?}", r.issues);
    }
}
