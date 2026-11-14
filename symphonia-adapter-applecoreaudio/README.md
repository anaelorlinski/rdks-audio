# symphonia-adapter-applecoreaudio

An AAC decoder for [Symphonia](https://github.com/pdeljanov/Symphonia) backed by the system decoder of Apple
platforms (AudioToolbox's `AudioConverter`): AAC-LC, HE-AAC v1 and v2 (SBR at twice the core rate, PS to stereo) and
xHE-AAC (USAC), through the operating system's licensed decoder; HE-AAC signalled implicitly (ADTS) included. With
it, a reader of ADTS streams (raw `.aac` files) for every platform.

```rust,no_run
use symphonia::core::codecs::registry::CodecRegistry;

let mut codecs = CodecRegistry::new();
symphonia::default::register_enabled_codecs(&mut codecs);
// ahead of Symphonia's own AAC decoder; does nothing on other platforms
symphonia_adapter_applecoreaudio::register(&mut codecs);

// the ADTS reader (raw `.aac` streams), on every platform
let mut probe = symphonia::core::formats::probe::Probe::default();
symphonia::default::register_enabled_formats(&mut probe);
symphonia_adapter_applecoreaudio::register_formats(&mut probe);
```

With both, an application can leave Symphonia's `aac` feature off on Apple platforms and bundle no AAC decoder of its
own.

The decoder outputs every frame of every packet, the encoder delay included, as Symphonia's own AAC decoder does:
the container's trims are the caller's. A packet AudioToolbox rejects is a `DecodeError`; the next packet decodes
from a clean decoder state. See `DESIGN.md` for the rules and what the tests measure.

## Licence

MIT OR Apache-2.0, at your option.
