//! The decoder over AudioToolbox's `AudioConverter`: one call per packet, untrimmed output (DESIGN A1 to A5).

use std::ffi::c_void;
use std::fmt::Write as _;
use std::mem::size_of;
use std::ptr::{self, NonNull};

use objc2_audio_toolbox::{
    AudioConverterDispose, AudioConverterFillComplexBuffer, AudioConverterNew, AudioConverterRef, AudioConverterReset,
    AudioConverterSetProperty, AudioFormatGetProperty, AudioFormatGetPropertyInfo, AudioFormatInfo,
    kAudioConverterDecompressionMagicCookie, kAudioFormatProperty_FirstPlayableFormatFromList,
    kAudioFormatProperty_FormatList,
};
use objc2_core_audio_types::{
    AudioBuffer as CaBuffer, AudioBufferList, AudioFormatListItem, AudioStreamBasicDescription,
    AudioStreamPacketDescription, kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked, kAudioFormatLinearPCM,
    kAudioFormatMPEG4AAC, kAudioFormatMPEGD_USAC,
};
use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, Channels, GenericAudioBufferRef, Position,
};
use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia_core::codecs::audio::well_known::profiles::{
    CODEC_PROFILE_AAC_HE, CODEC_PROFILE_AAC_HE_V2, CODEC_PROFILE_AAC_LC, CODEC_PROFILE_AAC_USAC,
};
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::codecs::{CodecInfo, CodecProfileInfo};
use symphonia_core::errors::{Error, Result, unsupported_error};
use symphonia_core::packet::PacketRef;

/// What the input callback returns once its one packet is given: the converter stops asking and returns what it
/// decoded (any value that is not an AudioToolbox status works; this is the four characters `rdks`).
const NO_MORE_INPUT: i32 = i32::from_be_bytes(*b"rdks");
/// Output frames to make room for when the format states none per packet (the largest AAC frame, 4096).
const MAX_FRAMES_PER_PACKET: usize = 4096;
/// The MPEG-4 `objectTypeIndication` of MPEG-4 audio in a decoder config descriptor.
const MPEG4_AUDIO: u8 = 0x40;
/// `streamType` 5 (audio) << 2 | the reserved bit, in a decoder config descriptor.
const AUDIO_STREAM: u8 = 0x15;
/// The audio object type of USAC (xHE-AAC).
const USAC: u32 = 42;
/// The audio object type of AAC-LC.
const AAC_LC: u32 = 2;
/// The audio object type of SBR, in an extension of the configuration.
const SBR: u64 = 5;
/// The sync word of a backward-compatible extension of the configuration (ISO/IEC 14496-3 1.6.5.2).
const SYNC_EXTENSION: u64 = 0x2b7;
/// Rate indices of a core SBR can double, 24 kHz (6) to 8 kHz (11): the doubled rate's index is 3 lower.
const SBR_CORE_RATES: std::ops::RangeInclusive<u8> = 6..=11;

/// The codec this decoder registers for.
const SUPPORTED: &[SupportedAudioCodec] = &[SupportedAudioCodec {
    id: CODEC_ID_AAC,
    info: CodecInfo {
        short_name: "aac",
        long_name: "Advanced Audio Coding (Apple AudioToolbox)",
        profiles: &[
            CodecProfileInfo { profile: CODEC_PROFILE_AAC_LC, short_name: "aac-lc", long_name: "Low Complexity" },
            CodecProfileInfo { profile: CODEC_PROFILE_AAC_HE, short_name: "he-aac", long_name: "High Efficiency" },
            CodecProfileInfo {
                profile: CODEC_PROFILE_AAC_HE_V2,
                short_name: "he-aac-v2",
                long_name: "High Efficiency v2",
            },
            CodecProfileInfo {
                profile: CODEC_PROFILE_AAC_USAC,
                short_name: "xhe-aac",
                long_name: "Extended High Efficiency (USAC)",
            },
        ],
    },
}];

/// An `AudioConverter`, disposed of on drop.
struct Converter(AudioConverterRef);

// SAFETY: an AudioConverter may be used from any thread, one call at a time. The handle is only used through
// `&mut AppleAacDecoder` (decode, reset) and on drop, never through `&self`, so moving the decoder to another thread
// or sharing `&AppleAacDecoder` cannot run two calls on it at once.
unsafe impl Send for Converter {}
// SAFETY: see `Send`: no method taking `&self` touches the handle.
unsafe impl Sync for Converter {}

impl Drop for Converter {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful AudioConverterNew and is disposed of once, here.
        unsafe { AudioConverterDispose(self.0) };
    }
}

