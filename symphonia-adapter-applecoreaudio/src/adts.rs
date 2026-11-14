//! ADTS: raw AAC frames, each behind a 7-byte header (9 with a CRC), as `.aac` files hold them (ISO/IEC 13818-7,
//! 14496-3 1.A.2). A container reader only: the frames go to whichever AAC decoder the registry holds (DESIGN A7).

use symphonia_core::audio::Channels;
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::CodecProfile;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia_core::common::Tier;
use symphonia_core::errors::{Error, Result, SeekErrorKind, decode_error, seek_error};
use symphonia_core::formats::probe::{Probe, ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_ADTS;
use symphonia_core::formats::{
    FormatInfo, FormatOptions, FormatReader, MediaInfo, SeekMode, SeekTo, SeekedTo, Track, TrackFlags,
};
use symphonia_core::io::{MediaSourceStream, ReadBytes, ScopedStream, SeekBuffered};
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::packet::Packet;
use symphonia_core::support_format;
use symphonia_core::units::{Duration, TimeBase, Timestamp};

/// Bytes of a header without its CRC.
const HEADER_LEN: usize = 7;
/// Bytes of the CRC that follows the header when `protection_absent` is 0.
const CRC_LEN: usize = 2;
/// Frames of audio in one raw data block, at the header's (core) rate.
const FRAMES_PER_BLOCK: u64 = 1024;
/// The sampling frequencies by index (ISO/IEC 14496-3 table 1.18); 13 and 14 are reserved, 15 is not allowed in ADTS.
const RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
/// Junk scanned for the next header before the rest of the stream counts as trailing data (one second of the largest
/// frames, 8191 bytes at 47 frames per second at 48 kHz, is far less).
const MAX_RESYNC: u64 = 1 << 20;

/// The reader's description.
const INFO: FormatInfo =
    FormatInfo { format: FORMAT_ID_ADTS, short_name: "adts", long_name: "Audio Data Transport Stream" };

/// One frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    /// The MPEG-4 audio object type: the 2-bit profile + 1 (1 Main, 2 LC, 3 SSR, 4 LTP).
    object_type: u8,
    /// Index in [`RATES`].
    rate_index: u8,
    /// The channel configuration (0: in a program config element of the stream).
    channels: u8,
    /// A CRC follows the header.
    crc: bool,
    /// The frame's length, header included.
    frame_len: usize,
    /// Raw data blocks in the frame (1 to 4).
    blocks: u8,
}

impl Header {
    /// Parses a header; None when `b` is not one (sync word, layer, reserved rate, a frame shorter than its header).
    fn parse(b: &[u8; HEADER_LEN]) -> Option<Header> {
        // syncword 0xFFF, then ID (any), layer 00, protection_absent (any)
        if b[0] != 0xff || b[1] & 0xf6 != 0xf0 {
            return None;
        }
        let rate_index = (b[2] >> 2) & 0x0f;
        if usize::from(rate_index) >= RATES.len() {
            return None;
        }
        let crc = b[1] & 0x01 == 0;
        let frame_len = usize::from(b[3] & 0x03) << 11 | usize::from(b[4]) << 3 | usize::from(b[5] >> 5);
        let header = Header {
            object_type: (b[2] >> 6) + 1,
            rate_index,
            channels: (b[2] & 0x01) << 2 | b[3] >> 6,
            crc,
            frame_len,
            blocks: (b[6] & 0x03) + 1,
        };
        (frame_len > header.header_len()).then_some(header)
    }

    /// Bytes before the raw data: the header, the block positions and the CRC when protected.
    fn header_len(&self) -> usize {
        match (self.crc, self.blocks) {
            (false, _) => HEADER_LEN,
            (true, 1) => HEADER_LEN + CRC_LEN,
            // raw_data_block_position for each block after the first, then the header's CRC
            (true, n) => HEADER_LEN + 2 * usize::from(n - 1) + CRC_LEN,
        }
    }

    fn rate(&self) -> u32 {
        RATES[usize::from(self.rate_index)]
    }

    /// The same stream: object type, rate and channels as `other`.
    fn continues(&self, other: &Header) -> bool {
        (self.object_type, self.rate_index, self.channels) == (other.object_type, other.rate_index, other.channels)
    }

    /// The `AudioSpecificConfig` the header stands for (ISO/IEC 14496-3 1.6.2.1): object type, rate index, channels.
    fn config(&self) -> [u8; 2] {
        let bits = u16::from(self.object_type) << 11 | u16::from(self.rate_index) << 7 | u16::from(self.channels) << 3;
        bits.to_be_bytes()
    }
}

