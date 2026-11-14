# Changelog

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
