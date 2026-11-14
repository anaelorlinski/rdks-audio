//! What an MP4 / M4A file states about its sound track's timing: the codec, the timescales, the first non-empty
//! edit of `moov/trak/edts/elst`, whether the sample table has a `roll` sample group (`sgpd`), and the iTunes
//! `iTunSMPB` item of `moov/udta/meta/ilst`, and an AAC track's `AudioSpecificConfig` (`stsd/mp4a/esds`). Facts only; the rule that turns them into a trim is in [`crate::trim`].
//! Every size and offset in the file is untrusted: the walker never indexes past a box and never overflows.

use crate::aac::{self, AacConfig};

/// The sound track's timing statements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mp4Audio {
    /// Sample entry of the track's `stsd` (`mp4a` = AAC, `alac`, ...); zeros when absent.
    pub codec: [u8; 4],
    /// `mvhd` timescale (edit durations are in it).
    pub movie_scale: u32,
    /// `mdhd` timescale (edit media times are in it).
    pub media_scale: u32,
    /// The first edit that plays media.
    pub edit: Option<Edit>,
    /// A `roll` sample group description in the track's `stbl` (ffmpeg writes one next to its edit list).
    pub roll: bool,
    /// The iTunes gapless statement.
    pub smpb: Option<Smpb>,
    /// An AAC track's declared configuration (profile, core rate).
    pub aac: Option<AacConfig>,
}

/// The first edit with a media time (an empty edit has -1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edit {
    /// Where playback starts, in the media timescale.
    pub media_time: u64,
    /// How long it plays, in the movie timescale.
    pub duration: u64,
}

/// `iTunSMPB` = " 00000000 PPPPPPPP RRRRRRRR VVVVVVVVVVVVVVVV ...": priming, remainder (end padding) and valid samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Smpb {
    /// Encoder delay: frames to drop at the start.
    pub priming: u64,
    /// Frames of padding after the last valid one.
    pub remainder: u64,
    /// Frames of audio between them.
    pub valid: u64,
}

impl Mp4Audio {
    /// The track is AAC (`mp4a`).
    pub fn is_aac(&self) -> bool {
        &self.codec == b"mp4a"
    }
}

/// The sound track's statements, None when the file has no `moov` or no sound track.
pub fn audio(file: &[u8]) -> Option<Mp4Audio> {
    let moov = child(file, b"moov")?;
    let movie_scale = path(moov, &[b"mvhd"]).and_then(timescale).unwrap_or(0);
    let trak = boxes(moov).into_iter().find(|(k, t)| {
        k == b"trak" && path(t, &[b"mdia", b"hdlr"]).and_then(|h| h.get(8..12)) == Some(b"soun".as_slice())
    })?;
    let trak = trak.1;
    let stbl = path(trak, &[b"mdia", b"minf", b"stbl"]);
    Some(Mp4Audio {
        codec: stbl.and_then(sample_entry).unwrap_or_default(),
        movie_scale,
        media_scale: path(trak, &[b"mdia", b"mdhd"]).and_then(timescale).unwrap_or(0),
        edit: path(trak, &[b"edts", b"elst"]).and_then(edit),
        roll: stbl
            .is_some_and(|s| boxes(s).iter().any(|(k, p)| k == b"sgpd" && p.get(4..8) == Some(b"roll".as_slice()))),
        smpb: itunsmpb(moov),
        aac: stbl.and_then(aac_config),
    })
}

/// A file whose last top-level box (the sample data) runs past the end of the file, after the `moov` that indexes
/// it: the same bytes with that box ending at the end of the file, so a reader decodes the frames that are there.
/// None when the file is whole, or when the `moov` itself is missing (nothing indexes the samples).
pub fn cut_short(file: &[u8]) -> Option<Vec<u8>> {
    let (mut i, mut moov) = (0usize, false);
    while let Some(h) = file.get(i..i + 8) {
        let size = u64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]]));
        let kind = [h[4], h[5], h[6], h[7]];
        let len = match size {
            0 => return None,
            1 => be64(file, i + 8)?,
            s => s,
        };
        let rest = (file.len() - i) as u64;
        if len > rest {
            if !moov || &kind != b"mdat" {
                return None;
            }
            let mut fixed = file.to_vec();
            if size == 1 {
                fixed[i + 8..i + 16].copy_from_slice(&rest.to_be_bytes());
            } else {
                // size 0: the box runs to the end of the file
                fixed[i..i + 4].copy_from_slice(&0u32.to_be_bytes());
            }
            return Some(fixed);
        }
        moov |= &kind == b"moov";
        i += usize::try_from(len).ok().filter(|&l| l >= 8)?;
    }
    None
}

