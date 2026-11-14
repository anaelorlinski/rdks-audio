//! The decoder against Symphonia's own AAC decoder and against the lossless source of the fixtures (shared with
//! `rdks-audio-decode`: 0.5 s of pink noise encoded by afconvert and ffmpeg, `src.wav` its source).

#![cfg(target_vendor = "apple")]

use std::path::{Path, PathBuf};

use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::packet::Packet;
use symphonia_adapter_applecoreaudio::{AppleAacDecoder, register};

/// Largest lag searched (frames).
const MAX_LAG: i64 = 8000;
/// Frames correlated.
const WINDOW: usize = 12000;
/// How much further from the source than Symphonia's decoder ours may be (dB): the two differ only where the
/// encoder's tools leave the decoder a choice (noise substitution).
const SOURCE_SNR_SLACK: f64 = 1.0;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rdks-audio-decode/tests/fixtures").join(name)
}

fn open(name: &str) -> Box<dyn FormatReader> {
    let file = std::fs::File::open(fixture(name)).unwrap();
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(name.rsplit('.').next().unwrap());
    symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default()).unwrap()
}

fn params(fmt: &dyn FormatReader) -> AudioCodecParameters {
    fmt.default_track(TrackType::Audio).unwrap().codec_params.as_ref().unwrap().audio().unwrap().clone()
}

fn packets(fmt: &mut dyn FormatReader) -> Vec<Packet> {
    std::iter::from_fn(|| fmt.next_packet().unwrap()).collect()
}

/// A decode: mono (mean of the channels), rate, channels, frames per packet.
struct Out {
    mono: Vec<f32>,
    rate: u32,
    channels: usize,
    per_packet: Vec<usize>,
}

fn run(dec: &mut dyn AudioDecoder, packets: &[Packet]) -> Out {
    let mut out = Out { mono: Vec::new(), rate: 0, channels: 0, per_packet: Vec::new() };
    let mut buf = Vec::new();
    for p in packets {
        let a = dec.decode(p).unwrap();
        out.rate = a.spec().rate();
        out.channels = a.spec().channels().count();
        buf.resize(a.samples_interleaved(), 0.0f32);
        a.copy_to_slice_interleaved(&mut buf);
        out.per_packet.push(buf.len() / out.channels);
        out.mono.extend(buf.chunks_exact(out.channels).map(|f| f.iter().sum::<f32>() / f.len() as f32));
    }
    out
}

fn apple(name: &str) -> Out {
    let mut fmt = open(name);
    let mut dec = AppleAacDecoder::try_new(&params(fmt.as_ref()), &AudioDecoderOptions::default()).unwrap();
    run(&mut dec, &packets(fmt.as_mut()))
}

fn symphonia(name: &str) -> Out {
    let mut fmt = open(name);
    let opts = AudioDecoderOptions::default();
    let mut dec = symphonia::default::get_codecs().make_audio_decoder(&params(fmt.as_ref()), &opts).unwrap();
    run(dec.as_mut(), &packets(fmt.as_mut()))
}

fn source() -> Vec<f32> {
    let mut fmt = open("src.wav");
    let opts = AudioDecoderOptions::default();
    let mut dec = symphonia::default::get_codecs().make_audio_decoder(&params(fmt.as_ref()), &opts).unwrap();
    run(dec.as_mut(), &packets(fmt.as_mut())).mono
}

/// The lag that best aligns `dec` to `src` (positive = `dec` later).
fn lag(src: &[f32], dec: &[f32]) -> i64 {
    let n = src.len().min(WINDOW);
    let corr = |l: i64| -> f64 {
        (0..n)
            .filter_map(|i| {
                let j = i as i64 + l;
                (j >= 0).then(|| dec.get(j as usize).map(|&d| f64::from(src[i] * d))).flatten()
            })
            .sum()
    };
    (-MAX_LAG..=MAX_LAG).max_by(|&a, &b| corr(a).total_cmp(&corr(b))).unwrap()
}

/// Signal to difference ratio in dB of two aligned decodes.
fn snr(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    let s: f64 = a[..n].iter().map(|&x| f64::from(x * x)).sum();
    let d: f64 = a[..n].iter().zip(&b[..n]).map(|(&x, &y)| f64::from((x - y) * (x - y))).sum();
    10.0 * (s / d.max(1e-30)).log10()
}

#[test]
fn aac_lc_matches_symphonias_decoder_frame_for_frame() {
    // DESIGN A3, A4: same rate, channels, frames per packet and timeline as Symphonia's AAC decoder
    for name in ["a_itunes.m4a", "b_ffmpeg.m4a", "c_noedit.m4a"] {
        let (ours, theirs) = (apple(name), symphonia(name));
        assert_eq!((ours.rate, ours.channels), (theirs.rate, theirs.channels), "{name}");
        assert_eq!(ours.per_packet, theirs.per_packet, "{name}");
        assert_eq!(lag(&theirs.mono, &ours.mono), 0, "{name}");
        let db = snr(&theirs.mono, &ours.mono);
        let src = source();
        let at = |d: &[f32]| {
            let l = lag(&src, d).max(0) as usize;
            snr(&src, &d[l..])
        };
        let (ours_db, theirs_db) = (at(&ours.mono), at(&theirs.mono));
        eprintln!(
            "{name}: {} frames, {db:.1} dB against Symphonia's decoder; against the source: ours {ours_db:.1} dB, \
             Symphonia's {theirs_db:.1} dB",
            ours.mono.len(),
        );
        assert!(ours_db > theirs_db - SOURCE_SNR_SLACK, "{name}: {ours_db:.1} against {theirs_db:.1} dB");
    }
}

