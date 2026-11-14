//! Mutation fuzzing of the readers: small WAV files from a synthetic writer and the MP3 / M4A fixtures through the
//! whole decoder and the checker, MP4 box trees through the MP4 statement reader and the trim rule, MP3 files through
//! the stream walker. The only property is "never panics" (DESIGN D10); a failure names the reader, seed and
//! iteration.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};

use rdks_audio_decode::{Backend, Options, check, decode, mp4, mpeg, trim};

const ROUNDS: usize = 200;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

fn mutate(seed: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut b = seed.to_vec();
    for _ in 0..1 + rng.below(3) {
        let len = b.len();
        match rng.below(6) {
            0 if len > 0 => {
                let i = rng.below(len);
                b[i] ^= 1 << rng.below(8);
            }
            1 if len > 0 => {
                let i = rng.below(len);
                b[i] = rng.next() as u8;
            }
            2 if len >= 4 => {
                let i = rng.below(len - 3);
                let v = [0u32, 1, u32::MAX, 0x8000_0000, 0x7fff_ffff][rng.below(5)];
                let bytes = if rng.below(2) == 0 { v.to_be_bytes() } else { v.to_le_bytes() };
                b[i..i + 4].copy_from_slice(&bytes);
            }
            3 => b.truncate(rng.below(len + 1)),
            4 if len > 0 => {
                let i = rng.below(len);
                let n = 1 + rng.below((len - i).min(32));
                b.drain(i..i + n);
            }
            _ => b.push(rng.next() as u8),
        }
    }
    b
}

fn fuzz(name: &str, seeds: &[Vec<u8>], read: impl Fn(&[u8])) {
    let mut fails = Vec::new();
    for (s, seed) in seeds.iter().enumerate() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ (s as u64 + 1) ^ (name.len() as u64) << 32);
        for it in 0..ROUNDS {
            let m = mutate(seed, &mut rng);
            if let Err(e) = catch_unwind(AssertUnwindSafe(|| read(&m))) {
                let msg =
                    e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()));
                fails.push(format!("{name} seed {s} iteration {it}: {}", msg.unwrap_or_default()));
            }
        }
    }
    assert!(fails.is_empty(), "{} panics:\n{}", fails.len(), fails[..fails.len().min(20)].join("\n"));
}

/// A PCM WAV file: `channels` x 16-bit at `rate`.
pub fn wav(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut b = b"RIFF".to_vec();
    b.extend((36 + data.len() as u32).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(channels.to_le_bytes());
    b.extend(rate.to_le_bytes());
    b.extend((rate * channels as u32 * 2).to_le_bytes());
    b.extend((channels * 2).to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(b"data");
    b.extend((data.len() as u32).to_le_bytes());
    b.extend(data);
    b
}

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut b = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
    b.extend(kind);
    b.extend(payload);
    b
}

/// An `ftyp` / `moov` (sound track with an edit list, an `iTunSMPB` item) / `mdat` tree.
fn mp4_tree() -> Vec<u8> {
    let mut mdhd = vec![0u8; 12];
    mdhd.extend(44100u32.to_be_bytes());
    mdhd.extend([0u8; 4]);
    let mut hdlr = vec![0u8; 8];
    hdlr.extend(b"soun");
    hdlr.extend([0u8; 12]);
    let mut elst = vec![0, 0, 0, 0, 0, 0, 0, 2];
    for t in [-1i32, 1024] {
        elst.extend(1000u32.to_be_bytes());
        elst.extend(t.to_be_bytes());
        elst.extend([0, 1, 0, 0]);
    }
    let mdia = [bx(b"mdhd", &mdhd), bx(b"hdlr", &hdlr)].concat();
    let trak = bx(b"trak", &[bx(b"edts", &bx(b"elst", &elst)), bx(b"mdia", &mdia)].concat());
    let mut name = vec![0u8; 4];
    name.extend(b"iTunSMPB");
    let mut data = vec![0, 0, 0, 1, 0, 0, 0, 0];
    data.extend(b" 00000000 00000840 000001CA 0000000000BFA4A6");
    let item =
        bx(b"----", &[bx(b"mean", b"\0\0\0\0com.apple.iTunes"), bx(b"name", &name), bx(b"data", &data)].concat());
    let mut meta = vec![0u8; 4];
    meta.extend(bx(b"hdlr", &[0u8; 24]));
    meta.extend(bx(b"ilst", &item));
    let moov = bx(b"moov", &[trak, bx(b"udta", &bx(b"meta", &meta))].concat());
    [bx(b"ftyp", b"M4A \0\0\0\0"), moov, bx(b"mdat", &[1, 2, 3])].concat()
}

#[test]
fn wav_decoder_never_panics() {
    let tone: Vec<i16> = (0..2000).map(|i| ((i as f32 * 0.05).sin() * 12000.0) as i16).collect();
    fuzz("wav", &[wav(44100, 2, &tone), wav(48000, 1, &tone[..500])], |b| {
        let _ = decode(b.to_vec(), Some("wav"), &Options::default());
    });
}

#[test]
fn mp4_statement_reader_never_panics() {
    fuzz("mp4", &[mp4_tree()], |b| {
        let a = mp4::audio(b);
        for rate in [0.0, 44100.0, f64::MAX] {
            let _ = trim::aac(a.as_ref(), rate);
        }
        let _ = mp4::cut_short(b);
    });
}

#[test]
fn mp3_walker_and_checker_never_panic() {
    let seeds: Vec<Vec<u8>> =
        ["mp3_broken_id3.mp3", "mp3_lame_vbr.mp3"].iter().map(|n| std::fs::read(common::fixture(n)).unwrap()).collect();
    fuzz("mp3", &seeds, |b| {
        let _ = mpeg::read(b);
        let _ = check(b.to_vec(), Some("mp3"), &Options::default());
    });
}

#[test]
fn m4a_checker_never_panics() {
    let seed = std::fs::read(common::fixture("a_itunes.m4a")).unwrap();
    fuzz("m4a", &[seed], |b| {
        let _ = check(b.to_vec(), Some("m4a"), &Options::default().backend(Backend::Portable));
    });
}