/// The one packet the input callback hands over.
struct Feed {
    data: *const u8,
    len: u32,
    channels: u32,
    desc: AudioStreamPacketDescription,
    given: bool,
}

/// `AudioConverterComplexInputDataProc`: gives the packet once, then says there is no more input.
unsafe extern "C-unwind" fn give_packet(
    _converter: AudioConverterRef,
    io_packets: NonNull<u32>,
    io_data: NonNull<AudioBufferList>,
    out_desc: *mut *mut AudioStreamPacketDescription,
    user: *mut c_void,
) -> i32 {
    // SAFETY: `user` is the `Feed` that `decode_packet` passes to AudioConverterFillComplexBuffer, alive and not
    // otherwise borrowed for the length of that call, which is the only time the converter calls back.
    let feed = unsafe { &mut *user.cast::<Feed>() };
    if feed.given {
        // SAFETY: the converter passes a valid packet count and buffer list; nothing is handed over this time, so
        // the buffer and the description are emptied too.
        unsafe {
            io_packets.write(0);
            let list = io_data.as_ptr();
            (*list).mBuffers[0].mDataByteSize = 0;
            (*list).mBuffers[0].mData = ptr::null_mut();
            if !out_desc.is_null() {
                *out_desc = ptr::null_mut();
            }
        }
        return NO_MORE_INPUT;
    }
    feed.given = true;
    // SAFETY: the converter passes a buffer list with at least one buffer and a valid packet count; the packet's
    // bytes stay alive until FillComplexBuffer returns (the converter only reads them); `out_desc`, when not null,
    // receives a pointer to the description, which lives in `feed` for the same time.
    unsafe {
        let list = io_data.as_ptr();
        (*list).mNumberBuffers = 1;
        (*list).mBuffers[0] =
            CaBuffer { mNumberChannels: feed.channels, mDataByteSize: feed.len, mData: feed.data.cast_mut().cast() };
        io_packets.write(1);
        if !out_desc.is_null() {
            *out_desc = &raw mut feed.desc;
        }
    }
    0
}

/// An AAC decoder (AAC-LC, HE-AAC v1 and v2, xHE-AAC) over AudioToolbox. It decodes every layer the stream has (SBR
/// at twice the core rate, PS to stereo) and outputs every frame, the encoder delay included: trims are the
/// caller's, as with Symphonia's own AAC decoder.
pub struct AppleAacDecoder {
    converter: Converter,
    params: AudioCodecParameters,
    buf: AudioBuffer<f32>,
    scratch: Vec<f32>,
    channels: usize,
    frames_per_packet: usize,
    /// The converter was reset after an error: the next packet's output, shorter by the decoder's start-up delay,
    /// is padded at its front to a whole packet so the timeline does not move.
    refill: bool,
}

