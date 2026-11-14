//! MP4 / M4A decoding against the lossless source (DESIGN D2, D3): each fixture of `tests/fixtures/` (made by
//! `tools/fixtures.py`) starts where AudioToolbox starts it, through both backends. On Apple platforms `Platform` is
//! the system's AAC decoder and `Portable` symphonia's, both trimmed by `trim::aac`, so the test also checks that the
//! two agree; elsewhere both are symphonia's.

mod common;

use common::{lag, mono};
use rdks_audio_decode::Backend;

/// (file, lag against the source, decoded length = the source's); stream priming 2112 (afconvert) or 1024 (ffmpeg).
const CASES: &[(&str, i64, bool)] = &[
    // iTunSMPB, no edit list: iTunes / Apple Music files
    ("a_itunes.m4a", 0, true),
    // edit list + roll group: ffmpeg
    ("b_ffmpeg.m4a", 0, true),
    // nothing stated: 2112 dropped, the end kept
    ("c_noedit.m4a", -1088, false),
    // the tag says 1024: AudioToolbox believes it
    ("g_tag1024.m4a", 1088, true),
    // an edit list without a roll group is ignored: the tag, else 2112
    ("h_edit_tag.m4a", 0, true),
    ("i_edit.m4a", 0, false),
    // with a roll group the edit list wins, even over the tag
    ("j_edit_roll.m4a", 1088, true),
    ("k_edit_roll_tag.m4a", 1088, true),
    ("alac.m4a", 0, true),
    // raw ADTS: nothing stated, as c_noedit (the end kept)
    ("adts_lc.aac", 0, false),
    ("adts_ffmpeg.aac", -1088, false),
];

#[test]
fn mp4_files_start_where_audiotoolbox_starts_them() {
    // DESIGN D2, D3
    let (src, _) = mono("src.wav", Backend::Platform);
    for backend in [Backend::Platform, Backend::Portable] {
        for &(name, want, exact_len) in CASES {
            let (dec, rate) = mono(name, backend);
            assert_eq!(rate, 44100.0, "{name} {backend:?}");
            assert_eq!(lag(&src, &dec), want, "{name} {backend:?}: start");
            if exact_len {
                assert_eq!(dec.len(), src.len(), "{name} {backend:?}: length");
            }
        }
    }
}

#[cfg(target_vendor = "apple")]
#[test]
fn the_portable_path_has_audiotoolbox_s_length_everywhere() {
    // DESIGN D3
    for &(name, _, _) in CASES {
        let (at, _) = mono(name, Backend::Platform);
        let (portable, _) = mono(name, Backend::Portable);
        assert_eq!(portable.len(), at.len(), "{name}");
    }
}

/// HE-AAC v1 and v2 decode in full (SBR, and PS for v2) at the output rate, with lag 0.
#[cfg(target_vendor = "apple")]
#[test]
fn he_aac_decodes_in_full() {
    // DESIGN D2
    let (src, _) = mono("src.wav", Backend::Platform);
    for name in ["he1.m4a", "he2.m4a"] {
        let (dec, rate) = mono(name, Backend::Platform);
        assert_eq!(rate, 44100.0, "{name}");
        assert_eq!((lag(&src, &dec), dec.len()), (0, src.len()), "{name}");
    }
    // ADTS signals SBR implicitly; the default trim at the core rate, doubled (DESIGN D12)
    let (dec, rate) = mono("adts_he.aac", Backend::Platform);
    assert_eq!((rate, lag(&src, &dec)), (44100.0, 0));
}