/// Child boxes of `b` as (type, payload); size 0 = to the end, size 1 = 64-bit size. A box that overruns ends the
/// list.
fn boxes(b: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(h) = b.get(i..i + 8) {
        let size = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as u64;
        let kind = [h[4], h[5], h[6], h[7]];
        let (hdr, len) = match size {
            0 => (8, (b.len() - i) as u64),
            1 => match b.get(i + 8..i + 16) {
                Some(x) => (16, u64::from_be_bytes([x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7]])),
                None => break,
            },
            s => (8, s),
        };
        let Some(end) = (i as u64).checked_add(len).filter(|&e| len >= hdr && e <= b.len() as u64) else { break };
        let end = end as usize;
        out.push((kind, &b[i + hdr as usize..end]));
        i = end;
    }
    out
}

fn child<'a>(b: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(b).into_iter().find(|(k, _)| k == kind).map(|(_, p)| p)
}

fn path<'a>(b: &'a [u8], kinds: &[&[u8; 4]]) -> Option<&'a [u8]> {
    kinds.iter().try_fold(b, |b, k| child(b, k))
}

fn be32(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o.checked_add(4)?).map(|x| u32::from_be_bytes([x[0], x[1], x[2], x[3]]))
}

fn be64(b: &[u8], o: usize) -> Option<u64> {
    Some((be32(b, o)? as u64) << 32 | be32(b, o + 4)? as u64)
}

/// The timescale of an `mvhd` / `mdhd` (version 1: 64-bit times before it).
fn timescale(b: &[u8]) -> Option<u32> {
    if b.first() == Some(&1) { be32(b, 20) } else { be32(b, 12) }
}

/// The first sample entry's type in `stsd` (version / flags and the entry count, then the entries as boxes).
fn sample_entry(stbl: &[u8]) -> Option<[u8; 4]> {
    let stsd = child(stbl, b"stsd")?;
    boxes(stsd.get(8..)?).first().map(|(k, _)| *k)
}

/// The `AudioSpecificConfig` of an `mp4a` sample entry: its `esds` (or QuickTime's `wave/esds`) after the sound
/// sample entry's fields (28 bytes; QuickTime versions 1 and 2 add 16 and 36).
fn aac_config(stbl: &[u8]) -> Option<AacConfig> {
    let stsd = child(stbl, b"stsd")?;
    let (kind, entry) = boxes(stsd.get(8..)?).into_iter().next()?;
    if &kind != b"mp4a" {
        return None;
    }
    let version = entry.get(8..10).map(|v| u16::from_be_bytes([v[0], v[1]]))?;
    let fields = 28
        + match version {
            1 => 16,
            2 => 36,
            _ => 0,
        };
    let children = entry.get(fields..)?;
    let esds = child(children, b"esds").or_else(|| path(children, &[b"wave", b"esds"]))?;
    aac::parse(decoder_specific_info(esds.get(4..)?)?)
}

/// One MPEG-4 descriptor: its tag and body (the size in 7-bit groups, at most four).
fn descriptor(b: &[u8]) -> Option<(u8, &[u8])> {
    let tag = *b.first()?;
    let (mut len, mut i) = (0usize, 1usize);
    loop {
        let x = *b.get(i)?;
        len = (len << 7) | usize::from(x & 0x7f);
        i += 1;
        if x & 0x80 == 0 {
            break;
        }
        if i > 4 {
            return None;
        }
    }
    Some((tag, b.get(i..i.checked_add(len)?)?))
}

/// The decoder specific info of an `esds`: ES descriptor (3), decoder config descriptor (4) after its 13 fixed
/// bytes, decoder specific info (5).
fn decoder_specific_info(esds: &[u8]) -> Option<&[u8]> {
    const ES: u8 = 3;
    const DECODER_CONFIG: u8 = 4;
    const DECODER_SPECIFIC: u8 = 5;
    let (ES, es) = descriptor(esds)? else { return None };
    let flags = *es.get(2)?;
    let mut at = 3usize;
    if flags & 0x80 != 0 {
        at += 2; // depends on ES id
    }
    if flags & 0x40 != 0 {
        at += 1 + usize::from(*es.get(at)?); // URL
    }
    if flags & 0x20 != 0 {
        at += 2; // OCR ES id
    }
    let (DECODER_CONFIG, config) = descriptor(es.get(at..)?)? else { return None };
    let (DECODER_SPECIFIC, info) = descriptor(config.get(13..)?)? else { return None };
    Some(info)
}