impl AppleAacDecoder {
    /// A decoder for an AAC track: its `AudioSpecificConfig` in the parameters' extra data (MP4), or the rate and
    /// channels alone (ADTS: decoded as AAC-LC).
    ///
    /// # Errors
    ///
    /// `Unsupported` when the track is not AAC or AudioToolbox has no decoder for its configuration.
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_AAC {
            return unsupported_error("applecoreaudio: not AAC");
        }
        let asc = match &params.extra_data {
            Some(asc) if asc.len() >= 2 => asc.to_vec(),
            _ => adts_config(params)?,
        };
        let asc = with_implicit_sbr(&asc).unwrap_or(asc);
        let cookie = es_descriptor(&asc);
        let format_id = if object_type(&asc) == Some(USAC) { kAudioFormatMPEGD_USAC } else { kAudioFormatMPEG4AAC };
        let input = best_layer(format_id, &cookie)?;
        let channels = input.mChannelsPerFrame as usize;
        if channels == 0 || input.mSampleRate <= 0.0 {
            return unsupported_error("applecoreaudio: no channels or rate");
        }
        let output = float_output(input.mSampleRate, input.mChannelsPerFrame);
        let mut handle: AudioConverterRef = ptr::null_mut();
        // SAFETY: both descriptions and the handle slot are valid for the call.
        let status =
            unsafe { AudioConverterNew(NonNull::from(&input), NonNull::from(&output), NonNull::from(&mut handle)) };
        if status != 0 || handle.is_null() {
            return unsupported_error("applecoreaudio: no AudioToolbox decoder for this configuration");
        }
        let converter = Converter(handle);
        // SAFETY: the converter is valid; the cookie is read during the call only.
        let status = unsafe {
            AudioConverterSetProperty(
                converter.0,
                kAudioConverterDecompressionMagicCookie,
                u32::try_from(cookie.len()).unwrap_or(u32::MAX),
                NonNull::from(cookie.as_slice()).cast(),
            )
        };
        if status != 0 {
            return unsupported_error("applecoreaudio: the decoder refused the configuration");
        }
        let frames_per_packet = match input.mFramesPerPacket {
            0 => MAX_FRAMES_PER_PACKET,
            n => n as usize,
        };
        let rate = input.mSampleRate.round() as u32;
        let layout = layout(params, channels);
        let mut params = params.clone();
        params.with_sample_rate(rate).with_channels(layout.clone());
        Ok(AppleAacDecoder {
            converter,
            params,
            buf: AudioBuffer::new(AudioSpec::new(rate, layout), frames_per_packet),
            scratch: vec![0.0; frames_per_packet * channels],
            channels,
            frames_per_packet,
            refill: false,
        })
    }

    fn decode_packet(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        self.buf.clear();
        let len =
            u32::try_from(packet.data.len()).map_err(|_| Error::DecodeError("applecoreaudio: packet too large"))?;
        let mut feed = Feed {
            data: packet.data.as_ptr(),
            len,
            channels: self.channels as u32,
            desc: AudioStreamPacketDescription { mStartOffset: 0, mVariableFramesInPacket: 0, mDataByteSize: len },
            given: false,
        };
        let mut frames = self.frames_per_packet as u32;
        let mut out = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [CaBuffer {
                mNumberChannels: self.channels as u32,
                mDataByteSize: (self.scratch.len() * size_of::<f32>()) as u32,
                mData: self.scratch.as_mut_ptr().cast(),
            }],
        };
        // SAFETY: the converter is valid; `feed` and `out` (over `scratch`, sized for `frames` frames of `channels`
        // samples) live for the call; the callback is `give_packet` above.
        let status = unsafe {
            AudioConverterFillComplexBuffer(
                self.converter.0,
                Some(give_packet),
                (&raw mut feed).cast(),
                NonNull::from(&mut frames),
                NonNull::from(&mut out),
                ptr::null_mut(),
            )
        };
        if status != 0 && status != NO_MORE_INPUT {
            // the next packet starts from a clean decoder
            // SAFETY: the converter is valid.
            unsafe { AudioConverterReset(self.converter.0) };
            self.refill = true;
            let mut what = String::from("applecoreaudio: ");
            push_status(&mut what, status);
            return Err(Error::DecodeError(leak_reason(what)));
        }
        // the frames written, bounded by the bytes the converter reports and by the buffer
        let written = out.mBuffers[0].mDataByteSize as usize / (size_of::<f32>() * self.channels);
        let n = (frames as usize).min(written).min(self.frames_per_packet);
        if n == 0 {
            // AudioToolbox drops a packet it cannot decode without an error status
            return Err(Error::DecodeError("applecoreaudio: the decoder dropped the packet (damaged data)"));
        }
        let pad = if std::mem::take(&mut self.refill) { self.frames_per_packet - n } else { 0 };
        self.buf.render_uninit(Some(pad + n));
        let samples = &self.scratch[..n * self.channels];
        for (c, plane) in self.buf.iter_planes_mut().enumerate() {
            plane[..pad].fill(0.0);
            for (dst, frame) in plane[pad..].iter_mut().zip(samples.chunks_exact(self.channels)) {
                *dst = frame[c];
            }
        }
        Ok(())
    }
}

impl AudioDecoder for AppleAacDecoder {
    fn reset(&mut self) {
        // a seek: the next packet starts a new stream, start-up delay included
        // SAFETY: the converter is valid.
        unsafe { AudioConverterReset(self.converter.0) };
        self.refill = false;
    }

