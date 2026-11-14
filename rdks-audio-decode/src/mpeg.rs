//! What an MPEG audio file (MP3, MP2, MP1) states and how its stream is laid out, read without decoding: the
//! ID3v2 tags at the start (checked frame by frame), the frames (counted, with every byte range between them that is
//! not a frame), the header frame (Xing / Info / VBRI) with its LAME / ffmpeg extension and that extension's checksum,
//! and the tags at the end (ID3v1, APEv2, Lyrics3, a trailing ID3v2). Every size in the file is untrusted: the walk
//! never indexes past the data and never overflows.

use crate::report::{IssueKind, Issues, Position};

/// The header frame that opens a stream: silent, never audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderFrame {
    /// `Xing`: a VBR stream's frame count and seek table.
    Xing,
    /// `Info`: the same for a CBR stream.
    Info,
    /// `VBRI`: the Fraunhofer encoder's header.
    Vbri,
}

/// The LAME extension of a Xing / Info header (LAME, or ffmpeg's `Lavc` / `Lavf`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LameExtension {
    /// The first four bytes of the encoder's name (`LAME`, `Lavc`, `Lavf`).
    pub encoder: [u8; 4],
    /// Encoder delay in frames (the decoder's 529 not included).
    pub delay: u16,
    /// Padding frames at the end (the decoder's 529 included).
    pub padding: u16,
    /// The extension's own checksum matches its bytes.
    pub checksum_ok: bool,
}

/// What [`read`] found in an MPEG audio file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mp3Statements {
    /// MPEG version: 1, 2, or 25 for MPEG-2.5.
    pub version: u8,
    /// Layer: 1, 2 or 3.
    pub layer: u8,
    /// Sample rate of the first frame.
    pub rate: u32,
    /// Channels of the first frame.
    pub channels: u16,
    /// Frames of audio per MPEG frame (384, 576 or 1152).
    pub samples_per_frame: u32,
    /// Byte offset of the first MPEG frame.
    pub first_frame_at: u64,
    /// The header frame, when the stream opens with one.
    pub header_frame: Option<HeaderFrame>,
    /// MPEG frames the header frame states (its own not counted).
    pub stated_frames: Option<u32>,
    /// The LAME extension, when the header frame has one from an encoder that writes it reliably.
    pub lame: Option<LameExtension>,
    /// MPEG frames of audio found by the walk (the header frame not counted, a cut last frame not counted).
    pub audio_frames: u64,
    pub(crate) issues: Issues,
}

/// The rule's input bounds: no encoder writes a delay or padding longer than this many MPEG frames.
const MAX_GAPLESS_FRAMES: u64 = 4;

impl Mp3Statements {
    /// The encoder delay and padding to trim, when the LAME extension is usable: present, from a known encoder, and
    /// plausible against the stream's length (a failed checksum alone does not reject it).
    pub fn gapless(&self) -> Option<(u64, u64)> {
        let lame = self.lame?;
        let (delay, padding) = (u64::from(lame.delay), u64::from(lame.padding));
        let spf = u64::from(self.samples_per_frame);
        let total = self.audio_frames * spf;
        let plausible = delay <= MAX_GAPLESS_FRAMES * spf
            && padding <= MAX_GAPLESS_FRAMES * spf
            && delay + padding + crate::trim::MP3_DECODER_DELAY < total;
        plausible.then_some((delay, padding))
    }

    /// The header frame's count matches the walk, so the stated padding applies to the stream's real end.
    pub fn end_is_stated(&self) -> bool {
        self.stated_frames.is_none_or(|n| u64::from(n) == self.audio_frames)
    }
}

/// An MPEG audio frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    /// 1, 2, or 25.
    version: u8,
    layer: u8,
    crc: bool,
    /// kbit/s; 0 = free format.
    bitrate: u32,
    rate: u32,
    padding: bool,
    mono: bool,
}

const RATES: [u32; 3] = [44100, 48000, 32000];
const BITRATES_V1: [[u32; 15]; 3] = [
    [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448],
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],
    [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
];
const BITRATES_V2: [[u32; 15]; 2] = [
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256],
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
];

