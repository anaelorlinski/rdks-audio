# Changelog

## Unreleased

- MP3: a stream cut at its start (its first frames need bit-reservoir bytes of frames that are not in the file) is
  `truncated` (DESIGN D13).
- FLAC: an ID3v1 and / or APEv2 tag after the last frame is left out of the decode, a `junk_outside_audio` note; the
  last frame was lost and the file reported `truncated` (DESIGN D14).

## 0.1.0 (unreleased)

- First version: symphonia 0.6.1 for every format; AAC through the system's decoder on Apple platforms
  (`symphonia-adapter-applecoreaudio`, `Backend::Platform`), in the same decode loop, trimmed by the same rule.
- MP3: own stream walker (ID3v2 checks, header frame, LAME extension and its checksum, gaps, cuts, trailing tags);
  untrimmed decode cut by the stated delay and padding.
- No symphonia AAC decoder in Apple builds (feature `symphonia-aac` to add it); ADTS through the adapter's reader on
  every platform, junk inside a stream a `stream_gap`.
- MP4: AudioToolbox's trim rule on every platform, its counts scaled to the decoded rate (HE-AAC with SBR); a file cut inside its sample data decodes up to the cut.
- AAC profile from the `AudioSpecificConfig` (`Report::profile`); `reduced_quality` note when the decoder gives
  HE-AAC without SBR / PS; an unsupported profile (xHE-AAC, ...) named in the error.
- The report: notes and damage, `check` without keeping samples, size and duration limits, a panic guard.