/// The first edit with a media time; entries are (duration, media time, rate), 32-bit or 64-bit (version 1).
fn edit(elst: &[u8]) -> Option<Edit> {
    let v1 = elst.first() == Some(&1);
    let entry = if v1 { 20usize } else { 12 };
    for n in 0..be32(elst, 4)? as usize {
        let o = n.checked_mul(entry)?.checked_add(8)?;
        let (duration, time) = if v1 {
            (be64(elst, o)?, be64(elst, o + 8)? as i64)
        } else {
            (be32(elst, o)? as u64, be32(elst, o + 4)? as i32 as i64)
        };
        if time >= 0 {
            return Some(Edit { media_time: time as u64, duration });
        }
    }
    None
}

/// The iTunes `----` item named `iTunSMPB` (mean `com.apple.iTunes`). The QuickTime `keys` / `mdta` form that ffmpeg
/// can write is not this item; AudioToolbox does not read it either.
fn itunsmpb(moov: &[u8]) -> Option<Smpb> {
    let meta = path(moov, &[b"udta", b"meta"])?;
    // `meta` is a full box in MP4 (version and flags first), a plain container in QuickTime files
    let meta = if meta.get(4..8) == Some(b"hdlr") { meta } else { meta.get(4..)? };
    for (k, item) in boxes(child(meta, b"ilst")?) {
        if &k != b"----" {
            continue;
        }
        let named = child(item, b"name").and_then(|n| n.get(4..)) == Some(b"iTunSMPB".as_slice());
        // `data`: type (4) and locale (4), then the text
        let text = child(item, b"data").and_then(|d| d.get(8..)).and_then(|t| std::str::from_utf8(t).ok());
        if let (true, Some(t)) = (named, text) {
            let mut f = t.split_whitespace().skip(1).map(|x| u64::from_str_radix(x, 16).ok());
            return Some(Smpb { priming: f.next()??, remainder: f.next()??, valid: f.next()?? });
        }
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
        b.extend(kind);
        b.extend(payload);
        b
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    fn full_scale(scale: u32, v1: bool) -> Vec<u8> {
        let mut b = vec![u8::from(v1), 0, 0, 0];
        if v1 {
            b.extend([0u8; 16]);
            b.extend(scale.to_be_bytes());
            b.extend([0u8; 8]);
        } else {
            b.extend([0u8; 8]);
            b.extend(scale.to_be_bytes());
            b.extend([0u8; 4]);
        }
        b
    }

    /// An `mvhd` with a timescale.
    pub fn mvhd(scale: u32) -> Vec<u8> {
        bx(b"mvhd", &full_scale(scale, false))
    }

    /// A sound `trak`: `mdhd` timescale, edit list of (duration, media time) entries, a `roll` group or not, codec.
    pub fn trak(scale: u32, edits: &[(u64, i64)], v1: bool, roll: bool, codec: &[u8; 4]) -> Vec<u8> {
        let mut hdlr = vec![0u8; 8];
        hdlr.extend(b"soun");
        hdlr.extend([0u8; 12]);
        let mut elst = vec![u8::from(v1), 0, 0, 0];
        elst.extend((edits.len() as u32).to_be_bytes());
        for &(d, t) in edits {
            if v1 {
                elst.extend(d.to_be_bytes());
                elst.extend(t.to_be_bytes());
            } else {
                elst.extend((d as u32).to_be_bytes());
                elst.extend((t as i32).to_be_bytes());
            }
            elst.extend([0, 1, 0, 0]);
        }
        let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd.extend(bx(codec, &[0u8; 28]));
        let mut stbl = bx(b"stsd", &stsd);
        if roll {
            stbl.extend(bx(b"sgpd", &[1, 0, 0, 0, b'r', b'o', b'l', b'l', 0, 0, 0, 2, 0, 0, 0, 1, 0xff, 0xff]));
        }
        let mdia = cat(&[bx(b"mdhd", &full_scale(scale, v1)), bx(b"hdlr", &hdlr), bx(b"minf", &bx(b"stbl", &stbl))]);
        let edts = if edits.is_empty() { vec![] } else { bx(b"edts", &bx(b"elst", &elst)) };
        bx(b"trak", &cat(&[edts, bx(b"mdia", &mdia)]))
    }

    pub fn smpb(text: &str) -> Vec<u8> {
        let mut name = vec![0u8; 4];
        name.extend(b"iTunSMPB");
        let mut data = vec![0, 0, 0, 1, 0, 0, 0, 0];
        data.extend(text.as_bytes());
        let item =
            bx(b"----", &cat(&[bx(b"mean", b"\0\0\0\0com.apple.iTunes"), bx(b"name", &name), bx(b"data", &data)]));
        let mut meta = vec![0u8; 4];
        meta.extend(bx(b"hdlr", &[0u8; 24]));
        meta.extend(bx(b"ilst", &item));
        bx(b"udta", &bx(b"meta", &meta))
    }

    pub fn file(moov: &[Vec<u8>]) -> Vec<u8> {
        cat(&[bx(b"ftyp", b"M4A \0\0\0\0"), bx(b"moov", &cat(moov)), bx(b"mdat", &[1, 2, 3])])
    }

    #[test]
    fn reads_every_statement() {
        let f = file(&[
            mvhd(1000),
            trak(44100, &[(500, -1), (8000, 1024)], false, true, b"mp4a"),
            smpb(" 00000000 00000840 000001CA 0000000000BFA4A6 00000000"),
        ]);
        let a = audio(&f).expect("sound track");
        assert!(a.is_aac() && a.roll);
        assert_eq!((a.movie_scale, a.media_scale), (1000, 44100));
        assert_eq!(a.edit, Some(Edit { media_time: 1024, duration: 8000 }));
        assert_eq!(a.smpb, Some(Smpb { priming: 0x840, remainder: 0x1CA, valid: 0xBFA4A6 }));
        // 64-bit edits, no roll group, ALAC, no tag
        let a = audio(&file(&[mvhd(44100), trak(48000, &[(9, 2048)], true, false, b"alac")])).unwrap();
        assert!(!a.is_aac() && !a.roll && a.smpb.is_none());
        assert_eq!((a.media_scale, a.edit), (48000, Some(Edit { media_time: 2048, duration: 9 })));
    }

    #[test]
    fn missing_or_broken_statements_are_none() {
        assert_eq!(audio(b"not an mp4"), None);
        assert_eq!(audio(&file(&[])), None);
        let a = audio(&file(&[trak(44100, &[], false, false, b"mp4a")])).unwrap();
        assert_eq!((a.edit, a.smpb, a.movie_scale), (None, None, 0));
        // only an empty edit
        assert_eq!(audio(&file(&[trak(44100, &[(5, -1)], false, false, b"mp4a")])).unwrap().edit, None);
        // a tag with too few fields
        assert_eq!(audio(&file(&[trak(44100, &[], false, false, b"mp4a"), smpb(" 0 840")])).unwrap().smpb, None);
    }

    #[test]
    fn a_cut_sample_box_is_closed_at_the_end_of_the_file() {
        let whole = file(&[mvhd(1000), trak(44100, &[], false, false, b"mp4a")]);
        assert_eq!(cut_short(&whole), None);
        let mut cut = whole.clone();
        let mdat = cut.len() - 11;
        cut[mdat..mdat + 4].copy_from_slice(&1000u32.to_be_bytes());
        let fixed = cut_short(&cut).expect("repaired");
        assert_eq!(&fixed[mdat..mdat + 4], &[0, 0, 0, 0]);
        assert_eq!(audio(&fixed), audio(&whole));
        // the index after the cut: nothing to repair
        let mut late = bx(b"ftyp", b"M4A \0\0\0\0");
        late.extend(bx(b"mdat", &[1, 2, 3]));
        let n = late.len();
        late[n - 11..n - 7].copy_from_slice(&1000u32.to_be_bytes());
        assert_eq!(cut_short(&late), None);
    }

    #[test]
    fn hostile_sizes_end_the_walk() {
        let mut b = 1u32.to_be_bytes().to_vec();
        b.extend(b"moov");
        b.extend(u64::MAX.to_be_bytes());
        assert_eq!(audio(&b), None);
        let mut elst_count = file(&[trak(44100, &[(9, 1024)], false, false, b"mp4a")]);
        // entry count 0xffffffff with one entry present: stops at the end of the box
        let at = elst_count.windows(4).position(|w| w == b"elst").map(|i| i + 8).expect("elst");
        elst_count[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(audio(&elst_count).unwrap().edit, Some(Edit { media_time: 1024, duration: 9 }));
    }
}
