/*
 * Streaming build only (CHDMAN_WITH_STREAMING, compiled in by Makefile.chdman_lib when
 * STREAMING=1): chdman_extract_stream and the chdman_reader_* random-access API declared
 * in chdman_lib.h, plus the chdman_stream:: hook that
 * user/patches/streaming/06_extract_stream_hook.patch calls from chdman.cpp's write sites.
 *
 * The reader half needs no patch at all -- it uses chd_file / cdrom_file, which are
 * already compiled into the library, directly.
 */

#include "chdman_lib.h"
#include "chdman_stream_hook.h"

#include "cdrom.h"
#include "chd.h"
#include "ioprocs.h"
#include "osdfile.h"

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <map>
#include <mutex>
#include <cstdlib>
#include <cstring>
#include <cerrno>
#include <new>
#include <list>
#include <memory>
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

#if defined(_WIN32)
#include <windows.h>
#endif

// ---------------------------------------------------------------------------
// Extract streaming: the write-site hook
// ---------------------------------------------------------------------------

namespace {

// State of the one active chdman_extract_stream call. A plain global is enough: like
// chdman_run, chdman_extract_stream must not run concurrently with itself, and chdman's
// extract loops write on the calling thread.
struct stream_state
{
	ChdmanDataCb on_data = nullptr;
	void *user_data = nullptr;
	bool write_files = true;
	bool stopped = false;
	std::vector<std::string> files; // output files in first-write order = file_index
};

stream_state *g_stream = nullptr;

uint32_t file_index_for(stream_state &state, const std::string &name)
{
	for (size_t i = 0; i < state.files.size(); i++)
		if (state.files[i] == name)
			return uint32_t(i);
	state.files.push_back(name);
	return uint32_t(state.files.size() - 1);
}

bool deliver(stream_state &state, const std::string &name, uint64_t offset, const void *data, uint32_t size)
{
	const uint32_t index = file_index_for(state, name);
	if (!state.on_data || state.on_data(index, name.c_str(), offset, data, size, state.user_data))
		return true;
	state.stopped = true;
	return false;
}

// The value following -o / --output in a chdman argv, or empty.
std::string output_arg(int argc, const char *const *argv)
{
	for (int i = 0; i + 1 < argc; i++)
		if (argv[i] && (!std::strcmp(argv[i], "-o") || !std::strcmp(argv[i], "--output")))
			return argv[i + 1] ? argv[i + 1] : "";
	return {};
}

FILE *open_file(const char *path, const char *mode)
{
#if defined(_WIN32)
	auto widen = [](const char *s) {
		const int n = MultiByteToWideChar(CP_UTF8, 0, s, -1, nullptr, 0);
		std::wstring w(n > 0 ? n - 1 : 0, L'\0');
		if (n > 0)
			MultiByteToWideChar(CP_UTF8, 0, s, -1, w.data(), n);
		return w;
	};
	return _wfopen(widen(path).c_str(), widen(mode).c_str());
#else
	return std::fopen(path, mode);
#endif
}

bool read_whole_file(const std::string &path, std::vector<uint8_t> &out)
{
	FILE *f = open_file(path.c_str(), "rb");
	if (!f)
		return false;
	uint8_t chunk[65536];
	size_t n;
	while ((n = std::fread(chunk, 1, sizeof chunk, f)) > 0)
		out.insert(out.end(), chunk, chunk + n);
	const bool ok = !std::ferror(f);
	std::fclose(f);
	return ok;
}

void remove_file(const std::string &path)
{
#if defined(_WIN32)
	const int n = MultiByteToWideChar(CP_UTF8, 0, path.c_str(), -1, nullptr, 0);
	std::wstring w(n > 0 ? n - 1 : 0, L'\0');
	if (n > 0)
		MultiByteToWideChar(CP_UTF8, 0, path.c_str(), -1, w.data(), n);
	_wremove(w.c_str());
#else
	std::remove(path.c_str());
#endif
}

} // namespace

namespace chdman_stream {

bool emit(const std::string &file_name, uint64_t offset, const void *data, uint32_t size)
{
	return !g_stream || deliver(*g_stream, file_name, offset, data, size);
}

bool write_enabled()
{
	return !g_stream || g_stream->write_files;
}

} // namespace chdman_stream