impl Header {
    fn parse(b: &[u8]) -> Option<Header> {
        let w = u32::from_be_bytes(b.get(..4)?.try_into().ok()?);
        if w >> 21 != 0x7ff {
            return None;
        }
        let version = match (w >> 19) & 3 {
            0 => 25,
            2 => 2,
            3 => 1,
            _ => return None,
        };
        let layer = match (w >> 17) & 3 {
            1 => 3,
            2 => 2,
            3 => 1,
            _ => return None,
        };
        let br = ((w >> 12) & 0xf) as usize;
        let ri = ((w >> 10) & 3) as usize;
        if br == 15 || ri == 3 {
            return None;
        }
        let bitrate = if version == 1 {
            BITRATES_V1[usize::from(layer - 1)][br]
        } else {
            BITRATES_V2[usize::from(layer != 1)][br]
        };
        let rate = RATES[ri]
            / if version == 1 {
                1
            } else if version == 2 {
                2
            } else {
                4
            };
        Some(Header {
            version,
            layer,
            crc: (w >> 16) & 1 == 0,
            bitrate,
            rate,
            padding: (w >> 9) & 1 == 1,
            mono: (w >> 6) & 3 == 3,
        })
    }

    fn samples(&self) -> u32 {
        match (self.layer, self.version) {
            (1, _) => 384,
            (3, 2 | 25) => 576,
            _ => 1152,
        }
    }

    /// Bytes in the frame; None for free format (no bitrate in the header).
    fn len(&self) -> Option<usize> {
        if self.bitrate == 0 {
            return None;
        }
        let pad = usize::from(self.padding);
        let (br, sr) = (self.bitrate as usize * 1000, self.rate as usize);
        Some(match self.layer {
            1 => (12 * br / sr + pad) * 4,
            3 if self.version != 1 => 72 * br / sr + pad,
            _ => 144 * br / sr + pad,
        })
    }

    /// The same stream: version, layer and rate (channel mode may change between joint and plain stereo).
    fn same_stream(&self, o: &Header) -> bool {
        (self.version, self.layer, self.rate) == (o.version, o.layer, o.rate)
    }

    fn side_info(&self) -> usize {
        match (self.version == 1, self.mono) {
            (true, true) => 17,
            (true, false) => 32,
            (false, true) => 9,
            (false, false) => 17,
        }
    }

    /// Layer III: how many bytes of the earlier frames' main data the frame's own starts back into
    /// (`main_data_begin`: 9 bits in MPEG-1, 8 in MPEG-2 / 2.5), and how many bytes of main data the frame holds.
    /// None for layers I and II, which have no bit reservoir, and for a frame too short for its side info.
    fn reservoir(&self, frame: &[u8]) -> Option<(usize, usize)> {
        if self.layer != 3 {
            return None;
        }
        let at = 4 + if self.crc { 2 } else { 0 };
        let b = frame.get(at..at + 2)?;
        let begin =
            if self.version == 1 { (usize::from(b[0]) << 1) | usize::from(b[1] >> 7) } else { usize::from(b[0]) };
        let own = frame.len().checked_sub(at + self.side_info())?;
        Some((begin, own))
    }
}

/// A frame at `at`: its header and length. Free format: the length is the distance to the next header of the
/// same stream.
fn frame_at(file: &[u8], at: usize) -> Option<(Header, usize)> {
    let h = Header::parse(file.get(at..)?)?;
    let len = match h.len() {
        Some(n) => n,
        None => {
            let next = (at + 4..file.len().saturating_sub(4))
                .find(|&i| Header::parse(&file[i..]).is_some_and(|n| n.same_stream(&h) && n.bitrate == 0))?;
            next - at
        }
    };
    (len >= 4).then_some((h, len))
}