    fn codec_info(&self) -> &CodecInfo {
        &SUPPORTED[0].info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        if let Err(e) = self.decode_packet(packet) {
            self.buf.clear();
            return Err(e);
        }
        Ok(self.buf.as_generic_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for AppleAacDecoder {
    fn try_registry_new(params: &AudioCodecParameters, opts: &AudioDecoderOptions) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(AppleAacDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        SUPPORTED
    }
}

/// Symphonia's errors carry a static reason; a status is known only at run time. The few distinct statuses a
/// decoder returns are leaked once each.
fn leak_reason(what: String) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut seen = SEEN.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(s) = seen.get(what.as_str()) {
        return s;
    }
    let s: &'static str = Box::leak(what.into_boxed_str());
    seen.insert(s);
    s
}

/// An OSStatus as its four characters when printable (`'!dat'`), else as a number.
fn push_status(out: &mut String, status: i32) {
    let b = status.to_be_bytes();
    if b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
        let _ = write!(out, "'{}'", String::from_utf8_lossy(&b));
    } else {
        let _ = write!(out, "status {status}");
    }
}

/// The audio object type of an `AudioSpecificConfig` (5 bits, 31 = escape to 32 + 6 bits).
fn object_type(asc: &[u8]) -> Option<u32> {
    let head = u32::from(*asc.first()?) << 8 | u32::from(*asc.get(1)?);
    match head >> 11 {
        31 => Some(32 + ((head >> 5) & 0x3f)),
        t => Some(t),
    }
}

/// A bare AAC-LC configuration at a core rate SBR can double, with SBR stated through a backward-compatible
/// extension, so the decoder applies SBR when the stream carries it (implicit signalling, as ADTS streams of HE-AAC
/// do); a stream without SBR then decodes at twice its rate (DESIGN A2). None for any other configuration.
fn with_implicit_sbr(asc: &[u8]) -> Option<Vec<u8>> {
    let [a, b] = *asc else { return None };
    let rate_index = (b >> 7) | ((a & 0x07) << 1);
    if object_type(asc)? != AAC_LC || !SBR_CORE_RATES.contains(&rate_index) {
        return None;
    }
    let bits = u64::from(u16::from_be_bytes([a, b])) << 21
        | SYNC_EXTENSION << 10
        | SBR << 5
        | 1 << 4
        | u64::from(rate_index - 3);
    // 37 bits, left-aligned in 5 bytes
    Some((bits << 3).to_be_bytes()[3..].to_vec())
}

/// An AAC-LC `AudioSpecificConfig` from the rate and channel count alone (ADTS streams carry no configuration).
fn adts_config(params: &AudioCodecParameters) -> Result<Vec<u8>> {
    const FREQUENCIES: [u32; 13] =
        [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
    const AAC_LC: u16 = 2;
    let rate = params.sample_rate.unwrap_or(0);
    let Some(index) = FREQUENCIES.iter().position(|&f| f == rate) else {
        return unsupported_error("applecoreaudio: no configuration and an unusual rate");
    };
    let channels = params.channels.as_ref().map_or(2, Channels::count).min(7) as u16;
    let bits = AAC_LC << 11 | (index as u16) << 7 | channels << 3;
    Ok(bits.to_be_bytes().to_vec())
}

/// The `AudioSpecificConfig` wrapped in an MPEG-4 ES descriptor, the form AudioToolbox takes as an AAC magic cookie
/// (as the `esds` box of an MP4 file holds it): ES descriptor, decoder config descriptor, decoder specific info, SL
/// config descriptor. Sizes are written on four bytes.
fn es_descriptor(asc: &[u8]) -> Vec<u8> {
    fn descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
        let n = body.len() as u32;
        let mut d =
            vec![tag, 0x80 | (n >> 21 & 0x7f) as u8, 0x80 | (n >> 14 & 0x7f) as u8, 0x80 | (n >> 7 & 0x7f) as u8];
        d.push((n & 0x7f) as u8);
        d.extend_from_slice(body);
        d
    }
    let mut config = vec![MPEG4_AUDIO, AUDIO_STREAM];
    config.extend([0u8; 11]); // buffer size, max and average bitrates: unknown
    config.extend(descriptor(5, asc));
    let mut es = vec![0, 0, 0]; // ES id, flags
    es.extend(descriptor(4, &config));
    es.extend(descriptor(6, &[2])); // SL config: predefined MP4
    descriptor(3, &es)
}

/// The richest layer AudioToolbox can decode from this configuration (HE-AAC v2 over HE-AAC over AAC-LC).
fn best_layer(format_id: u32, cookie: &[u8]) -> Result<AudioStreamBasicDescription> {
    let info = AudioFormatInfo {
        mASBD: AudioStreamBasicDescription { mFormatID: format_id, ..zero_asbd() },
        mMagicCookie: NonNull::from(cookie).cast(),
        mMagicCookieSize: u32::try_from(cookie.len()).unwrap_or(u32::MAX),
    };
    let spec_size = size_of::<AudioFormatInfo>() as u32;
    let mut size = 0u32;
    // SAFETY: `info` and the cookie it points to are valid for the call; `size` receives the list's byte size.
    let status = unsafe {
        AudioFormatGetPropertyInfo(
            kAudioFormatProperty_FormatList,
            spec_size,
            (&raw const info).cast(),
            NonNull::from(&mut size),
        )
    };
    let count = size as usize / size_of::<AudioFormatListItem>();
    if status != 0 || count == 0 {
        return unsupported_error("applecoreaudio: AudioToolbox does not read this configuration");
    }
    let mut items = vec![AudioFormatListItem { mASBD: zero_asbd(), mChannelLayoutTag: 0 }; count];
    // SAFETY: `items` holds `size` bytes of list items.
    let status = unsafe {
        AudioFormatGetProperty(
            kAudioFormatProperty_FormatList,
            spec_size,
            (&raw const info).cast(),
            &raw mut size,
            items.as_mut_ptr().cast(),
        )
    };
    if status != 0 {
        return unsupported_error("applecoreaudio: AudioToolbox does not read this configuration");
    }
    items.truncate(size as usize / size_of::<AudioFormatListItem>());
    let mut index = 0u32;
    let mut index_size = size_of::<u32>() as u32;
    // SAFETY: the specifier is the list just read; the output is one u32.
    let status = unsafe {
        AudioFormatGetProperty(
            kAudioFormatProperty_FirstPlayableFormatFromList,
            (items.len() * size_of::<AudioFormatListItem>()) as u32,
            items.as_ptr().cast(),
            &raw mut index_size,
            (&raw mut index).cast(),
        )
    };
    let index = if status == 0 { index as usize } else { 0 };
    items.get(index).or(items.first()).map(|i| i.mASBD).ok_or(Error::Unsupported("applecoreaudio: no playable layer"))
}

fn zero_asbd() -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: 0.0,
        mFormatID: 0,
        mFormatFlags: 0,
        mBytesPerPacket: 0,
        mFramesPerPacket: 0,
        mBytesPerFrame: 0,
        mChannelsPerFrame: 0,
        mBitsPerChannel: 0,
        mReserved: 0,
    }
}

