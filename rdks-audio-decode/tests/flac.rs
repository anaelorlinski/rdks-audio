//! FLAC against its lossless source, and the tags some taggers write after a FLAC stream (DESIGN D14): left out of
//! the decode with a note, every sample kept; a really cut last frame stays damage.

mod common;

use common::{decoded, fixture};
use rdks_audio_decode::{IssueKind, Options, Position, Severity, decode};

/// An ID3v1 tag: `TAG` and 125 bytes of fields.
fn id3v1() -> Vec<u8> {
    let mut t = b"TAG".to_vec();
    t.extend(b"Title");
    t.resize(128, 0);
    t
}

/// An APEv2 tag with a header, one item and a footer.
fn apev2() -> Vec<u8> {
    let mut item = 5u32.to_le_bytes().to_vec();
    item.extend([0; 4]);
    item.extend(b"Title\0Hello");
    let size = (item.len() + 32) as u32;
    let block = |flags: u32| {
        let mut b = b"APETAGEX".to_vec();
        b.extend(2000u32.to_le_bytes());
        b.extend(size.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b.extend(flags.to_le_bytes());
        b.extend([0; 8]);
        b
    };
    let mut t = block((1 << 31) | (1 << 29));
    t.extend(item);
    t.extend(block(1 << 31));
    t
}

fn flac_with(tail: &[u8]) -> Vec<u8> {
    let mut b = std::fs::read(fixture("flac.flac")).unwrap();
    b.extend(tail);
    b
}

#[test]
fn flac_is_its_source() {
    let src = decoded("src.wav", rdks_audio_decode::Backend::Platform).samples;
    let d = decoded("flac.flac", rdks_audio_decode::Backend::Platform);
    assert_eq!(d.samples, src);
    assert!(d.report.issues.is_empty(), "{:?}", d.report.issues);
}

#[test]
fn tags_after_the_stream_are_a_note_and_every_sample_is_kept() {
    // DESIGN D14
    let src = decoded("src.wav", rdks_audio_decode::Backend::Platform).samples;
    let audio_len = std::fs::read(fixture("flac.flac")).unwrap().len() as u64;
    for (name, tail) in [("ID3v1", id3v1()), ("APEv2", apev2()), ("APEv2 + ID3v1", [apev2(), id3v1()].concat())] {
        let d = decode(flac_with(&tail), Some("flac"), &Options::default()).unwrap();
        assert_eq!(d.samples, src, "{name}");
        assert!(d.report.is_valid(), "{name}: {:?}", d.report.issues);
        let i = d.report.issue(IssueKind::JunkOutsideAudio).expect(name);
        assert_eq!((i.severity(), i.at), (Severity::Note, Some(Position::Byte(audio_len))), "{name}: {}", i.detail);
    }
    // found by the stream marker too, without an extension
    let d = decode(flac_with(&id3v1()), None, &Options::default()).unwrap();
    assert_eq!(d.samples, src);
}

#[test]
fn a_cut_last_frame_before_a_tag_is_still_damage() {
    // DESIGN D7, D14
    let mut b = std::fs::read(fixture("flac.flac")).unwrap();
    b.truncate(b.len() - 100);
    b.extend(id3v1());
    let d = decode(b, Some("flac"), &Options::default()).unwrap();
    assert!(!d.report.is_valid(), "{:?}", d.report.issues);
    assert!(d.report.issue(IssueKind::Truncated).is_some(), "{:?}", d.report.issues);
}