/// Reads a header at the stream's position.
fn read_header(src: &mut impl ReadBytes) -> std::io::Result<Option<Header>> {
    let mut b = [0u8; HEADER_LEN];
    src.read_buf_exact(&mut b)?;
    Ok(Header::parse(&b))
}

/// Reads an ADTS stream: one audio track, one packet per frame, timestamps counted in frames at the header's rate (the
/// core rate of HE-AAC). Junk inside the stream is skipped up to the next header of the same stream, reported once as
/// a `DecodeError` before the frame after it (DESIGN A7).
pub struct AdtsReader<'s> {
    reader: MediaSourceStream<'s>,
    tracks: Vec<Track>,
    media: MediaInfo,
    metadata: MetadataLog,
    /// The first header: what the rest of the stream must continue.
    first: Header,
    /// Timestamp of the next packet.
    next_ts: u64,
    /// A frame found after junk: returned on the call after the one that reported the junk.
    pending: Option<Packet>,
}

impl<'s> AdtsReader<'s> {
    /// A reader positioned on the first header of `mss`.
    ///
    /// # Errors
    ///
    /// `DecodeError` when no ADTS header is there.
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let Some(first) = read_header(&mut mss)? else {
            return decode_error("adts: no frame header");
        };
        mss.seek_buffered_rel(-(HEADER_LEN as isize));
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_AAC)
            .with_profile(CodecProfile::new(u32::from(first.object_type) - 1))
            .with_sample_rate(first.rate())
            .with_max_frames_per_packet(FRAMES_PER_BLOCK)
            .with_extra_data(Box::new(first.config()));
        if let Some(channels) = channels(first.channels) {
            params.with_channels(channels);
        }
        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(params)).with_flags(TrackFlags::DEFAULT);
        if let Some(tb) = TimeBase::try_new(1, first.rate()) {
            track.with_time_base(tb);
        }
        let tracks = vec![track];
        let media = MediaInfo::from_tracks(&tracks);
        Ok(AdtsReader {
            reader: mss,
            tracks,
            media,
            metadata: opts.external_data.metadata.unwrap_or_default(),
            first,
            next_ts: 0,
            pending: None,
        })
    }

    /// The frame of `header` (read): its raw data as a packet.
    fn frame(&mut self, header: Header) -> Result<Packet> {
        self.reader.ignore_bytes((header.header_len() - HEADER_LEN) as u64)?;
        let data = self.reader.read_boxed_slice_exact(header.frame_len - header.header_len())?;
        let dur = FRAMES_PER_BLOCK * u64::from(header.blocks);
        let ts = Timestamp::new(i64::try_from(self.next_ts).unwrap_or(i64::MAX));
        self.next_ts += dur;
        Ok(Packet::new(0, ts, Duration::new(dur), data))
    }

    /// After `start`, 7 bytes that are not a header of the stream: the next header that is, None at the end of the
    /// stream (or past [`MAX_RESYNC`] bytes: trailing data such as tags).
    fn resync(&mut self, start: [u8; HEADER_LEN]) -> Result<Option<Header>> {
        let mut window = start;
        for _ in 0..MAX_RESYNC {
            window.copy_within(1.., 0);
            window[HEADER_LEN - 1] = match self.reader.read_byte() {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(e) => return Err(e.into()),
            };
            if let Some(h) = Header::parse(&window).filter(|h| h.continues(&self.first)) {
                return Ok(Some(h));
            }
        }
        Ok(None)
    }
}

/// The channel layout of a channel configuration (ISO/IEC 14496-3 table 1.19); None for 0 (stated in the stream).
fn channels(config: u8) -> Option<Channels> {
    use symphonia_core::audio::Position as P;
    let positions = match config {
        1 => P::FRONT_CENTER,
        2 => P::FRONT_LEFT | P::FRONT_RIGHT,
        3 => P::FRONT_CENTER | P::FRONT_LEFT | P::FRONT_RIGHT,
        4 => P::FRONT_CENTER | P::FRONT_LEFT | P::FRONT_RIGHT | P::REAR_CENTER,
        5 => P::FRONT_CENTER | P::FRONT_LEFT | P::FRONT_RIGHT | P::REAR_LEFT | P::REAR_RIGHT,
        6 => P::FRONT_CENTER | P::FRONT_LEFT | P::FRONT_RIGHT | P::REAR_LEFT | P::REAR_RIGHT | P::LFE1,
        7 => {
            P::FRONT_CENTER
                | P::FRONT_LEFT
                | P::FRONT_RIGHT
                | P::SIDE_LEFT
                | P::SIDE_RIGHT
                | P::REAR_LEFT
                | P::REAR_RIGHT
                | P::LFE1
        }
        _ => return None,
    };
    Some(Channels::Positioned(positions))
}