int chdman_extract_stream(int argc, const char *const *argv, int write_files, ChdmanDataCb on_data,
                          char **out_log, ChdmanProgressCb on_progress, void *user_data)
{
	if (argc < 1 || !argv || !argv[0])
		return 1;
	const std::string command = argv[0];
	if (command != "extractraw" && command != "extracthd" && command != "extractdvd" && command != "extractcd")
	{
		if (out_log)
		{
			static const char msg[] = "chdman_extract_stream: unsupported command (extractraw/extracthd/extractdvd/extractcd only)\n";
			*out_log = static_cast<char *>(std::malloc(sizeof msg));
			if (*out_log)
				std::memcpy(*out_log, msg, sizeof msg);
		}
		return 1;
	}

	stream_state state;
	state.on_data = on_data;
	state.user_data = user_data;
	state.write_files = write_files != 0;

	g_stream = &state;
	int rc = chdman_run(argc, argv, out_log, on_progress, user_data);
	g_stream = nullptr;

	// extractcd: the cue/gdi/toc sheet is written with printf, not through the hooked
	// write sites -- deliver the finished sheet as one more file.
	const std::string sheet = output_arg(argc, argv);
	if (rc == 0 && command == "extractcd" && !sheet.empty())
	{
		std::vector<uint8_t> text;
		if (!read_whole_file(sheet, text))
			rc = 1;
		else
			deliver(state, sheet, 0, text.data(), uint32_t(text.size()));
	}

	// Stream only: chdman still created its (empty) output files -- remove them all.
	// (On failure chdman's own catch blocks have already removed them.)
	if (!state.write_files)
	{
		for (const std::string &name : state.files)
			remove_file(name);
		if (command == "extractcd" && !sheet.empty())
			remove_file(sheet);
	}

	return state.stopped ? -6 : rc;
}

// ---------------------------------------------------------------------------
// Random access reader
// ---------------------------------------------------------------------------

namespace {

thread_local std::string t_reader_error;

int fail(std::string message)
{
	t_reader_error = std::move(message);
	return -1;
}

int fail(std::string const &what, std::error_condition const &err)
{
	return fail(what + ": " + err.message());
}

// Read-only stdio file with 64-bit offsets. MAME's util::stdio_read_write seeks with
// `long`, which is 32 bits on Windows, so it cannot open a CHD over 2 GiB there (the
// header's length check fails with EINVAL).
class stdio64_reader : public util::random_read_write
{
public:
	explicit stdio64_reader(FILE *f) noexcept : m_file(f) {}
	~stdio64_reader() override { std::fclose(m_file); }

	std::error_condition seek(std::int64_t offset, int whence) noexcept override
	{
		return do_seek(offset, whence) ? std::error_condition(errno, std::generic_category()) : std::error_condition();
	}
	std::error_condition tell(std::uint64_t &result) noexcept override
	{
		const std::int64_t pos = do_tell();
		if (pos < 0)
			return std::error_condition(errno, std::generic_category());
		result = std::uint64_t(pos);
		return {};
	}
	std::error_condition length(std::uint64_t &result) noexcept override
	{
		const std::int64_t pos = do_tell();
		if (pos < 0 || do_seek(0, SEEK_END))
			return std::error_condition(errno, std::generic_category());
		const std::int64_t end = do_tell();
		do_seek(pos, SEEK_SET);
		if (end < 0)
			return std::error_condition(errno, std::generic_category());
		result = std::uint64_t(end);
		return {};
	}
	std::error_condition read_some(void *buffer, std::size_t len, std::size_t &actual) noexcept override
	{
		actual = std::fread(buffer, 1, len, m_file);
		return (actual < len && std::ferror(m_file)) ? std::error_condition(std::errc::io_error) : std::error_condition();
	}
	std::error_condition read_some_at(std::uint64_t offset, void *buffer, std::size_t len, std::size_t &actual) noexcept override
	{
		actual = 0;
		if (do_seek(std::int64_t(offset), SEEK_SET))
			return std::error_condition(errno, std::generic_category());
		return read_some(buffer, len, actual);
	}
	std::error_condition finalize() noexcept override { return {}; }
	std::error_condition flush() noexcept override { return std::fflush(m_file) ? std::error_condition(errno, std::generic_category()) : std::error_condition(); }
	std::error_condition write_some(void const *, std::size_t, std::size_t &actual) noexcept override
	{
		actual = 0;
		return std::errc::bad_file_descriptor;
	}
	std::error_condition write_some_at(std::uint64_t, void const *, std::size_t, std::size_t &actual) noexcept override
	{
		actual = 0;
		return std::errc::bad_file_descriptor;
	}

private:
	int do_seek(std::int64_t offset, int whence) noexcept
	{
#if defined(_WIN32)
		return _fseeki64(m_file, offset, whence);
#else
		return fseeko(m_file, off_t(offset), whence);
#endif
	}
	std::int64_t do_tell() noexcept
	{
#if defined(_WIN32)
		return _ftelli64(m_file);
#else
		return std::int64_t(ftello(m_file));
#endif
	}

