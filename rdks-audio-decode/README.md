# rdks-audio-decode

Decodes audio files from their exact first sample, as far as they can be decoded, and reports what was wrong with
them.

- **Exact timeline.** Encoder delay and padding are removed as the file states them: the LAME / ffmpeg extension of
  MP3 files, the edit list or the iTunes `iTunSMPB` statement of MP4 / M4A files (Apple's rule, so every platform
  gives the same samples as AudioToolbox). Sample 0 of the output is the first sample that was encoded.
- **Best effort.** A broken tag, junk inside the stream, frames that fail to decode or a cut file never stop the
  decode: bad frames become silence of their length, so the rest keeps its place in time.
- **A report of what was wrong.** Each problem is an [`Issue`] of a [`Severity`]: a [`Severity::Note`] (the audio is
  whole: a malformed tag, a missing gapless statement, bytes outside the audio) or [`Severity::Damage`] (the audio is
  not what was encoded: undecodable frames, a skip in the stream, a cut, a format change). A player plays the file and
  warns; a checker rejects it when [`Report::is_valid`] is false.

Built on [symphonia](https://crates.io/crates/symphonia) for every format; on Apple platforms AAC goes through the
system's decoder (HE-AAC and xHE-AAC in full, `symphonia-adapter-applecoreaudio`) and the build bundles no other AAC
decoder; elsewhere through symphonia's (AAC-LC, the core of HE-AAC, with a note saying so). Feature `symphonia-aac`
adds symphonia's AAC decoder on Apple platforms too (`Backend::Portable`).

```no_run
let decoded = rdks_audio_decode::decode_file("song.mp3", &rdks_audio_decode::Options::default())?;
println!("{} frames at {} Hz", decoded.frames(), decoded.rate);
for issue in &decoded.report.issues {
    println!("{:?}: {issue}", issue.severity());
}

// a check: decode everything, keep nothing
let report = rdks_audio_decode::check_file("upload.m4a", &rdks_audio_decode::Options::default())?;
println!("valid: {}, {:.3} s", report.is_valid(), report.seconds());
# Ok::<(), rdks_audio_decode::Error>(())
```

How each rule was measured and decided: `DESIGN.md`. Test fixtures: `tests/fixtures/`, made by `tools/fixtures.py`.

## License

Licensed under either of the Apache License, Version 2.0 (`LICENSE-APACHE`) or the MIT license (`LICENSE-MIT`), at
your option.
