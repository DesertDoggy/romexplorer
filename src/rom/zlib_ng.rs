//! Minimal FFI binding to the vendored zlib-ng dynamic library, used for CHD's `zlib`/`cdzl`
//! codecs (raw DEFLATE, i.e. `inflateInit2` with `windowBits = -15`, no zlib/gzip header).
//!
//! Follows the same `libloading`-based dlopen pattern as `rahash_bytes` in
//! [`crate::rom::hash`], rather than linking directly at build time, so the rest of the
//! crate keeps working even when the vendored dylib is not present.

use std::ffi::{c_char, c_int, c_ulong, c_void};
use std::path::PathBuf;
use std::sync::OnceLock;

use libloading::{Library, Symbol};

use super::native_lib;

const MAX_WBITS: c_int = 15;
const Z_FINISH: c_int = 4;
const Z_OK: c_int = 0;
const Z_STREAM_END: c_int = 1;

// zlib-ng is built with `ZLIB_COMPAT=ON`, so it exports the classic zlib v1 ABI/symbol names
// (`inflateInit2_`, `inflate`, `inflateEnd`, ...) rather than `zng_`-prefixed ones. Confirmed via
// `nm -gU libz.dylib`.
const ZLIB_VERSION: &[u8] = b"1.3.1.zlib-ng\0";

/// Mirrors zlib's `z_stream` layout. The `uLong` fields are `c_ulong`: 8 bytes on macOS/Linux
/// (LP64) but 4 on Windows (LLP64), where a fixed `u64` makes `inflateInit2_` reject the
/// stream size. Only used opaquely via FFI; fields are read/written by the C
/// library, not by Rust code, aside from setting up input/output slices.
#[repr(C)]
struct ZStream {
    next_in: *const u8,
    avail_in: u32,
    total_in: c_ulong,
    next_out: *mut u8,
    avail_out: u32,
    total_out: c_ulong,
    msg: *const c_char,
    state: *mut c_void,
    zalloc: usize,
    zfree: usize,
    opaque: *mut c_void,
    data_type: c_int,
    adler: c_ulong,
    reserved: c_ulong,
}

impl ZStream {
    fn zeroed() -> Self {
        ZStream {
            next_in: std::ptr::null(),
            avail_in: 0,
            total_in: 0,
            next_out: std::ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: std::ptr::null(),
            state: std::ptr::null_mut(),
            zalloc: 0,
            zfree: 0,
            opaque: std::ptr::null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        }
    }
}

type InflateInit2Fn = unsafe extern "C" fn(*mut ZStream, c_int, *const c_char, c_int) -> c_int;
type InflateFn = unsafe extern "C" fn(*mut ZStream, c_int) -> c_int;
type InflateEndFn = unsafe extern "C" fn(*mut ZStream) -> c_int;

fn library() -> Option<&'static Library> {
    static LIB: OnceLock<Option<Library>> = OnceLock::new();
    LIB.get_or_init(|| {
        let path = find_zlib_ng_library()?;
        unsafe { Library::new(path).ok() }
    })
    .as_ref()
}

/// Tries `app_root_ex/lib/<os>/libz.<ext>` (`zlib.dll` on Windows, which is what zlib-ng's
/// CMake build names it there) first
/// (`scripts/move_libs_to_app_root.sh`'s flat layout), falling back to zlib-ng's own
/// submodule build tree (`deps/zlib-ng/release/<platform>/<arch>/<latest-
/// version>/dynamic/libz.<ext>`, via the same `native_lib::find_native_library` helper
/// chdman/dolphinrvz already use). **Replaces** a previous hardcoded, macOS-arm64-only
/// path under `deps/7-Zip-zstd/deps/zlib-ng/...` that didn't correspond to
/// anything either build script actually produces (confirmed not present on disk in
/// this checkout on any platform) -- the real build output was always at
/// `deps/zlib-ng/release/...`, just never looked for there.
fn find_zlib_ng_library() -> Option<PathBuf> {
    let base_name = if cfg!(target_os = "windows") { "zlib" } else { "z" };
    if let Some(path) = native_lib::find_app_root_library(base_name) {
        return Some(path);
    }
    let release_root = native_lib::source_root().join("deps/zlib-ng/release");
    native_lib::find_native_library(&release_root, base_name)
}

