//! Decode round trip: a small PCM WAV from a synthetic writer decodes to the samples written (stereo, mono
//! duplicated), at its own rate, with nothing to report.

use proptest::prelude::*;
use rdks_audio_decode::{Options, decode};

fn wav(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
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

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn pcm_wav_decodes_to_its_samples(
        rate in prop::sample::select(vec![22050u32, 44100, 48000, 96000]),
        stereo in any::<bool>(),
        frames in proptest::collection::vec(any::<(i16, i16)>(), 1..600),
    ) {
        let ch: u16 = if stereo { 2 } else { 1 };
        let pcm: Vec<i16> = frames.iter().flat_map(|&(l, r)| if stereo { vec![l, r] } else { vec![l] }).collect();
        let t = decode(wav(rate, ch, &pcm), Some("wav"), &Options::default())
            .map_err(|e| TestCaseError::fail(e.to_string()))?;
        prop_assert_eq!(t.rate, rate);
        prop_assert!(t.report.issues.is_empty(), "{:?}", t.report.issues);
        let expect: Vec<f32> = frames
            .iter()
            .flat_map(|&(l, r)| [l, if stereo { r } else { l }])
            .map(|s| s as f32 / 32768.0)
            .collect();
        prop_assert_eq!(t.samples.len(), expect.len());
        for (a, b) in t.samples.iter().zip(&expect) {
            prop_assert!((a - b).abs() <= 1.0 / 32768.0, "{a} vs {b}");
        }
    }
}