/// A frame at `at` that another frame of the same stream follows (or the end of the data, or a tag): a real frame,
/// not a sync pattern inside other data.
fn confirmed_frame(file: &[u8], at: usize, like: Option<&Header>) -> Option<(Header, usize)> {
    let (h, len) = frame_at(file, at)?;
    if like.is_some_and(|l| !l.same_stream(&h)) {
        return None;
    }
    let next = at.checked_add(len)?;
    if next >= file.len() || trailing_tag(&file[next..]).is_some() {
        return Some((h, len));
    }
    let (n, _) = frame_at(file, next)?;
    n.same_stream(&h).then_some((h, len))
}

/// Reads an MPEG audio file's statements and layout. None when no MPEG audio frame is found.
pub fn read(file: &[u8]) -> Option<Mp3Statements> {
    let mut issues = Issues::default();
    let mut at = 0usize;
    // ID3v2 tags at the start, one after the other
    while let Some(len) = id3v2(file, at, &mut issues) {
        at = at.saturating_add(len).min(file.len());
    }
    let start = at;
    let first = (start..file.len()).find(|&i| confirmed_frame(file, i, None).is_some())?;
    let junk = &file[start..first];
    if junk.iter().any(|&b| b != 0) {
        issues.add(
            IssueKind::JunkOutsideAudio,
            1,
            Some(Position::Byte(start as u64)),
            format!("{} bytes before the first frame", junk.len()),
        );
    }
    let (h0, len0) = confirmed_frame(file, first, None)?;
    let (header_frame, stated_frames, lame) = header_frame(&file[first..first + len0.min(file.len() - first)], &h0);
    let mut s = Mp3Statements {
        version: h0.version,
        layer: h0.layer,
        rate: h0.rate,
        channels: if h0.mono { 1 } else { 2 },
        samples_per_frame: h0.samples(),
        first_frame_at: first as u64,
        header_frame,
        stated_frames,
        lame,
        audio_frames: 0,
        issues: Issues::default(),
    };
    walk(file, first, &h0, header_frame.is_some(), &mut s, &mut issues);
    if let Some(l) = s.lame
        && !l.checksum_ok
    {
        let used = s.gapless().is_some();
        let what = if used { "used: its values are plausible" } else { "not used" };
        let kind = if used { IssueKind::GaplessChecksum } else { IssueKind::GaplessIgnored };
        issues.add(kind, 1, Some(Position::Byte(s.first_frame_at)), format!("LAME extension checksum fails; {what}"));
    } else if s.lame.is_some() && s.gapless().is_none() {
        issues.add(IssueKind::GaplessIgnored, 1, None, "LAME extension delay or padding out of range");
    }
    if s.gapless().is_none() && s.layer == 3 {
        issues.add(IssueKind::NoGaplessInfo, 1, None, "no usable LAME / ffmpeg extension: the start is the decoder's");
    }
    if let Some(n) = s.stated_frames {
        let n = u64::from(n);
        if n > s.audio_frames && !issues.has(IssueKind::Truncated) {
            issues.add(
                IssueKind::Truncated,
                1,
                None,
                format!("the header frame states {n} frames, the stream has {}", s.audio_frames),
            );
        } else if n != s.audio_frames && s.lame.is_some() {
            issues.add(
                IssueKind::GaplessIgnored,
                1,
                None,
                format!(
                    "the header frame states {n} frames, the stream has {}: the end is not trimmed",
                    s.audio_frames
                ),
            );
        }
    }
    s.issues = issues;
    Some(s)
}

