//! The MPEG-4 audio configuration of an AAC track (`AudioSpecificConfig`, ISO/IEC 14496-3 section 1.6.2.1): the
//! profile the file declares, so that a decoder without SBR, PS or USAC can say it gives less than the file holds
//! (DESIGN D11). Facts only, read from untrusted bytes: a configuration the reader cannot walk gives what it read so
//! far.

use std::fmt;

/// MPEG-4 audio object types the profile depends on (ISO/IEC 14496-3 table 1.17).
mod object_type {
    pub const AAC_LC: u8 = 2;
    pub const SBR: u8 = 5;
    pub const ER_BSAC: u8 = 22;
    pub const PS: u8 = 29;
    pub const USAC: u8 = 42;
    /// The general audio coders whose `GASpecificConfig` follows the header.
    pub const GENERAL_AUDIO: [u8; 12] = [1, 2, 3, 4, 6, 7, 17, 19, 20, 21, 22, 23];
    /// The error-resilient coders, which end with an `epConfig`.
    pub const ERROR_RESILIENT: [u8; 11] = [17, 19, 20, 21, 22, 23, 24, 25, 26, 27, 39];
}

/// The sampling frequencies by index; 15 = a 24-bit rate follows.
const FREQUENCIES: [u32; 13] =
    [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
/// Sync word of the backward-compatible SBR signalling after the core's configuration.
const SYNC_SBR: u32 = 0x2b7;
/// Sync word of the PS signalling that follows it.
const SYNC_PS: u32 = 0x548;
/// The highest core rate SBR doubles: its output rate is twice the core's, at most 48 kHz. A plain AAC-LC
/// configuration at this rate or below may carry SBR without stating it (implicit signalling).
pub const IMPLICIT_SBR_MAX_CORE_RATE: u32 = 24_000;

/// What an `AudioSpecificConfig` declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AacConfig {
    /// The core's audio object type (2 = AAC-LC, 42 = USAC, ...), after an SBR / PS wrapper.
    pub object_type: u8,
    /// The core's sample rate.
    pub rate: u32,
    /// The channel configuration (0 = in a program config element).
    pub channels: u8,
    /// SBR: stated present, stated absent, or not stated (it may still be in the stream: implicit signalling).
    pub sbr: Option<bool>,
    /// PS stated present (HE-AAC v2).
    pub ps: bool,
}

/// The profile a configuration declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AacProfile {
    /// AAC-LC, SBR not stated present.
    Lc,
    /// HE-AAC: AAC-LC + SBR.
    He,
    /// HE-AAC v2: AAC-LC + SBR + PS.
    HeV2,
    /// xHE-AAC (USAC).
    Xhe,
    /// Another audio object type (Main, LTP, ELD, ...).
    Other(u8),
}

impl AacConfig {
    /// The declared profile.
    pub fn profile(&self) -> AacProfile {
        match (self.object_type, self.sbr, self.ps) {
            (object_type::USAC, ..) => AacProfile::Xhe,
            (object_type::AAC_LC, Some(true), true) => AacProfile::HeV2,
            (object_type::AAC_LC, Some(true), false) => AacProfile::He,
            (object_type::AAC_LC, ..) => AacProfile::Lc,
            (other, ..) => AacProfile::Other(other),
        }
    }

    /// AAC-LC whose SBR is not stated either way at a rate SBR could double: it may be HE-AAC.
    pub fn may_hide_sbr(&self) -> bool {
        self.object_type == object_type::AAC_LC && self.sbr.is_none() && self.rate <= IMPLICIT_SBR_MAX_CORE_RATE
    }
}

impl fmt::Display for AacProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AacProfile::Lc => f.write_str("aac-lc"),
            AacProfile::He => f.write_str("he-aac"),
            AacProfile::HeV2 => f.write_str("he-aac-v2"),
            AacProfile::Xhe => f.write_str("xhe-aac"),
            AacProfile::Other(t) => write!(f, "aac-object-type-{t}"),
        }
    }
}

/// MSB-first bit reader.
struct Bits<'a> {
    b: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn left(&self) -> usize {
        (self.b.len() * 8).saturating_sub(self.at)
    }

    fn read(&mut self, n: usize) -> Option<u32> {
        if n > 32 || self.left() < n {
            return None;
        }
        let mut v = 0u32;
        for _ in 0..n {
            let bit = (self.b[self.at / 8] >> (7 - self.at % 8)) & 1;
            v = (v << 1) | u32::from(bit);
            self.at += 1;
        }
        Some(v)
    }

    fn object_type(&mut self) -> Option<u8> {
        let t = self.read(5)?;
        let t = if t == 31 { 32 + self.read(6)? } else { t };
        u8::try_from(t).ok()
    }

    fn frequency(&mut self) -> Option<u32> {
        match self.read(4)? {
            15 => self.read(24),
            i => FREQUENCIES.get(i as usize).copied(),
        }
    }
}

