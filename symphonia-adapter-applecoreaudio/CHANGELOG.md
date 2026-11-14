# Changelog

## 0.1.0 (unreleased)

- First version: AAC-LC, HE-AAC v1 / v2 and xHE-AAC through AudioToolbox's `AudioConverter`, one call per packet,
  untrimmed output, registered at the preferred tier; a packet the decoder drops (damaged data) is a
  `DecodeError`; implicit SBR stated to the decoder.
- `AdtsReader` and `register_formats`: raw AAC streams on every platform, junk inside skipped and reported once.