	FILE *m_file;
};

// Opens a CHD file through stdio (see chdman_lib.h: avoids the OSD layer's Windows
// sequential-scan hint, which penalizes random reads).
std::error_condition open_chd(chd_file &chd, const char *path, chd_file *parent)
{
	FILE *f = open_file(path, "rb");
	if (!f)
		return std::errc::no_such_file_or_directory;
	util::random_read_write::ptr io(new (std::nothrow) stdio64_reader(f)); // takes ownership of f
	if (!io)
	{
		std::fclose(f);
		return std::errc::not_enough_memory;
	}
	return chd.open(std::move(io), false, parent);
}

} // namespace

struct ChdmanReader
{
	chd_file parent;                   // declared first: must outlive chd
	chd_file chd;
	std::unique_ptr<cdrom_file> cdrom; // non-null for CD/GD-ROM
	uint32_t cache_capacity = 0;

	// LRU of decompressed hunks: front = most recently used.
	std::list<std::pair<uint32_t, std::vector<uint8_t>>> lru;
	std::unordered_map<uint32_t, decltype(lru)::iterator> lru_index;

	// Returns the cached hunk, decompressing it on a miss; nullptr + error on failure.
	const uint8_t *cached_hunk(uint32_t hunk, std::error_condition &err)
	{
		auto found = lru_index.find(hunk);
		if (found != lru_index.end())
		{
			lru.splice(lru.begin(), lru, found->second);
			return lru.front().second.data();
		}
		std::vector<uint8_t> data;
		if (lru.size() >= cache_capacity)
		{
			data = std::move(lru.back().second); // reuse the evicted buffer
			lru_index.erase(lru.back().first);
			lru.pop_back();
		}
		data.resize(chd.hunk_bytes());
		err = chd.read_hunk(hunk, data.data());
		if (err)
			return nullptr;
		lru.emplace_front(hunk, std::move(data));
		lru_index[hunk] = lru.begin();
		return lru.front().second.data();
	}
};

ChdmanReader *chdman_reader_open(const char *path, const char *parent_path, uint32_t cache_hunks)
{
	if (!path)
	{
		fail("path is NULL");
		return nullptr;
	}
	try
	{
		auto reader = std::make_unique<ChdmanReader>();
		reader->cache_capacity = cache_hunks;

		chd_file *parent = nullptr;
		if (parent_path)
		{
			std::error_condition err = open_chd(reader->parent, parent_path, nullptr);
			if (err)
			{
				fail(std::string("opening parent ") + parent_path, err);
				return nullptr;
			}
			parent = &reader->parent;
		}

		std::error_condition err = open_chd(reader->chd, path, parent);
		if (err)
		{
			fail(std::string("opening ") + path, err);
			return nullptr;
		}

		// cdrom_file's constructor throws (nullptr) when the CHD has no CD/GD-ROM track
		// metadata -- i.e. it's a DVD/HD/raw CHD.
		try
		{
			reader->cdrom = std::make_unique<cdrom_file>(&reader->chd);
		}
		catch (...)
		{
			reader->cdrom.reset();
		}
		return reader.release();
	}
	catch (std::exception const &e)
	{
		fail(e.what());
	}
	catch (...)
	{
		fail("unexpected exception opening CHD");
	}
	return nullptr;
}

void chdman_reader_close(ChdmanReader *reader)
{
	delete reader;
}

