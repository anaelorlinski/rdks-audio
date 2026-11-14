//! Helpers of the decode tests: fixtures, a decode to mono, and the lag between a decode and its source.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use rdks_audio_decode::{Backend, Decoded, Options, decode_file};

/// Largest lag searched (frames).
pub const MAX_LAG: i64 = 3000;
/// Frames correlated.
pub const WINDOW: usize = 16384;

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

pub fn decoded(name: &str, backend: Backend) -> Decoded {
    decode_file(fixture(name), &Options::default().backend(backend)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Mono (mean of the two channels) of a decoded file, and its rate.
pub fn mono(name: &str, backend: Backend) -> (Vec<f32>, f64) {
    let d = decoded(name, backend);
    (d.samples.as_chunks::<2>().0.iter().map(|[l, r]| (l + r) * 0.5).collect(), f64::from(d.rate))
}

/// The lag that best aligns `dec` to `src` (positive = `dec` later).
pub fn lag(src: &[f32], dec: &[f32]) -> i64 {
    let n = src.len().min(dec.len()).min(WINDOW);
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
