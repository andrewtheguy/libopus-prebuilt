//! Raw FFI for libopus, linked from a **prebuilt static archive** — so nothing that
//! depends on this crate needs cmake, a C compiler, pkg-config, or a network.
//!
//! This is a drop-in stand-in for `audiopus_sys` as far as the safe wrapper next door
//! ([`opus-prebuilt`](../opus-prebuilt)) is concerned, and it exists for one reason:
//! `audiopus_sys` *builds* opus, with cmake, on every clean build of every project. See
//! the repository README for why that was worth removing.
//!
//! Two halves, deliberately different in origin:
//!
//! - the **constants** are generated from the pinned opus headers by `gen-consts.sh`,
//!   because they are bare integers where a typo is a runtime bug rather than a
//!   compile error;
//! - the **function declarations** are written out below, transcribed from those same
//!   headers, because a wrong signature *is* a compile error at every call site and
//!   because the alternative — a 74 KB bindgen dump — is not reviewable.
//!
//! Only what the safe wrapper calls is declared. This is not a complete binding to
//! libopus 1.6, and in particular the 1.5+ DRED and OSCE entry points are absent
//! because the archives are built without them (`OPUS_DRED=OFF`, `OPUS_OSCE=OFF`).

#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_int, c_uchar};

pub type opus_int16 = i16;
pub type opus_int32 = i32;
pub type opus_uint32 = u32;

include!("consts.rs");

// Opaque, and must stay opaque: every one of these is allocated by libopus with a size
// that depends on the channel count and on how opus itself was configured. Rust only
// ever holds the pointer.
macro_rules! opaque {
    ($($name:ident),* $(,)?) => { $(
        #[repr(C)]
        #[derive(Debug, Copy, Clone)]
        pub struct $name {
            _opaque: [u8; 0],
        }
    )* };
}
opaque!(OpusEncoder, OpusDecoder, OpusMSEncoder, OpusMSDecoder, OpusRepacketizer);