/// Walks the frames from the first one: counts them, and records the gaps, the cut, the format changes and what
/// follows the last frame.
fn walk(file: &[u8], first: usize, h0: &Header, has_header: bool, s: &mut Mp3Statements, issues: &mut Issues) {
    let mut at = first;
    let mut frames = 0u64;
    let mut prev = *h0;
    // DESIGN D13: main data bytes of the audio frames so far, and the frames that start back further than that
    let mut held = 0usize;
    let mut cut_start: Option<(usize, usize, usize)> = None;
    let mut cut_frames = 0u32;
    while at < file.len() {
        let rest = &file[at..];
        if let Some(tag) = trailing_tag(rest) {
            at = at.saturating_add(tag.max(1));
            continue;
        }
        match frame_at(file, at) {
            Some((h, len)) if h.same_stream(h0) => {
                if at + len > file.len() {
                    issues.add(
                        IssueKind::Truncated,
                        1,
                        Some(Position::Byte(at as u64)),
                        format!("the last frame is cut: {} of {len} bytes", file.len() - at),
                    );
                    break;
                }
                if h.mono != prev.mono {
                    issues.add(
                        IssueKind::FormatChange,
                        1,
                        Some(Position::Byte(at as u64)),
                        "the channel count changes inside the stream",
                    );
                }
                let audio = !(has_header && frames == 0);
                if audio && let Some((begin, own)) = h.reservoir(&file[at..at + len]) {
                    if begin > held {
                        cut_start.get_or_insert((at, begin, held));
                        cut_frames += 1;
                    }
                    held = held.saturating_add(own);
                }
                prev = h;
                frames += 1;
                at += len;
                continue;
            }
            Some((h, _)) if confirmed_frame(file, at, Some(&h)).is_some() => {
                issues.add(
                    IssueKind::FormatChange,
                    1,
                    Some(Position::Byte(at as u64)),
                    format!(
                        "from MPEG-{} layer {} {} Hz to MPEG-{} layer {} {} Hz",
                        h0.version, h0.layer, h0.rate, h.version, h.layer, h.rate
                    ),
                );
                // frames of another stream: skipped as the decoder skips them
                let (_, len) = frame_at(file, at).unwrap_or((h, 1));
                at = at.saturating_add(len.min(file.len() - at).max(1));
                continue;
            }
            _ => {}
        }
        // not a frame: the next real one, or the end
        let next = (at + 1..file.len())
            .find(|&i| trailing_tag(&file[i..]).is_some() || confirmed_frame(file, i, Some(h0)).is_some());
        let gap = &file[at..next.unwrap_or(file.len())];
        let data = gap.iter().any(|&b| b != 0);
        match next {
            Some(n) if trailing_tag(&file[n..]).is_none() => issues.add(
                IssueKind::StreamGap,
                1,
                Some(Position::Byte(at as u64)),
                format!("{} bytes that are not audio between two frames", n - at),
            ),
            _ if data => issues.add(
                IssueKind::JunkOutsideAudio,
                1,
                Some(Position::Byte(at as u64)),
                format!("{} bytes after the last frame", gap.len()),
            ),
            _ => {}
        }
        match next {
            Some(n) => at = n,
            None => break,
        }
    }
    if let Some((at, begin, held)) = cut_start {
        issues.add(
            IssueKind::Truncated,
            cut_frames,
            Some(Position::Byte(at as u64)),
            format!("the stream starts cut: a frame needs {begin} bytes of the frames before it, the file has {held}"),
        );
    }
    s.audio_frames = frames.saturating_sub(u64::from(has_header));
}

/// The header frame's kind, stated frame count and LAME extension.
fn header_frame(f: &[u8], h: &Header) -> (Option<HeaderFrame>, Option<u32>, Option<LameExtension>) {
    const VBRI_AT: usize = 4 + 32;
    let be32 = |o: usize| f.get(o..o + 4).map(|x| u32::from_be_bytes([x[0], x[1], x[2], x[3]]));
    let xing_at = 4 + 2 * usize::from(h.crc) + h.side_info();
    let kind = match f.get(xing_at..xing_at + 4) {
        Some(b"Xing") => Some(HeaderFrame::Xing),
        Some(b"Info") => Some(HeaderFrame::Info),
        _ => None,
    };
    if let Some(kind) = kind {
        let Some(flags) = be32(xing_at + 4) else { return (Some(kind), None, None) };
        let mut o = xing_at + 8;
        let frames = if flags & 1 != 0 {
            let n = be32(o);
            o += 4;
            n
        } else {
            None
        };
        for (bit, n) in [(2u32, 4usize), (4, 100), (8, 4)] {
            if flags & bit != 0 {
                o += n;
            }
        }
        return (Some(kind), frames, lame_extension(f, o));
    }
    if f.get(VBRI_AT..VBRI_AT + 4) == Some(b"VBRI") {
        return (Some(HeaderFrame::Vbri), be32(VBRI_AT + 14), None);
    }
    (None, None, None)
}

