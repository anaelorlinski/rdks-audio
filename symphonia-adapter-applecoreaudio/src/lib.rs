#![doc = include_str!("../README.md")]

mod adts;
#[cfg(target_vendor = "apple")]
mod apple;

pub use adts::{AdtsReader, register_formats};

#[cfg(target_vendor = "apple")]
pub use apple::AppleAacDecoder;
use symphonia_core::codecs::registry::CodecRegistry;
use symphonia_core::common::Tier;

/// Registers the system AAC decoder in `registry` ahead of every other AAC decoder ([`Tier::Preferred`]). Does nothing
/// on platforms other than Apple's, so callers need no platform condition (DESIGN A1).
pub fn register(registry: &mut CodecRegistry) {
    #[cfg(target_vendor = "apple")]
    registry.register_audio_decoder_at_tier::<AppleAacDecoder>(Tier::Preferred);
    #[cfg(not(target_vendor = "apple"))]
    let _ = (registry, Tier::Preferred);
}

/// The system AAC decoder is there: built for an Apple platform.
pub const AVAILABLE: bool = cfg!(target_vendor = "apple");