/// Raw-inflate (no zlib/gzip header, `windowBits = -15`) `src` into exactly `dest_len` bytes,
/// mirroring `zlib_codec_decompress` in `libchdr_chd.c`. Returns `None` if the library could not
/// be loaded, or the data failed to decompress to exactly `dest_len` bytes.
pub fn raw_inflate(src: &[u8], dest_len: usize) -> Option<Vec<u8>> {
    let lib = library()?;

    unsafe {
        let inflate_init2: Symbol<InflateInit2Fn> = lib.get(b"inflateInit2_").ok()?;
        let inflate: Symbol<InflateFn> = lib.get(b"inflate").ok()?;
        let inflate_end: Symbol<InflateEndFn> = lib.get(b"inflateEnd").ok()?;

        let mut stream = ZStream::zeroed();
        let init_rc = inflate_init2(
            &mut stream,
            -MAX_WBITS,
            ZLIB_VERSION.as_ptr() as *const c_char,
            std::mem::size_of::<ZStream>() as c_int,
        );
        if init_rc != Z_OK {
            return None;
        }

        let mut dest = vec![0u8; dest_len];
        stream.next_in = src.as_ptr();
        stream.avail_in = src.len() as u32;
        stream.next_out = dest.as_mut_ptr();
        stream.avail_out = dest.len() as u32;

        let rc = inflate(&mut stream, Z_FINISH);
        let total_out = stream.total_out as u64;
        inflate_end(&mut stream);

        if (rc != Z_STREAM_END && rc != Z_OK) || total_out != dest_len as u64 {
            return None;
        }

        Some(dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_inflate_round_trips_known_deflate_stream() {
        // "hello world" raw-deflated (windowBits=-15) with Python's zlib.compressobj(wbits=-15).
        let compressed: [u8; 13] = [
            0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0x28, 0xcf, 0x2f, 0xca, 0x49, 0x01, 0x00,
        ];
        let out = raw_inflate(&compressed, b"hello world".len()).expect("inflate should succeed");
        assert_eq!(out, b"hello world");
    }
}

type DeflateInit2Fn = unsafe extern "C" fn(*mut ZStream, c_int, c_int, c_int, c_int, c_int, *const c_char, c_int) -> c_int;
type DeflateFn = unsafe extern "C" fn(*mut ZStream, c_int) -> c_int;
type DeflateEndFn = unsafe extern "C" fn(*mut ZStream) -> c_int;
type DeflateBoundFn = unsafe extern "C" fn(*mut ZStream, c_ulong) -> c_ulong;

const Z_DEFLATED: c_int = 8;

/// Whether the zlib-ng library loaded.
pub fn available() -> bool {
    library().is_some()
}

/// Raw-deflates (no zlib/gzip header, `windowBits = -15`) `src` at `level` (1..=9), as CSO
/// blocks are stored. `None` if the library could not be loaded or compression failed.
pub fn raw_deflate(src: &[u8], level: i32) -> Option<Vec<u8>> {
    let lib = library()?;
    unsafe {
        let init: Symbol<DeflateInit2Fn> = lib.get(b"deflateInit2_").ok()?;
        let deflate: Symbol<DeflateFn> = lib.get(b"deflate").ok()?;
        let end: Symbol<DeflateEndFn> = lib.get(b"deflateEnd").ok()?;
        let bound: Symbol<DeflateBoundFn> = lib.get(b"deflateBound").ok()?;

        let mut stream = ZStream::zeroed();
        // memLevel 9 and the default strategy: what maxcso's zlib pass uses.
        if init(&mut stream, level.clamp(1, 9), Z_DEFLATED, -MAX_WBITS, 9, 0, ZLIB_VERSION.as_ptr() as *const c_char, std::mem::size_of::<ZStream>() as c_int) != Z_OK {
            return None;
        }
        let mut dest = vec![0u8; bound(&mut stream, src.len() as c_ulong) as usize];
        stream.next_in = src.as_ptr();
        stream.avail_in = src.len() as u32;
        stream.next_out = dest.as_mut_ptr();
        stream.avail_out = dest.len() as u32;
        let rc = deflate(&mut stream, Z_FINISH);
        let total_out = stream.total_out as usize;
        end(&mut stream);
        if rc != Z_STREAM_END {
            return None;
        }
        dest.truncate(total_out);
        Some(dest)
    }
}