/// Reads an `AudioSpecificConfig` (the payload of the MP4 `esds` decoder specific info).
pub fn parse(asc: &[u8]) -> Option<AacConfig> {
    let mut r = Bits { b: asc, at: 0 };
    let mut core = r.object_type()?;
    let rate = r.frequency()?;
    let channels = u8::try_from(r.read(4)?).ok()?;
    let (mut sbr, mut ps) = (None, false);
    // explicit hierarchical signalling: SBR / PS wrap the core's object type
    if core == object_type::SBR || core == object_type::PS {
        sbr = Some(true);
        ps = core == object_type::PS;
        r.frequency()?;
        core = r.object_type()?;
        if core == object_type::ER_BSAC {
            r.read(4)?;
        }
    }
    let config = AacConfig { object_type: core, rate, channels, sbr, ps };
    if sbr.is_some() || skip_core_config(&mut r, core, channels).is_none() {
        return Some(config);
    }
    // explicit backward-compatible signalling after the core's configuration
    if r.left() >= 16 && r.read(11) == Some(SYNC_SBR) && r.object_type() == Some(object_type::SBR) {
        let Some(present) = r.read(1) else { return Some(config) };
        sbr = Some(present == 1);
        if present == 1 && r.frequency().is_some() && r.left() >= 12 && r.read(11) == Some(SYNC_PS) {
            ps = r.read(1) == Some(1);
        }
    }
    Some(AacConfig { sbr, ps, ..config })
}

/// Walks a general audio coder's `GASpecificConfig` and `epConfig`; None where the reader cannot (a program config
/// element, error protection data, another coder's configuration).
fn skip_core_config(r: &mut Bits<'_>, core: u8, channels: u8) -> Option<()> {
    if !object_type::GENERAL_AUDIO.contains(&core) || channels == 0 {
        return None;
    }
    r.read(1)?; // frame length flag
    if r.read(1)? == 1 {
        r.read(14)?; // core coder delay
    }
    let extension = r.read(1)? == 1;
    if core == 6 || core == 20 {
        r.read(3)?; // layer number
    }
    if extension {
        if core == object_type::ER_BSAC {
            r.read(16)?; // sub frames, layer length
        }
        if [17, 19, 20, 23].contains(&core) {
            r.read(3)?; // resilience flags
        }
        r.read(1)?; // extension flag 3
    }
    if object_type::ERROR_RESILIENT.contains(&core) && r.read(2)? >= 2 {
        return None;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn profiles_of_common_encoders() {
        // DESIGN D11: Apple AAC-LC, ffmpeg AAC-LC (SBR stated absent), Apple HE-AAC and HE-AAC v2
        // (backward-compatible signalling)
        let cases = [
            ("1210", AacProfile::Lc, 44100, 2, None),
            ("121056e500", AacProfile::Lc, 44100, 2, Some(false)),
            ("139056e5a0", AacProfile::He, 22050, 2, Some(true)),
            ("138856e5a54880", AacProfile::HeV2, 22050, 1, Some(true)),
        ];
        for (asc, profile, rate, channels, sbr) in cases {
            let c = parse(&hex(asc)).unwrap();
            assert_eq!((c.profile(), c.rate, c.channels, c.sbr), (profile, rate, channels, sbr), "{asc}");
        }
        // a plain LC configuration at 22.05 kHz may hide SBR; one that states SBR absent does not
        assert!(parse(&hex("1390")).unwrap().may_hide_sbr());
        assert!(!parse(&hex("139056e500")).unwrap().may_hide_sbr());
        assert!(!parse(&hex("1210")).unwrap().may_hide_sbr());
    }

    #[test]
    fn hierarchical_and_extended_object_types() {
        // DESIGN D11: SBR (5) at 22.05 kHz stereo, output 44.1 kHz, core AAC-LC: 00101 0111 0010 0100 00010 ...
        let c = parse(&[0b0010_1011, 0b1001_0010, 0b0000_1000, 0]).unwrap();
        assert_eq!((c.profile(), c.rate, c.channels), (AacProfile::He, 22050, 2));
        // PS (29) wrapping AAC-LC, mono core
        let c = parse(&[0b1110_1011, 0b1000_1010, 0b0000_1000, 0]).unwrap();
        assert_eq!(c.profile(), AacProfile::HeV2);
        // USAC (42) through the escape: 11111 001010, 48 kHz (0011), stereo
        let c = parse(&[0b1111_1001, 0b0100_0110, 0b0100_0000]).unwrap();
        assert_eq!((c.profile(), c.rate), (AacProfile::Xhe, 48000));
        assert_eq!(c.profile().to_string(), "xhe-aac");
    }

    #[test]
    fn short_or_hostile_configs_never_panic() {
        // DESIGN D10
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&[0x12]), None);
        for b in 0..=255u8 {
            let _ = parse(&[b, b.wrapping_mul(7), 0x56, 0xe5, b]);
            let _ = parse(&[0xff, b, 0xff, 0xff, 0xff]);
        }
    }
}