/// Interleaved native f32 at the decoder's rate and channel count (no resampling, no channel mixing).
fn float_output(rate: f64, channels: u32) -> AudioStreamBasicDescription {
    const BYTES: u32 = size_of::<f32>() as u32;
    AudioStreamBasicDescription {
        mSampleRate: rate,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
        mBytesPerPacket: BYTES * channels,
        mFramesPerPacket: 1,
        mBytesPerFrame: BYTES * channels,
        mChannelsPerFrame: channels,
        mBitsPerChannel: BYTES * 8,
        mReserved: 0,
    }
}

/// The output's channels: the track's when the count matches, stereo for PS (a mono core decoded to two), else
/// discrete.
fn layout(params: &AudioCodecParameters, channels: usize) -> Channels {
    match &params.channels {
        Some(c) if c.count() == channels => c.clone(),
        _ if channels == 1 => Channels::Positioned(Position::FRONT_CENTER),
        _ if channels == 2 => Channels::Positioned(Position::FRONT_LEFT | Position::FRONT_RIGHT),
        _ => Channels::Discrete(channels as u16),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_types() {
        // DESIGN A2
        assert_eq!(object_type(&[0x12, 0x10]), Some(2));
        assert_eq!(object_type(&[0x2b, 0x92]), Some(5));
        assert_eq!(object_type(&[0xf9, 0x46]), Some(42));
        assert_eq!(object_type(&[0x12]), None);
    }

    #[test]
    fn implicit_sbr_is_stated_like_apple_s_encoder_does() {
        // DESIGN A2: he1.m4a's configuration, from its bare AAC-LC 22.05 kHz stereo core
        assert_eq!(with_implicit_sbr(&[0x13, 0x90]), Some(vec![0x13, 0x90, 0x56, 0xe5, 0xa0]));
        // 44.1 kHz, already extended, or not AAC-LC: unchanged
        assert_eq!(with_implicit_sbr(&[0x12, 0x10]), None);
        assert_eq!(with_implicit_sbr(&[0x13, 0x90, 0x56, 0xe5, 0xa0]), None);
        assert_eq!(with_implicit_sbr(&[0x0b, 0x90]), None);
    }

    #[test]
    fn es_descriptor_nests_the_config() {
        // DESIGN A2
        let d = es_descriptor(&[0x12, 0x10]);
        assert_eq!(&d[..5], &[3, 0x80, 0x80, 0x80, (d.len() - 5) as u8]);
        let at = d.windows(6).position(|w| w == [5, 0x80, 0x80, 0x80, 2, 0x12]).expect("decoder specific info");
        assert_eq!(d[at + 6], 0x10);
        assert_eq!(&d[d.len() - 6..], &[6, 0x80, 0x80, 0x80, 1, 2]);
    }
}