extern "C" {
    // ---------------------------------------------------------------- encoder
    pub fn opus_encoder_create(
        Fs: opus_int32,
        channels: c_int,
        application: c_int,
        error: *mut c_int,
    ) -> *mut OpusEncoder;
    pub fn opus_encoder_destroy(st: *mut OpusEncoder);
    pub fn opus_encode(
        st: *mut OpusEncoder,
        pcm: *const opus_int16,
        frame_size: c_int,
        data: *mut c_uchar,
        max_data_bytes: opus_int32,
    ) -> opus_int32;
    pub fn opus_encode_float(
        st: *mut OpusEncoder,
        pcm: *const f32,
        frame_size: c_int,
        data: *mut c_uchar,
        max_data_bytes: opus_int32,
    ) -> opus_int32;
    /// Variadic in C, and genuinely so: the third argument's type depends on the
    /// request. The safe wrapper is what pairs each `OPUS_*_REQUEST` with the right one.
    pub fn opus_encoder_ctl(st: *mut OpusEncoder, request: c_int, ...) -> c_int;

    // ---------------------------------------------------------------- decoder
    pub fn opus_decoder_create(
        Fs: opus_int32,
        channels: c_int,
        error: *mut c_int,
    ) -> *mut OpusDecoder;
    pub fn opus_decoder_destroy(st: *mut OpusDecoder);
    pub fn opus_decode(
        st: *mut OpusDecoder,
        data: *const c_uchar,
        len: opus_int32,
        pcm: *mut opus_int16,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;
    pub fn opus_decode_float(
        st: *mut OpusDecoder,
        data: *const c_uchar,
        len: opus_int32,
        pcm: *mut f32,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;
    pub fn opus_decoder_ctl(st: *mut OpusDecoder, request: c_int, ...) -> c_int;
    pub fn opus_decoder_get_nb_samples(
        dec: *const OpusDecoder,
        packet: *const c_uchar,
        len: opus_int32,
    ) -> c_int;

    // ---------------------------------------------------- multistream encoder
    pub fn opus_multistream_encoder_create(
        Fs: opus_int32,
        channels: c_int,
        streams: c_int,
        coupled_streams: c_int,
        mapping: *const c_uchar,
        application: c_int,
        error: *mut c_int,
    ) -> *mut OpusMSEncoder;
    pub fn opus_multistream_encoder_destroy(st: *mut OpusMSEncoder);
    pub fn opus_multistream_encode(
        st: *mut OpusMSEncoder,
        pcm: *const opus_int16,
        frame_size: c_int,
        data: *mut c_uchar,
        max_data_bytes: opus_int32,
    ) -> c_int;
    pub fn opus_multistream_encode_float(
        st: *mut OpusMSEncoder,
        pcm: *const f32,
        frame_size: c_int,
        data: *mut c_uchar,
        max_data_bytes: opus_int32,
    ) -> c_int;
    pub fn opus_multistream_encoder_ctl(st: *mut OpusMSEncoder, request: c_int, ...) -> c_int;

    // ---------------------------------------------------- multistream decoder
    pub fn opus_multistream_decoder_create(
        Fs: opus_int32,
        channels: c_int,
        streams: c_int,
        coupled_streams: c_int,
        mapping: *const c_uchar,
        error: *mut c_int,
    ) -> *mut OpusMSDecoder;
    pub fn opus_multistream_decoder_destroy(st: *mut OpusMSDecoder);
    pub fn opus_multistream_decode(
        st: *mut OpusMSDecoder,
        data: *const c_uchar,
        len: opus_int32,
        pcm: *mut opus_int16,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;
    pub fn opus_multistream_decode_float(
        st: *mut OpusMSDecoder,
        data: *const c_uchar,
        len: opus_int32,
        pcm: *mut f32,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;
    pub fn opus_multistream_decoder_ctl(st: *mut OpusMSDecoder, request: c_int, ...) -> c_int;

    // ---------------------------------------------------------- repacketizer
    pub fn opus_repacketizer_create() -> *mut OpusRepacketizer;
    pub fn opus_repacketizer_destroy(rp: *mut OpusRepacketizer);
    pub fn opus_repacketizer_init(rp: *mut OpusRepacketizer) -> *mut OpusRepacketizer;
    pub fn opus_repacketizer_cat(
        rp: *mut OpusRepacketizer,
        data: *const c_uchar,
        len: opus_int32,
    ) -> c_int;
    pub fn opus_repacketizer_get_nb_frames(rp: *mut OpusRepacketizer) -> c_int;
    pub fn opus_repacketizer_out(
        rp: *mut OpusRepacketizer,
        data: *mut c_uchar,
        maxlen: opus_int32,
    ) -> opus_int32;
    pub fn opus_repacketizer_out_range(
        rp: *mut OpusRepacketizer,
        begin: c_int,
        end: c_int,
        data: *mut c_uchar,
        maxlen: opus_int32,
    ) -> opus_int32;

    // ------------------------------------------------- packets, and the rest
    pub fn opus_packet_pad(data: *mut c_uchar, len: opus_int32, new_len: opus_int32) -> c_int;
    pub fn opus_packet_unpad(data: *mut c_uchar, len: opus_int32) -> opus_int32;
    pub fn opus_multistream_packet_pad(
        data: *mut c_uchar,
        len: opus_int32,
        new_len: opus_int32,
        nb_streams: c_int,
    ) -> c_int;
    pub fn opus_multistream_packet_unpad(
        data: *mut c_uchar,
        len: opus_int32,
        nb_streams: c_int,
    ) -> opus_int32;
    pub fn opus_packet_parse(
        data: *const c_uchar,
        len: opus_int32,
        out_toc: *mut c_uchar,
        frames: *mut *const c_uchar,
        size: *mut opus_int16,
        payload_offset: *mut c_int,
    ) -> c_int;
    pub fn opus_packet_get_bandwidth(data: *const c_uchar) -> c_int;
    pub fn opus_packet_get_nb_channels(data: *const c_uchar) -> c_int;
    pub fn opus_packet_get_nb_frames(packet: *const c_uchar, len: opus_int32) -> c_int;
    pub fn opus_packet_get_nb_samples(
        packet: *const c_uchar,
        len: opus_int32,
        Fs: opus_int32,
    ) -> c_int;
    pub fn opus_packet_get_samples_per_frame(data: *const c_uchar, Fs: opus_int32) -> c_int;
    pub fn opus_pcm_soft_clip(
        pcm: *mut f32,
        frame_size: c_int,
        channels: c_int,
        softclip_mem: *mut f32,
    );
    pub fn opus_strerror(error: c_int) -> *const c_char;
    pub fn opus_get_version_string() -> *const c_char;
}

/// The opus release the linked archive was built from, as libopus reports it.
///
/// Worth having beyond curiosity: it is the one check that proves *which* library a
/// binary ended up linked against. A stray system libopus that got picked up instead
/// says so here.
pub fn version() -> &'static str {
    // Safe: libopus returns a pointer to a static string literal, always non-null.
    unsafe { std::ffi::CStr::from_ptr(opus_get_version_string()) }
        .to_str()
        .unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    /// Linking is most of what this crate does, so a test that resolves a symbol and
    /// reads a value back out of the library is a real check on it — and on the archive
    /// actually being the pinned version rather than something found on the system.
    #[test]
    fn links_the_pinned_version() {
        let version = super::version();
        assert!(
            version.starts_with(concat!("libopus ", env!("LIBOPUS_PREBUILT_VERSION"))),
            "linked {version:?}, expected libopus {}",
            env!("LIBOPUS_PREBUILT_VERSION"),
        );
    }
}