/// The LAME extension at `o`: 36 bytes; delay and padding at 21..24 (12 bits each), the checksum of the frame's
/// bytes before it at 34..36.
fn lame_extension(f: &[u8], o: usize) -> Option<LameExtension> {
    const ENCODERS: [&[u8; 4]; 3] = [b"LAME", b"Lavc", b"Lavf"];
    const LEN: usize = 36;
    const CHECKSUM_AT: usize = 34;
    let ext = f.get(o..o + LEN)?;
    let encoder: [u8; 4] = ext[..4].try_into().ok()?;
    if !ENCODERS.contains(&&encoder) {
        return None;
    }
    let dp = u32::from_be_bytes([0, ext[21], ext[22], ext[23]]);
    let stored = u16::from_be_bytes([ext[CHECKSUM_AT], ext[CHECKSUM_AT + 1]]);
    Some(LameExtension {
        encoder,
        delay: (dp >> 12) as u16,
        padding: (dp & 0xfff) as u16,
        checksum_ok: stored == crc16(&f[..o + CHECKSUM_AT]),
    })
}

/// CRC-16 with the reflected 0x8005 polynomial, starting at 0: the LAME extension's checksum.
fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in bytes {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xa001 } else { crc >> 1 };
        }
    }
    crc
}

/// A tag that can follow the audio: ID3v1 (`TAG`, 128 bytes), APEv2 (`APETAGEX`, header or footer), Lyrics3
/// (`LYRICSBEGIN`), or an ID3v2 tag. Its length (0 when unknown: the rest of the data).
fn trailing_tag(b: &[u8]) -> Option<usize> {
    const ID3V1: usize = 128;
    const APE_HEADER: usize = 32;
    if b.starts_with(b"TAG") && b.len() >= ID3V1 && b.len() - ID3V1 < APE_HEADER + ID3V1 * 2 {
        return Some(ID3V1);
    }
    if b.starts_with(b"APETAGEX") && b.len() >= APE_HEADER {
        let size = u32::from_le_bytes(b[12..16].try_into().ok()?) as usize;
        let flags = u32::from_le_bytes(b[20..24].try_into().ok()?);
        // a header (bit 29) is followed by the items and a footer: `size` counts them
        let header = flags & (1 << 29) != 0;
        return Some(if header { APE_HEADER.saturating_add(size) } else { APE_HEADER });
    }
    if b.starts_with(b"LYRICSBEGIN") {
        return Some(0);
    }
    if b.starts_with(b"ID3") && b.len() >= 10 {
        return Some(10usize.saturating_add(syncsafe(&b[6..10])?));
    }
    None
}

fn syncsafe(b: &[u8]) -> Option<usize> {
    if b.len() < 4 || b.iter().any(|&x| x >= 0x80) {
        return None;
    }
    Some((usize::from(b[0]) << 21) | (usize::from(b[1]) << 14) | (usize::from(b[2]) << 7) | usize::from(b[3]))
}

/// An ID3v2 tag at `at`: checks its frames into `issues` and gives its total length. None when there is no tag.
fn id3v2(file: &[u8], at: usize, issues: &mut Issues) -> Option<usize> {
    const HEADER: usize = 10;
    let h = file.get(at..at + HEADER)?;
    if &h[..3] != b"ID3" || h[3] == 0xff || h[4] == 0xff {
        return None;
    }
    let (major, flags) = (h[3], h[5]);
    let Some(size) = syncsafe(&h[6..10]) else {
        issues.add(IssueKind::TagDamaged, 1, Some(Position::Byte(at as u64)), "ID3v2 tag size is not syncsafe");
        return Some(HEADER);
    };
    let footer = if major == 4 && flags & 0x10 != 0 { HEADER } else { 0 };
    let total = HEADER + size + footer;
    let body_end = (at + HEADER + size).min(file.len());
    if at + total > file.len() {
        issues.add(
            IssueKind::TagDamaged,
            1,
            Some(Position::Byte(at as u64)),
            format!("ID3v2 tag runs {} bytes past the end of the file", at + total - file.len()),
        );
        // its size cannot be trusted: look for the audio right after the header
        return Some(HEADER);
    }
    // unsynchronised tags hold stuffing bytes: their frames are not checked
    if flags & 0x80 == 0 {
        let mut body = &file[at + HEADER..body_end];
        if flags & 0x40 != 0 {
            let ext = match major {
                3 => be_u32(body).map(|n| n as usize + 4),
                4 => syncsafe(body),
                _ => None,
            };
            body = body.get(ext.unwrap_or(body.len())..).unwrap_or_default();
        }
        id3v2_frames(body, major, at + HEADER, issues);
    }
    Some(total)
}

