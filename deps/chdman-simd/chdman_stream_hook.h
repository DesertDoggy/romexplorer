#pragma once

/*
 * Write-site hook for the streaming build (CHDMAN_WITH_STREAMING).
 *
 * user/patches/streaming/06_extract_stream_hook.patch makes chdman.cpp's do_extract_raw
 * (extractraw/extracthd/extractdvd) and do_extract_cd (extractcd) call these at the exact
 * point each block is written, so the bytes a consumer sees are exactly the bytes chdman
 * writes -- including every cue/gdi pregap/split/byte-swap/subcode rule in do_extract_cd,
 * which is why this is a hook and not a user/-side reimplementation.
 *
 * Implemented in user/chdman_stream.cpp. With no stream active (a plain chdman_run call
 * in the streaming build), emit() returns true and write_enabled() returns true, so the
 * patched chdman behaves exactly like the unpatched one.
 *
 * Found via -I$(U), which is already on every translation unit.
 */

#include <cstddef>
#include <cstdint>
#include <string>
#include <system_error>
#include <utility>

namespace chdman_stream {

// Hands one written block to the active stream consumer. file_name is the output file the
// block belongs to (as chdman opened it), offset is the block's position in that file.
// Returns false if the consumer asked to stop; the caller then aborts via report_error.
bool emit(const std::string &file_name, uint64_t offset, const void *data, uint32_t size);

// False when the active stream is stream-only (no bytes to disk).
bool write_enabled();

// Input side (osd_file::open hook in user/patches/streaming/07_*): if `path` names a source
// registered by the active chdman_create_from_sources call, opens it as a read-only
// virtual file, stores the result in `err` and returns true. Otherwise returns false and
// the real open proceeds. Declared with void* to keep osdfile.h out of this header:
// `file` is an osd_file::ptr*.
bool open_virtual(const std::string &path, std::uint32_t openflags, void *file,
                  std::uint64_t &filesize, std::error_condition &err);

// Stand-in for util::write's result when write_enabled() is false.
inline std::pair<std::error_condition, std::size_t> skipped_write(std::size_t length)
{
	return { std::error_condition(), length };
}

} // namespace chdman_stream