int chdman_reader_get_info(ChdmanReader *reader, ChdmanReaderInfo *out)
{
	if (!reader || !out)
		return fail("NULL argument");
	std::memset(out, 0, sizeof *out);
	out->logical_bytes = reader->chd.logical_bytes();
	out->hunk_bytes = reader->chd.hunk_bytes();
	out->hunk_count = reader->chd.hunk_count();
	out->unit_bytes = reader->chd.unit_bytes();
	out->version = reader->chd.version();
	if (reader->cdrom)
	{
		out->is_cd = 1;
		out->is_gdrom = reader->cdrom->is_gdrom() ? 1 : 0;
		out->num_tracks = reader->cdrom->get_toc().numtrks;
	}
	return 0;
}

int chdman_reader_read_hunk(ChdmanReader *reader, uint32_t hunk, void *buffer)
{
	if (!reader || !buffer)
		return fail("NULL argument");
	std::error_condition err = reader->chd.read_hunk(hunk, buffer);
	return err ? fail("read_hunk", err) : 0;
}

int chdman_reader_read_bytes(ChdmanReader *reader, uint64_t offset, void *buffer, uint32_t size)
{
	if (!reader || (!buffer && size))
		return fail("NULL argument");
	if (size == 0)
		return 0;
	if (offset > reader->chd.logical_bytes() || size > reader->chd.logical_bytes() - offset)
		return fail("read past end of CHD");

	if (reader->cache_capacity == 0)
	{
		std::error_condition err = reader->chd.read_bytes(offset, buffer, size);
		return err ? fail("read_bytes", err) : 0;
	}

	// Same walk as chd_file::read_bytes, but partial hunks come from our LRU instead of
	// chd_file's single-hunk cache; whole hunks still decompress straight into buffer.
	const uint32_t hunk_bytes = reader->chd.hunk_bytes();
	auto *dest = static_cast<uint8_t *>(buffer);
	uint64_t pos = offset;
	uint32_t remaining = size;
	while (remaining)
	{
		const uint32_t hunk = uint32_t(pos / hunk_bytes);
		const uint32_t start = uint32_t(pos % hunk_bytes);
		const uint32_t len = std::min<uint32_t>(hunk_bytes - start, remaining);
		std::error_condition err;
		if (start == 0 && len == hunk_bytes && !reader->lru_index.count(hunk))
		{
			err = reader->chd.read_hunk(hunk, dest);
			if (err)
				return fail("read_hunk", err);
		}
		else
		{
			const uint8_t *src = reader->cached_hunk(hunk, err);
			if (!src)
				return fail("read_hunk", err);
			std::memcpy(dest, src + start, len);
		}
		dest += len;
		pos += len;
		remaining -= len;
	}
	return 0;
}

int chdman_reader_get_track(ChdmanReader *reader, uint32_t track, ChdmanTrackInfo *out)
{
	if (!reader || !out)
		return fail("NULL argument");
	if (!reader->cdrom)
		return fail("not a CD/GD-ROM CHD");
	const cdrom_file::toc &toc = reader->cdrom->get_toc();
	if (track >= toc.numtrks)
		return fail("track out of range");
	const cdrom_file::track_info &t = toc.tracks[track];
	out->track_type = t.trktype;
	out->sub_type = t.subtype;
	out->data_size = t.datasize;
	out->sub_size = t.subsize;
	out->frames = t.frames;
	out->pregap = t.pregap;
	out->postgap = t.postgap;
	out->session = t.session;
	out->logical_start = t.logframeofs;
	out->physical_start = t.physframeofs;
	out->chd_frame_start = t.chdframeofs;
	return 0;
}

int chdman_reader_read_sector(ChdmanReader *reader, uint32_t lba, void *buffer, uint32_t datatype, int phys)
{
	if (!reader || !buffer)
		return fail("NULL argument");
	if (!reader->cdrom)
		return fail("not a CD/GD-ROM CHD");
	if (!reader->cdrom->read_data(lba, buffer, datatype, phys != 0))
		return fail("read_data failed (bad LBA, or datatype incompatible with the track)");
	return 0;
}

const char *chdman_reader_last_error(void)
{
	return t_reader_error.c_str();
}

// ---------------------------------------------------------------------------
// Creation from caller sources: virtual files behind osd_file::open
// ---------------------------------------------------------------------------

