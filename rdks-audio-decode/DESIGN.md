# rdks-audio-decode design

The rules the crate follows, with how each was measured. Tests cite the ids (`// DESIGN D4`).

## D1 Best effort, then a report

Decoding never stops for damage. An [`Error`](src/lib.rs) means no usable audio at all (unreadable input, unknown
container, no audio track, unsupported codec, no frame decoded, a decoder panic, a limit). Everything else is an issue
in the report, one entry per kind with its count and first position:

| Severity | Kinds |
|---|---|
| Note (the audio is whole) | `tag_damaged`, `gapless_ignored`, `gapless_checksum`, `no_gapless_info`, `junk_outside_audio` |
| Damage (the audio is not what was encoded) | `undecodable_frames`, `stream_gap`, `truncated`, `format_change` |

A player plays a damaged file and warns; a checker rejects it (`Report::is_valid`).

## D2 AAC in MP4: AudioToolbox's rule

Measured on macOS 15.8 with AudioToolbox opened through `AudioFileOpenWithCallbacks` (`mp4f`) and
`ExtAudioFileWrapAudioFileID`, against synthetic files whose statements were edited in place:

1. the first edit of the edit list, only when the track also has a `roll` sample group (ffmpeg writes both);
2. else the iTunes `iTunSMPB` item (`----`, mean `com.apple.iTunes`): priming at the start, the valid count kept;
3. else 2112 frames at the start, nothing at the end.

An edit list without a roll group is ignored, and so is the QuickTime `keys` / `mdta` form of `iTunSMPB`. ALAC is
not trimmed. The portable decoder (symphonia, AAC-LC only: HE-AAC decodes as its core) follows the same rule.

## D3 Platform and portable agree

One decode loop for every backend: symphonia's demuxers, its decoders, and for `Backend::Platform` on Apple
platforms the system's AAC decoder ahead of symphonia's (`symphonia-adapter-applecoreaudio`: HE-AAC v1 / v2 and
xHE-AAC in full). Both decoders output untrimmed; the trim of D2 is applied here, and both give the same start and
length on every fixture (`tests/aac.rs`). What the container counts (packet durations, the track's length, the
`iTunSMPB` counts) is at the track's rate, the core rate of HE-AAC; a decoder that applies SBR outputs at twice that
rate, so the counts are scaled to the decoded rate. Both decoders report a packet they cannot decode, which becomes
silence of its length (D8), so `check` uses the same backend as `decode`.

## D4 MP3: the stream walker and the LAME rule

`mpeg::read` walks the file without decoding: ID3v2 tags, every frame header, the header frame (Xing / Info / VBRI),
the LAME extension (`LAME`, `Lavc`, `Lavf`: delay, padding, checksum), and trailing tags (ID3v1, APEv2, Lyrics3,
ID3v2). The decoder runs untrimmed; the header frame is never audio, and with a usable extension the start drops
`delay + 529` and the end `padding - 529` (529: the decoder's synthesis delay). symphonia, ffmpeg 8, mpg123,
minimp3 and AudioToolbox all start a LAME-tagged file on the same sample. Without an extension nothing more is
dropped (`no_gapless_info`). When the header frame's frame count differs from the walk, the end is not trimmed.

## D5 A failed LAME checksum

ffmpeg, mpg123, minimp3 and AudioToolbox use the extension when its checksum fails; symphonia ignores it. Here it is
a `gapless_checksum` note and the values are used when plausible (delay and padding under four frames, shorter than
the stream); otherwise `gapless_ignored`.

## D6 Tags never stop a decode

A malformed tag frame (a size past its tag, an unreadable id, an invalid text encoding, a frame shorter than its fixed
fields) is a `tag_damaged` note; the decode goes on and the good frames are kept.

## D7 Damage in the stream

Bytes that are not a frame between two frames are a `stream_gap`; before the first frame or after the last (not a
tag) `junk_outside_audio`. A cut last frame, fewer frames than the header frame or the container states, or a stream
that ends with a read error is `truncated`, counted once per cut. A stated length longer than the decode by less than
one packet is rounding (an edit list in a coarse timescale), not a cut. A change of rate, layer or channel count is a
`format_change`.

## D8 Undecodable frames keep the timeline

A frame that fails to decode becomes silence of its length (`undecodable_frames`), so the audio after it keeps its
place in time.

## D9 A cut MP4

An MP4 whose sample data runs past the end of the file (the `moov` before it) is decoded up to the cut, with a
`truncated` issue. With the `moov` after the cut nothing indexes the samples: an error.

## D10 Hostile input

Every size in a file is untrusted; the readers never index past their data. A panic inside a decoder library is
caught and becomes `Error::DecoderFailure`. `Options::max_bytes` and `max_seconds` bound a service's work.

## D11 A decode below the file's quality is said

An AAC track's `AudioSpecificConfig` gives its profile (`Report::profile`): AAC-LC, HE-AAC (SBR), HE-AAC v2 (SBR +
PS), xHE-AAC (USAC). Where the decoder lacks a layer the file declares (the portable decoder has neither SBR nor PS),
the report adds the note `reduced_quality`: the core decodes on the same timeline, at half the rate and bandwidth, mono
for v2. An AAC-LC configuration that does not state SBR either way, at a core rate SBR could double (24 kHz or less),
and an ADTS stream at such a rate, may carry SBR unannounced (implicit signalling): the note says "possibly". A
profile the decoder cannot read at all (xHE-AAC, AAC Main, ...) is `Error::UnsupportedCodec` naming it. A decode above
the declared core rate applied SBR: no note (the system decoder on Apple platforms, which also states SBR for a bare
low-rate configuration, `symphonia-adapter-applecoreaudio` DESIGN A2).

## D12 One AAC decoder per build, ADTS everywhere

On Apple platforms the build holds no AAC decoder but the system's: symphonia's `aac` feature is off there (the
crate's `symphonia-aac` feature turns it on, the tests always do), so `Backend::Portable` reads no AAC on Apple
platforms in a normal build (`Error::UnsupportedCodec`). Elsewhere symphonia's AAC decoder is in. Raw AAC streams
(ADTS, `.aac`) go through the adapter's ADTS reader on every platform, ahead of symphonia's: junk inside such a
stream is a `stream_gap` and the decode goes on; nothing states a trim, so the default 2112 frames apply, counted at
the stream's rate and scaled to the decoded rate (4224 for HE-AAC with SBR).