fn be_u32(b: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(..4)?.try_into().ok()?))
}

/// Checks the frames of an ID3v2 tag body: well-formed ids and sizes, valid text encodings, and the fixed fields of
/// the frames that have them.
fn id3v2_frames(body: &[u8], major: u8, offset: usize, issues: &mut Issues) {
    let (id_len, head) = if major == 2 { (3, 6) } else { (4, 10) };
    let mut bad: Vec<String> = Vec::new();
    let mut first_bad = None;
    let mut i = 0usize;
    while let Some(h) = body.get(i..i + head) {
        if h[0] == 0 {
            break; // padding
        }
        let id = &h[..id_len];
        if !id.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
            bad.push("unreadable frame id".into());
            first_bad.get_or_insert(offset + i);
            break;
        }
        let size = match major {
            2 => (usize::from(h[3]) << 16) | (usize::from(h[4]) << 8) | usize::from(h[5]),
            3 => be_u32(&h[4..8]).map_or(usize::MAX, |n| n as usize),
            _ => syncsafe(&h[4..8])
                .filter(|&n| frame_fits(body, i + head + n, id_len))
                .or_else(|| be_u32(&h[4..8]).map(|n| n as usize))
                .unwrap_or(usize::MAX),
        };
        let name = String::from_utf8_lossy(id).into_owned();
        let Some(data) = body.get(i + head..(i + head).saturating_add(size)) else {
            bad.push(format!("{name} runs past the tag"));
            first_bad.get_or_insert(offset + i);
            break;
        };
        // compressed, encrypted or per-frame unsynchronised content is not checked
        let opaque = match major {
            3 => h[9] & 0xc0 != 0,
            4 => h[9] & 0x0e != 0,
            _ => false,
        };
        if !opaque && let Some(why) = frame_problem(id, data, major) {
            bad.push(format!("{name} {why}"));
            first_bad.get_or_insert(offset + i);
        }
        i += head + size;
    }
    if !bad.is_empty() {
        let n = u32::try_from(bad.len()).unwrap_or(u32::MAX);
        let detail = format!("ID3v2.{major}: {}", bad.join(", "));
        issues.add(IssueKind::TagDamaged, n, first_bad.map(|b| Position::Byte(b as u64)), detail);
    }
}

/// A frame of `n` bytes ending at `end` is followed by another frame id, padding or the end of the body.
fn frame_fits(body: &[u8], end: usize, id_len: usize) -> bool {
    match body.get(end..end + id_len) {
        None => end <= body.len(),
        Some(next) => next[0] == 0 || next.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
    }
}