namespace {

namespace fs = std::filesystem;

std::string normalize_path(const std::string &path)
{
	return fs::u8path(path).lexically_normal().generic_u8string();
}

// Sources of the active chdman_create_from_sources call, by normalized virtual path.
// Like g_stream: one call at a time (chdman_run's own rule).
std::map<std::string, const ChdmanSource *> *g_virtual = nullptr;

// Read-only osd_file over a ChdmanSource. read_at sources are read directly. Sequential
// sources are read forward into a history window, so chdman's reads (in order, with
// occasional small steps back from its buffering) are served; a read before the window
// fails.
class virtual_osd_file : public osd_file
{
public:
	explicit virtual_osd_file(const ChdmanSource &source) : m_source(source) {}

	std::error_condition read(void *buffer, std::uint64_t offset, std::uint32_t length, std::uint32_t &actual) noexcept override
	{
		std::lock_guard<std::mutex> lock(m_mutex);
		actual = 0;
		if (offset >= m_source.size || length == 0)
			return std::error_condition();
		const std::uint32_t len = std::uint32_t(std::min<std::uint64_t>(length, m_source.size - offset));
		auto *out = static_cast<std::uint8_t *>(buffer);

		if (m_source.read_at)
		{
			if (m_source.read_at(m_source.user_data, offset, out, len) != 0)
				return std::errc::io_error;
			actual = len;
			return std::error_condition();
		}

		if (offset < m_history_start)
			return std::errc::invalid_seek;
		const std::uint64_t end = offset + len;
		try
		{
			while (m_pos < end)
			{
				const std::uint64_t want = std::min<std::uint64_t>(1u << 20, m_source.size - m_pos);
				const std::size_t old = m_history.size();
				m_history.resize(old + want);
				const std::int64_t n = m_source.read(m_source.user_data, m_history.data() + old, want);
				if (n <= 0 || std::uint64_t(n) > want)
				{
					m_history.resize(old);
					return std::errc::io_error;
				}
				m_history.resize(old + std::size_t(n));
				m_pos += std::uint64_t(n);
				trim(offset);
			}
		}
		catch (...)
		{
			return std::errc::not_enough_memory;
		}
		std::memcpy(out, m_history.data() + (offset - m_history_start), len);
		actual = len;
		trim(offset);
		return std::error_condition();
	}

	std::error_condition write(void const *, std::uint64_t, std::uint32_t, std::uint32_t &actual) noexcept override
	{
		actual = 0;
		return std::errc::read_only_file_system;
	}
	std::error_condition truncate(std::uint64_t) noexcept override { return std::errc::read_only_file_system; }
	std::error_condition flush() noexcept override { return std::error_condition(); }

private:
	static constexpr std::uint64_t WINDOW = 32u << 20;

	// Drops history older than WINDOW behind the current position, but never bytes at or
	// after `keep_from` (the read in progress).
	void trim(std::uint64_t keep_from)
	{
		const std::uint64_t floor = std::min(keep_from, m_pos > WINDOW ? m_pos - WINDOW : 0);
		if (floor > m_history_start && floor - m_history_start >= WINDOW)
		{
			const std::size_t drop = std::size_t(floor - m_history_start);
			m_history.erase(m_history.begin(), m_history.begin() + drop);
			m_history_start = floor;
		}
	}

	const ChdmanSource &m_source;
	std::mutex m_mutex;
	std::vector<std::uint8_t> m_history;  // bytes [m_history_start, m_pos)
	std::uint64_t m_history_start = 0;
	std::uint64_t m_pos = 0;
};

bool read_source_fully(const ChdmanSource &source, std::vector<std::uint8_t> &out)
{
	out.resize(std::size_t(source.size));
	if (source.read_at)
		return source.size == 0 || source.read_at(source.user_data, 0, out.data(), source.size) == 0;
	std::uint64_t got = 0;
	while (got < source.size)
	{
		const std::int64_t n = source.read(source.user_data, out.data() + got, source.size - got);
		if (n <= 0)
			return false;
		got += std::uint64_t(n);
	}
	return true;
}

void set_log(char **out_log, const std::string &text)
{
	if (!out_log)
		return;
	*out_log = static_cast<char *>(std::malloc(text.size() + 1));
	if (*out_log)
		std::memcpy(*out_log, text.c_str(), text.size() + 1);
}

} // namespace

