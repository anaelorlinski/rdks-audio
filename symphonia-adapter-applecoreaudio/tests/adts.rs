//! The ADTS reader against Symphonia's on the fixtures of `rdks-audio-decode` (raw AAC streams written by afconvert
//! and ffmpeg), and on damaged copies of them (DESIGN A7).

use std::path::Path;

use symphonia::core::errors::Error;
use symphonia::core::formats::probe::{Hint, Probe};
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia_adapter_applecoreaudio::{AdtsReader, register_formats};

const FIXTURES: [&str; 3] = ["adts_lc.aac", "adts_he.aac", "adts_ffmpeg.aac"];
/// Where junk goes in a damaged copy: after this many frames.
const JUNK_AFTER: usize = 10;
/// Bytes of junk inserted (no 0xFF in it: no false header).
const JUNK_LEN: usize = 333;

fn bytes(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../rdks-audio-decode/tests/fixtures").join(name)).unwrap()
}

fn mss(data: Vec<u8>) -> MediaSourceStream<'static> {
    MediaSourceStream::new(Box::new(std::io::Cursor::new(data)), Default::default())
}

fn ours(data: Vec<u8>) -> AdtsReader<'static> {
    AdtsReader::try_new(mss(data), FormatOptions::default()).unwrap()
}

/// Every `next_packet` result until the end: the packet's data and duration, or None for an error.
fn read_all(fmt: &mut dyn FormatReader) -> Vec<Option<(Vec<u8>, u64)>> {
    let mut out = Vec::new();
    loop {
        match fmt.next_packet() {
            Ok(Some(p)) => out.push(Some((p.data.to_vec(), p.dur.get()))),
            Ok(None) => return out,
            Err(Error::DecodeError(_)) => out.push(None),
            Err(e) => panic!("{e}"),
        }
    }
}

/// Byte offset of frame `n`.
fn frame_at(data: &[u8], n: usize) -> usize {
    let mut at = 0;
    for _ in 0..n {
        at += usize::from(data[at + 3] & 3) << 11 | usize::from(data[at + 4]) << 3 | usize::from(data[at + 5] >> 5);
    }
    at
}

#[test]
fn same_packets_as_symphonias_reader() {
    for name in FIXTURES {
        let theirs = {
            let mut hint = Hint::new();
            hint.with_extension("aac");
            let mut fmt = symphonia::default::get_probe()
                .probe(&hint, mss(bytes(name)), FormatOptions::default(), MetadataOptions::default())
                .unwrap();
            read_all(fmt.as_mut())
        };
        let got = read_all(&mut ours(bytes(name)));
        assert!(got.len() > JUNK_AFTER + 2, "{name}: {} packets", got.len());
        assert_eq!(got, theirs, "{name}");
    }
}

#[test]
fn registered_ahead_of_symphonias_reader() {
    let mut probe = Probe::default();
    symphonia::default::register_enabled_formats(&mut probe);
    register_formats(&mut probe);
    let fmt =
        probe.probe(&Hint::new(), mss(bytes("adts_lc.aac")), FormatOptions::default(), MetadataOptions::default());
    assert_eq!(fmt.unwrap().format_info().long_name, "Audio Data Transport Stream");
}

#[test]
fn junk_inside_is_one_error_and_every_frame_is_kept() {
    for name in FIXTURES {
        let clean = bytes(name);
        let at = frame_at(&clean, JUNK_AFTER);
        let mut data = clean[..at].to_vec();
        data.extend((0..JUNK_LEN).map(|i| (i as u8).wrapping_mul(37) & 0x7f));
        data.extend_from_slice(&clean[at..]);
        let got = read_all(&mut ours(data));
        let want = read_all(&mut ours(clean));
        assert_eq!(got[JUNK_AFTER], None, "{name}");
        assert_eq!(got.iter().filter(|p| p.is_none()).count(), 1, "{name}");
        assert_eq!(got.into_iter().flatten().collect::<Vec<_>>(), want.into_iter().flatten().collect::<Vec<_>>());
    }
}

#[test]
fn a_cut_frame_is_an_io_error_and_a_trailing_tag_is_not_audio() {
    let clean = bytes("adts_lc.aac");
    let frames = read_all(&mut ours(clean.clone())).len();
    let mut cut = ours(clean[..clean.len() - 5].to_vec());
    for _ in 0..frames - 1 {
        cut.next_packet().unwrap().unwrap();
    }
    assert!(matches!(cut.next_packet(), Err(Error::IoError(_))));
    // an ID3v1 tag after the last frame
    let mut tagged = clean;
    tagged.extend_from_slice(b"TAG");
    tagged.resize(tagged.len() + 125, b' ');
    assert_eq!(read_all(&mut ours(tagged)).len(), frames);
}