/// What is wrong with one frame's content, if anything.
fn frame_problem(id: &[u8], data: &[u8], major: u8) -> Option<&'static str> {
    let enc_ok = |d: &[u8]| d.first().is_some_and(|&e| e <= 3);
    if data.is_empty() {
        return Some("is empty");
    }
    let v22 = major == 2;
    let is = |v3: &[u8], v2: &[u8]| id == if v22 { v2 } else { v3 };
    if id[0] == b'T' {
        return (!enc_ok(data)).then_some("has an invalid text encoding");
    }
    if is(b"COMM", b"COM") || is(b"USLT", b"ULT") {
        if data.len() < 4 {
            return Some("is shorter than its fixed fields");
        }
        return (!enc_ok(data)).then_some("has an invalid text encoding");
    }
    if is(b"APIC", b"PIC") {
        let fixed = if v22 { 6 } else { 4 };
        if data.len() < fixed {
            return Some("is shorter than its fixed fields");
        }
        return (!enc_ok(data)).then_some("has an invalid text encoding");
    }
    if is(b"POPM", b"POP") {
        let nul = data.iter().position(|&b| b == 0)?;
        return (data.len() < nul + 2).then_some("is shorter than its fixed fields");
    }
    if is(b"PCNT", b"CNT") {
        return (data.len() < 4).then_some("is shorter than its fixed fields");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MPEG-1 layer III, 128 kbit/s, 44.1 kHz, joint stereo: 417 bytes, no padding.
    const HEADER: [u8; 4] = [0xff, 0xfb, 0x90, 0x64];

    /// Side info of a stereo MPEG-1 frame.
    const SIDE_INFO: usize = 32;

    /// A frame whose main data starts in itself (`main_data_begin` 0).
    fn frame() -> Vec<u8> {
        frame_back(0)
    }

    /// A frame whose main data starts `begin` bytes back in the frames before it.
    fn frame_back(begin: u16) -> Vec<u8> {
        let mut f = HEADER.to_vec();
        f.extend((begin << 7).to_be_bytes());
        f.resize(4 + SIDE_INFO, 0);
        f.resize(417, 0x55);
        f
    }

    fn info_frame(delay: u16, padding: u16, frames: u32, good_crc: bool) -> Vec<u8> {
        let mut f = HEADER.to_vec();
        f.resize(417, 0);
        let x = 4 + 32;
        f[x..x + 4].copy_from_slice(b"Info");
        f[x + 4..x + 8].copy_from_slice(&1u32.to_be_bytes());
        f[x + 8..x + 12].copy_from_slice(&frames.to_be_bytes());
        let l = x + 12;
        f[l..l + 9].copy_from_slice(b"LAME3.100");
        let dp = (u32::from(delay) << 12) | u32::from(padding);
        f[l + 21..l + 24].copy_from_slice(&dp.to_be_bytes()[1..]);
        let crc = crc16(&f[..l + 34]) ^ u16::from(!good_crc);
        f[l + 34..l + 36].copy_from_slice(&crc.to_be_bytes());
        f
    }

    fn stream(lead: &[u8], frames: usize) -> Vec<u8> {
        let mut b = lead.to_vec();
        for _ in 0..frames {
            b.extend(frame());
        }
        b
    }

    fn kinds(s: &Mp3Statements) -> Vec<IssueKind> {
        s.issues.0.iter().map(|i| i.kind).collect()
    }

    #[test]
    fn crc_matches_the_reference_check_value() {
        // DESIGN D4: CRC-16/ARC of "123456789"
        assert_eq!(crc16(b"123456789"), 0xbb3d);
    }

    #[test]
    fn a_lame_header_states_delay_padding_and_frames() {
        // DESIGN D4
        let s = read(&stream(&info_frame(576, 1080, 20, true), 20)).unwrap();
        assert_eq!(s.header_frame, Some(HeaderFrame::Info));
        assert_eq!((s.stated_frames, s.audio_frames), (Some(20), 20));
        assert_eq!(s.gapless(), Some((576, 1080)));
        assert!(s.end_is_stated() && kinds(&s).is_empty());
    }

    #[test]
    fn a_failed_checksum_is_a_note_and_plausible_values_are_used() {
        // DESIGN D5
        let s = read(&stream(&info_frame(576, 1080, 20, false), 20)).unwrap();
        assert_eq!(s.gapless(), Some((576, 1080)));
        assert_eq!(kinds(&s), [IssueKind::GaplessChecksum]);
        let s = read(&stream(&info_frame(4095, 4095, 20, false), 3)).unwrap();
        assert_eq!(s.gapless(), None);
        assert!(kinds(&s).contains(&IssueKind::GaplessIgnored));
    }

    #[test]
    fn plain_stream_has_no_statements() {
        let s = read(&stream(&[], 10)).unwrap();
        assert_eq!((s.header_frame, s.audio_frames, s.gapless()), (None, 10, None));
        assert_eq!(kinds(&s), [IssueKind::NoGaplessInfo]);
    }

    #[test]
    fn a_gap_inside_the_stream_is_damage_and_junk_outside_a_note() {
        // DESIGN D7
        let mut b = vec![1, 2, 3];
        b.extend(stream(&info_frame(576, 1080, 20, true), 10));
        b.extend([7u8; 300]);
        b.extend(stream(&[], 10));
        b.extend([9u8; 50]);
        let s = read(&b).unwrap();
        assert_eq!(s.audio_frames, 20);
        let k = kinds(&s);
        assert!(k.contains(&IssueKind::StreamGap) && k.contains(&IssueKind::JunkOutsideAudio), "{k:?}");
        let gap = s.issues.0.iter().find(|i| i.kind == IssueKind::StreamGap).unwrap();
        assert_eq!(gap.at, Some(Position::Byte(3 + 11 * 417)));
    }

    #[test]
    fn a_cut_last_frame_and_missing_frames_are_truncation() {
        // DESIGN D7
        let mut b = stream(&info_frame(576, 1080, 20, true), 20);
        b.truncate(b.len() - 100);
        let s = read(&b).unwrap();
        assert_eq!(s.audio_frames, 19);
        assert_eq!(s.issues.0.iter().filter(|i| i.kind == IssueKind::Truncated).count(), 1);
        assert!(!s.end_is_stated());
    }

    #[test]
    fn a_stream_that_starts_inside_another_is_truncation() {
        // DESIGN D13: the first frame needs bytes of frames that are not in the file
        let mut b = frame_back(300);
        b.extend(stream(&[], 10));
        let s = read(&b).unwrap();
        let cut = s.issues.0.iter().find(|i| i.kind == IssueKind::Truncated).expect("cut start");
        assert_eq!((cut.count, cut.at), (1, Some(Position::Byte(0))));
        // what earlier frames of the file hold is there: no cut (each frame holds 417 - 36 bytes)
        let mut b = stream(&info_frame(576, 1080, 3, true), 1);
        b.extend(frame_back(381));
        b.extend(frame_back(511));
        let s = read(&b).unwrap();
        assert!(!kinds(&s).contains(&IssueKind::Truncated), "{:?}", kinds(&s));
        // the header frame holds no audio: a frame right after it cannot start back into it
        let mut b = info_frame(576, 1080, 2, true);
        b.extend(frame_back(10));
        b.extend(frame());
        assert!(kinds(&read(&b).unwrap()).contains(&IssueKind::Truncated));
    }

    #[test]
    fn broken_id3_frames_are_notes_and_trailing_tags_are_not_junk() {
        // DESIGN D6
        let fr = |id: &[u8], d: &[u8]| {
            let mut v = id.to_vec();
            v.extend((d.len() as u32).to_be_bytes());
            v.extend([0, 0]);
            v.extend(d);
            v
        };
        let body = [fr(b"TIT2", b"\0T"), fr(b"COMM", b"\0e"), fr(b"PCNT", b"\0\x05"), fr(b"TPE1", b"\x07A")].concat();
        let mut tag = b"ID3\x03\0\0".to_vec();
        tag.extend([0, 0, 0, body.len() as u8 + 16]);
        tag.extend(&body);
        tag.extend([0u8; 16]);
        let mut b = stream(&tag, 10);
        let mut v1 = b"TAG".to_vec();
        v1.resize(128, b' ');
        b.extend(v1);
        let s = read(&b).unwrap();
        assert_eq!(s.audio_frames, 10);
        let t = s.issues.0.iter().find(|i| i.kind == IssueKind::TagDamaged).expect("tag issue");
        assert_eq!(t.count, 3, "{}", t.detail);
        assert!(!kinds(&s).contains(&IssueKind::JunkOutsideAudio));
    }

    #[test]
    fn hostile_input_never_panics() {
        assert!(read(b"").is_none());
        assert!(read(&[0xff; 64]).is_none());
        let mut b = b"ID3\x04\0\x50\x7f\x7f\x7f\x7f".to_vec();
        b.extend(stream(&[], 3));
        assert!(read(&b).is_some());
    }
}