bool chdman_stream::open_virtual(const std::string &path, std::uint32_t openflags, void *file,
                                 std::uint64_t &filesize, std::error_condition &err)
{
	if (!g_virtual)
		return false;
	const auto found = g_virtual->find(normalize_path(path));
	if (found == g_virtual->end())
		return false;
	if (openflags & (OPEN_FLAG_WRITE | OPEN_FLAG_CREATE))
	{
		err = std::errc::read_only_file_system;
		return true;
	}
	try
	{
		*static_cast<osd_file::ptr *>(file) = std::make_unique<virtual_osd_file>(*found->second);
	}
	catch (...)
	{
		err = std::errc::not_enough_memory;
		return true;
	}
	filesize = found->second->size;
	err = std::error_condition();
	return true;
}

int chdman_create_from_sources(int argc, const char *const *argv, const ChdmanSource *sources, int count,
                               char **out_log, ChdmanProgressCb on_progress, void *user_data)
{
	if (argc < 1 || !argv || !argv[0] || !sources || count <= 0)
	{
		set_log(out_log, "chdman_create_from_sources: invalid arguments\n");
		return 1;
	}
	const std::string command = argv[0];
	if (command != "createraw" && command != "createhd" && command != "createdvd" && command != "createcd")
	{
		set_log(out_log, "chdman_create_from_sources: unsupported command (createraw/createhd/createdvd/createcd only)\n");
		return 1;
	}
	for (int i = 0; i < count; i++)
	{
		if (!sources[i].name || !*sources[i].name || (!sources[i].read && !sources[i].read_at))
		{
			set_log(out_log, "chdman_create_from_sources: every source needs a name and read or read_at\n");
			return 1;
		}
	}

	// The -i value must name a source; it is rewritten to that source's virtual path.
	std::vector<std::string> args(argv, argv + argc);
	const ChdmanSource *input = nullptr;
	for (std::size_t i = 0; i + 1 < args.size(); i++)
	{
		if (args[i] != "-i" && args[i] != "--input")
			continue;
		for (int k = 0; k < count && !input; k++)
			if (args[i + 1] == sources[k].name)
				input = &sources[k];
		if (!input)
		{
			set_log(out_log, "chdman_create_from_sources: -i does not name one of the sources\n");
			return 1;
		}
		break;
	}
	if (!input)
	{
		set_log(out_log, "chdman_create_from_sources: -i is required\n");
		return 1;
	}

	// Private directory: holds the (materialized) sheet, and anchors the virtual paths so a
	// sheet's relative track references resolve to them.
	std::error_code ec;
	const auto stamp = std::chrono::steady_clock::now().time_since_epoch().count();
	const fs::path dir = fs::temp_directory_path(ec) / ("chdman_sources_" + std::to_string(stamp));
	if (ec || !fs::create_directories(dir, ec))
	{
		set_log(out_log, "chdman_create_from_sources: cannot create a temporary directory\n");
		return 1;
	}

	std::map<std::string, const ChdmanSource *> registry;
	int rc = 1;
	try
	{
		for (int k = 0; k < count; k++)
		{
			const fs::path path = dir / fs::u8path(sources[k].name);
			const bool is_sheet = command == "createcd" && &sources[k] == input;
			if (is_sheet)
			{
				std::vector<std::uint8_t> text;
				if (!read_source_fully(sources[k], text))
				{
					set_log(out_log, "chdman_create_from_sources: reading the sheet source failed\n");
					fs::remove_all(dir, ec);
					return 1;
				}
				fs::create_directories(path.parent_path(), ec);
				std::ofstream(path, std::ios::binary).write(reinterpret_cast<const char *>(text.data()), std::streamsize(text.size()));
			}
			else
			{
				registry[normalize_path(path.u8string())] = &sources[k];
			}
			if (&sources[k] == input)
			{
				for (std::size_t i = 0; i + 1 < args.size(); i++)
					if (args[i] == "-i" || args[i] == "--input")
						args[i + 1] = path.u8string();
			}
		}

		std::vector<const char *> argv2;
		for (const std::string &a : args)
			argv2.push_back(a.c_str());

		g_virtual = &registry;
		rc = chdman_run(int(argv2.size()), argv2.data(), out_log, on_progress, user_data);
		g_virtual = nullptr;
	}
	catch (...)
	{
		g_virtual = nullptr;
		set_log(out_log, "chdman_create_from_sources: unexpected exception\n");
		rc = 1;
	}
	fs::remove_all(dir, ec);
	return rc;
}