impl FormatReader for AdtsReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &INFO
    }

    fn media_info(&self) -> &MediaInfo {
        &self.media
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn seek(&mut self, _mode: SeekMode, _to: SeekTo) -> Result<SeekedTo> {
        // a whole-stream reader: the caller decodes from the start (DESIGN A7)
        seek_error(SeekErrorKind::Unseekable)
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        if let Some(p) = self.pending.take() {
            return Ok(Some(p));
        }
        let mut b = [0u8; HEADER_LEN];
        b[0] = match self.reader.read_byte() {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        // a header cut by the end of the stream is a cut frame: the I/O error says so
        self.reader.read_buf_exact(&mut b[1..])?;
        if let Some(h) = Header::parse(&b).filter(|h| h.continues(&self.first)) {
            return self.frame(h).map(Some);
        }
        // junk: report it, then give the frame after it on the next call
        let Some(h) = self.resync(b)? else { return Ok(None) };
        self.pending = Some(self.frame(h)?);
        decode_error("adts: junk inside the stream skipped up to the next frame header")
    }

    fn into_inner<'b>(self: Box<Self>) -> MediaSourceStream<'b>
    where
        Self: 'b,
    {
        self.reader
    }
}

impl Scoreable for AdtsReader<'_> {
    fn score(mut src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        // a header, and a second one of the same stream right after its frame
        let Some(first) = read_header(&mut src)? else { return Ok(Score::Unsupported) };
        src.ignore_bytes((first.frame_len - HEADER_LEN) as u64)?;
        match read_header(&mut src) {
            Ok(Some(next)) if next.continues(&first) => Ok(Score::Supported(u8::MAX)),
            Ok(_) => Ok(Score::Unsupported),
            // a stream of one frame
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(Score::Supported(u8::MAX / 2)),
            Err(e) => Err(Error::IoError(e)),
        }
    }
}

impl<'a> ProbeableFormat<'a> for AdtsReader<'_> {
    fn try_probe_new(mss: MediaSourceStream<'a>, opts: FormatOptions) -> Result<Box<dyn FormatReader + 'a>> {
        Ok(Box::new(AdtsReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        // the sync word with layer 00: MPEG-4 or MPEG-2 ID, with or without a CRC (MPEG audio layers are never 00)
        &[support_format!(
            INFO,
            &["aac", "adts"],
            &["audio/aac", "audio/aacp", "audio/x-aac"],
            &[&[0xff, 0xf1], &[0xff, 0xf0], &[0xff, 0xf9], &[0xff, 0xf8]]
        )]
    }
}

/// Registers the ADTS reader in `probe` ahead of any other ([`Tier::Preferred`]), on every platform (DESIGN A7).
pub fn register_formats(probe: &mut Probe) {
    probe.register_format_at_tier::<AdtsReader<'static>>(Tier::Preferred);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A header: AAC-LC, 44.1 kHz, stereo, no CRC, `len` bytes, one block.
    fn lc_header(len: usize) -> [u8; HEADER_LEN] {
        let len = len as u32;
        [0xff, 0xf1, 0x50, 0x80 | (len >> 11) as u8, (len >> 3) as u8, ((len & 7) << 5) as u8 | 0x1f, 0xfc]
    }

    #[test]
    fn parses_the_fields() {
        let h = Header::parse(&lc_header(300)).unwrap();
        assert_eq!(h, Header { object_type: 2, rate_index: 4, channels: 2, crc: false, frame_len: 300, blocks: 1 });
        assert_eq!((h.rate(), h.header_len(), h.config()), (44100, 7, [0x12, 0x10]));
    }

    #[test]
    fn refuses_what_is_not_a_header() {
        let ok = lc_header(300);
        for (i, v) in [(0, 0xfe), (1, 0xf3), (1, 0xf7), (2, 0x7c)] {
            let mut b = ok;
            b[i] = v;
            assert_eq!(Header::parse(&b), None, "byte {i} = {v:#x}");
        }
        // a frame no longer than its header
        assert_eq!(Header::parse(&lc_header(7)), None);
    }

    #[test]
    fn a_protected_frame_of_several_blocks_skips_their_positions() {
        let mut b = lc_header(300);
        b[1] = 0xf0;
        b[6] = 0xfc | 2;
        let h = Header::parse(&b).unwrap();
        assert_eq!((h.crc, h.blocks, h.header_len()), (true, 3, 7 + 4 + 2));
    }
}