#[test]
fn delays_against_the_source() {
    // DESIGN A4: untrimmed output, the encoder delay in front of the source; HE-AAC at twice its core rate, its
    // priming doubled, the SBR delay (962 frames) already dropped from the first packet
    let src = source();
    for (name, want_rate, want_channels, want_lag, want_first) in [
        ("a_itunes.m4a", 44100, 2, 2112, 1024),
        ("b_ffmpeg.m4a", 44100, 2, 1024, 1024),
        ("he1.m4a", 44100, 2, 4224, 2048 - 962),
        ("he2.m4a", 44100, 2, 4224, 2048 - 962),
    ] {
        let out = apple(name);
        let l = lag(&src, &out.mono);
        eprintln!(
            "{name}: {} Hz, {} ch, per packet {:?}.., {} frames, source at {l}",
            out.rate,
            out.channels,
            &out.per_packet[..4],
            out.mono.len()
        );
        assert_eq!((out.rate, out.channels), (want_rate, want_channels), "{name}");
        assert_eq!((l, out.per_packet[0]), (want_lag, want_first), "{name}");
        assert!(out.per_packet[1..out.per_packet.len() - 1].iter().all(|&n| n == out.per_packet[1]), "{name}");
    }
}

#[test]
fn a_damaged_packet_is_an_error_and_the_rest_keeps_its_place() {
    // DESIGN A5: AudioToolbox outputs nothing for a damaged packet, without an error status
    for name in ["a_itunes.m4a", "he1.m4a"] {
        let mut fmt = open(name);
        let mut packets = packets(fmt.as_mut());
        let mid = packets.len() / 2;
        let mut data = packets[mid].data.to_vec();
        for (i, b) in data.iter_mut().enumerate().skip(2) {
            *b = (i as u8).wrapping_mul(97) ^ 0xa5;
        }
        packets[mid].data = data.into_boxed_slice();
        let mut dec = AppleAacDecoder::try_new(&params(fmt.as_ref()), &AudioDecoderOptions::default()).unwrap();
        let mut seen = Vec::new();
        for p in &packets {
            match dec.decode(p) {
                Ok(a) => seen.push(Some(a.frames())),
                Err(Error::DecodeError(_)) => seen.push(None),
                Err(e) => panic!("{name}: {e}"),
            }
        }
        eprintln!("{name}: around the damaged packet {mid}: {:?}", &seen[mid - 2..mid + 3]);
        assert_eq!(seen[mid], None, "{name}");
        let full = seen[1].unwrap();
        assert!(seen[mid + 1..].iter().all(|&n| n == Some(full)), "{name}: {:?}", &seen[mid..]);
    }
}

#[test]
fn registered_ahead_of_symphonias_decoder() {
    // DESIGN A1
    let mut codecs = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut codecs);
    register(&mut codecs);
    let fmt = open("he1.m4a");
    let dec = codecs.make_audio_decoder(&params(fmt.as_ref()), &AudioDecoderOptions::default()).unwrap();
    assert!(dec.codec_info().long_name.contains("Apple"));
    assert_eq!(dec.codec_params().sample_rate, Some(44100));
}

#[test]
fn the_decoder_is_send_and_sync() {
    // DESIGN A6
    fn check<T: Send + Sync>() {}
    check::<AppleAacDecoder>();
}

/// An ADTS fixture through the ADTS reader.
fn adts_apple(name: &str) -> Out {
    let data = std::fs::read(fixture(name)).unwrap();
    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(data)), Default::default());
    let mut fmt = symphonia_adapter_applecoreaudio::AdtsReader::try_new(mss, FormatOptions::default()).unwrap();
    let mut dec = AppleAacDecoder::try_new(&params(&fmt), &AudioDecoderOptions::default()).unwrap();
    run(&mut dec, &packets(&mut fmt))
}

#[test]
fn adts_decodes_as_its_mp4_twin() {
    // DESIGN A2, A7: the same encoder's ADTS and MP4 streams decode alike; HE-AAC signalled implicitly (an AAC-LC
    // header at 22.05 kHz) still decodes with SBR
    let src = source();
    for (adts, mp4) in
        [("adts_lc.aac", "a_itunes.m4a"), ("adts_he.aac", "he1.m4a"), ("adts_ffmpeg.aac", "b_ffmpeg.m4a")]
    {
        let (a, m) = (adts_apple(adts), apple(mp4));
        assert_eq!((a.rate, a.channels, a.per_packet[0]), (m.rate, m.channels, m.per_packet[0]), "{adts}");
        assert_eq!(lag(&src, &a.mono), lag(&src, &m.mono), "{adts}");
    }
}

#[test]
fn a_bare_low_rate_config_decodes_with_sbr() {
    // DESIGN A2: AAC-LC at 22.05 kHz is asked for with SBR, the decoder applies it
    let fmt = open("he1.m4a");
    let mut p = params(fmt.as_ref());
    p.extra_data = Some(Box::new([0x13, 0x90]));
    let dec = AppleAacDecoder::try_new(&p, &AudioDecoderOptions::default()).unwrap();
    assert_eq!(dec.codec_params().sample_rate, Some(44100));
}
