# Design

Rules of the crate, cited by its tests as `DESIGN An`.

## A1 A Symphonia decoder, Apple platforms only

`AppleAacDecoder` implements Symphonia's `AudioDecoder` and `RegisterableAudioDecoder` for `CODEC_ID_AAC`.
`register` puts it at the preferred tier, ahead of Symphonia's AAC-LC decoder; on other platforms `register` does
nothing and `AVAILABLE` is false, so callers need no platform condition.

## A2 The configuration

The track's `AudioSpecificConfig` (the codec parameters' extra data) is wrapped in an MPEG-4 ES descriptor, the
magic cookie AudioToolbox reads for AAC. The decoded layer is the richest the system can play from it (the format
list of `AudioFormatGetProperty`): HE-AAC v2 over HE-AAC over AAC-LC; xHE-AAC (object type 42) as USAC. A stream
without a configuration (another ADTS reader than A7's) is configured as AAC-LC from its rate and channels.

A bare AAC-LC configuration (two bytes, no extension) at a core rate SBR can double (24 kHz down to 8 kHz) may hide
SBR: HE-AAC signalled implicitly, as every ADTS stream of HE-AAC is. Given as it is, AudioToolbox decodes the core
only. So the decoder states SBR through a backward-compatible extension (sync word `0x2b7`, object type 5, the doubled
rate), the form Apple's encoder writes in MP4 files. Measured: afconvert's HE-AAC ADTS stream then decodes as its MP4
twin (44.1 kHz, first packet 1086 frames); a stream without SBR (afconvert AAC-LC at 22.05 kHz, ADTS and MP4) comes
out at twice its rate, and every second frame of it is Symphonia's decode at the core rate (same start, same 12.8 dB
against the source): the stated SBR costs nothing where there is none.

## A3 Output

Interleaved f32 converted to Symphonia's planar buffer, at the decoded layer's rate and channel count: no resampling,
no channel mixing (PS gives stereo from a mono core). One AudioConverter call per packet, at most one packet's frames
out.

## A4 No trimming

The decoder trims nothing, like Symphonia's own AAC decoder: the encoder delay and padding (edit list, `iTunSMPB`)
are the caller's to remove. AudioToolbox's AAC decoders refuse a prime method (`kAudioConverterPrimeMethod`, status
`prop`); what they do on their own, measured by the tests against the lossless source on macOS 15:

- AAC-LC: the first packet is a whole one (1024 frames); the source starts after the encoder delay (2112 frames for
  Apple's encoder, 1024 for ffmpeg's), the same output as Symphonia's decoder, frame for frame.
- HE-AAC (v1, v2): 2048 frames per packet at twice the core rate, the first packet 962 frames short (the SBR delay is
  dropped by the decoder); the source starts after twice the encoder delay stated at the core rate (2112 -> 4224).

After a reset that followed an error (A5) the first packet out is padded at its front to a whole packet, so the
frames after it keep their place.

## A5 Damaged packets

A packet the converter rejects returns `Error::DecodeError` with AudioToolbox's status, leaves the output empty and
resets the converter, so decoding goes on with the next packet. A damaged packet AudioToolbox accepts gives no frames
and no error status (measured on scrambled packets of AAC-LC and HE-AAC): that is `Error::DecodeError` too, and the
next packets come out whole. The caller fills the gap (one packet: `Packet::dur` at the track's rate, scaled to the
output rate) to keep the timeline.

## A6 Thread safety

Symphonia's decoders are `Send + Sync`. The converter handle is used only through `&mut self` and on drop, never
through `&self`, which makes both sound for an AudioConverter (usable from any thread, one call at a time).

## A7 ADTS reader

`AdtsReader` reads raw AAC streams (`.aac`, ISO/IEC 13818-7 / 14496-3 ADTS) for Symphonia on every platform, so a
build can leave out Symphonia's AAC crate (which holds its ADTS reader) and still open them; `register_formats` puts
it ahead of Symphonia's own. A container reader only, written from the standard (the header layout as in the
`adts-reader` crate, MIT OR Apache-2.0), no code taken from Symphonia's reader:

- one track; one packet per frame, without the header, the CRC and the block positions of a protected frame; its
  duration 1024 frames per raw data block, at the header's rate (the core rate of HE-AAC); timestamps counted from 0;
- the codec parameters from the first header: object type (profile + 1), rate, channel layout, and the matching
  two-byte `AudioSpecificConfig` as extra data;
- a header is valid with the sync word, layer 00, a rate index below 13 and a frame longer than its header; it
  continues the stream with the first header's object type, rate and channels;
- junk inside the stream (bytes that are not such a header) is skipped up to the next header that continues it, at
  most 1 MiB: one `DecodeError` reports it, then the frame after it comes; past the limit, or at the end, the stream
  ends there (trailing tags);
- a frame cut by the end of the file is an I/O error (`UnexpectedEof`);
- a probe needs a header and a second one continuing it after the first frame (or the end: a one-frame stream);
- no seeking (the readers' users decode from the start); a frame of several raw data blocks goes out as one packet
  (not met in the fixtures).

The tests check it against Symphonia's ADTS reader (identical packets on afconvert's and ffmpeg's streams) and on
copies with junk inserted, cut, and followed by an ID3v1 tag.
