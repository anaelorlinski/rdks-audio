# rdks-audio

Audio crates, each usable on its own, under MIT OR Apache-2.0.

| Crate | What it does |
|---|---|
| [`rdks-audio-decode`](rdks-audio-decode) | Decodes audio files from their exact first sample, best effort, with a report of what was wrong in the file (notes and damage) |
| [`symphonia-adapter-applecoreaudio`](symphonia-adapter-applecoreaudio) | A [Symphonia](https://github.com/pdeljanov/Symphonia) AAC decoder backed by the system decoder of Apple platforms (HE-AAC and xHE-AAC in full), and an ADTS reader |

Planned: more decoder adapters for Symphonia, named `symphonia-adapter-*` (Media Foundation, MediaCodec, WebCodecs).

## Building

```sh
cargo test --workspace
```

## Licence

Each crate is licensed under either of the Apache License, Version 2.0 or the MIT licence, at your option. Unless you
explicitly state otherwise, any contribution intentionally submitted for inclusion in a crate by you, as defined in the
Apache-2.0 licence, shall be dual licensed as above, without any additional terms or conditions.
