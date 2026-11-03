// cemuwua: see cemuwua.h. Everything that reads the Wii U formats is Cemu's own code, compiled
// from external/cemu; this file only walks a title's files and feeds them to ZArchive, the
// way Cemu's wxTitleManagerList::OnConvertToCompressedFormat does.

#include "cemuwua.h"

#include "Common/FileStream.h"
#include "Cafe/Filesystem/FST/FST.h"
#include "Cafe/Filesystem/FST/KeyCache.h"
#include "util/crypto/aes128.h"
#include "openssl/openssl_min.h"

#include "zarchive/zarchivewriter.h"
#include "zarchive/zarchivereader.h"

#include <algorithm>
#include <cstdio>
#include <exception>
#include <mutex>

namespace {

// --- errors ------------------------------------------------------------------------------

struct Failure
{
	int status;
	std::string message;
};

void write_error(char* error, size_t error_len, const std::string& message)
{
	if (!error || error_len == 0)
		return;
	size_t n = std::min(error_len - 1, message.size());
	std::memcpy(error, message.data(), n);
	error[n] = '\0';
}

fs::path utf8_path(const char* s)
{
	return _utf8ToPath(s);
}

std::once_flag g_aesInit;
void init_once()
{
	std::call_once(g_aesInit, [] { AES128_init(); });
}

// --- keys.txt ----------------------------------------------------------------------------
// Cemu keeps one global key cache filled from its keys.txt; FST.cpp asks it for candidates
// (KeyCache_Prepare / KeyCache_GetAES128) when no key is passed. Here the cache is whatever
// keys.txt the caller names, parsed by Cemu's rules: one hex key per line, anything after
// '#' or ';' is a comment, spaces, tabs, '-' and '_' are ignored.

std::mutex g_keyMutex;
std::vector<std::array<uint8, 16>> g_keys;

bool parse_hex_key(std::string line, std::array<uint8, 16>& out)
{
	for (size_t i = 0; i < line.size(); i++)
	{
		if (line[i] == '#' || line[i] == ';')
		{
			line.resize(i);
			break;
		}
	}
	std::string hex;
	for (char c : line)
	{
		if (c == ' ' || c == '\t' || c == '-' || c == '_' || c == '\r' || c == '\n')
			continue;
		hex.push_back(c);
	}
	if (hex.size() != 32)
		return false;
	for (size_t i = 0; i < 16; i++)
	{
		auto nibble = [](char c) -> int {
			if (c >= '0' && c <= '9') return c - '0';
			if (c >= 'a' && c <= 'f') return c - 'a' + 10;
			if (c >= 'A' && c <= 'F') return c - 'A' + 10;
			return -1;
		};
		int hi = nibble(hex[i * 2]), lo = nibble(hex[i * 2 + 1]);
		if (hi < 0 || lo < 0)
			return false;
		out[i] = (uint8)(hi << 4 | lo);
	}
	return true;
}

std::vector<std::array<uint8, 16>> load_keys_txt(const char* keys_txt)
{
	std::vector<std::array<uint8, 16>> keys;
	if (!keys_txt || !*keys_txt)
		return keys;
	std::unique_ptr<FileStream> fs(FileStream::openFile2(utf8_path(keys_txt)));
	if (!fs)
		return keys;
	std::string line;
	while (fs->readLine(line))
	{
		std::array<uint8, 16> key;
		if (parse_hex_key(line, key))
			keys.push_back(key);
	}
	return keys;
}

// --- a title's files ---------------------------------------------------------------------

class TitleSource
{
public:
	virtual ~TitleSource() = default;
	struct Entry
	{
		std::string path; // relative, '/'-separated
		bool isDir;
		uint64 size;
	};
	// Every directory and file, parents before children.
	virtual bool List(std::vector<Entry>& out) = 0;
	// Streams one file's bytes to `sink`; false on a read error.
	virtual bool Read(const Entry& entry, const std::function<bool(const uint8*, size_t)>& sink) = 0;
	// The whole of a small file, or nothing.
	virtual std::optional<std::vector<uint8>> ReadWhole(std::string_view path) = 0;
};

constexpr size_t TRANSFER = 1024 * 1024;

class FstSource : public TitleSource
{
public:
	explicit FstSource(std::unique_ptr<FSTVolume> volume) : m_volume(std::move(volume)) {}

	bool List(std::vector<Entry>& out) override { return Walk("", out); }

	bool Read(const Entry& entry, const std::function<bool(const uint8*, size_t)>& sink) override
	{
		FSTFileHandle h;
		if (!m_volume->OpenFile(entry.path, h, true))
			return false;
		std::vector<uint8> buf(TRANSFER);
		uint64 size = m_volume->GetFileSize(h);
		uint64 offset = 0;
		while (offset < size)
		{
			uint32 want = (uint32)std::min<uint64>(buf.size(), size - offset);
			uint32 got = m_volume->ReadFile(h, (uint32)offset, want, buf.data());
			if (got != want || m_volume->HasCorruption())
				return false;
			if (!sink(buf.data(), got))
				return false;
			offset += got;
		}
		return true;
	}

	std::optional<std::vector<uint8>> ReadWhole(std::string_view path) override
	{
		bool ok = false;
		auto data = m_volume->ExtractFile(path, &ok);
		if (!ok)
			return std::nullopt;
		return data;
	}

private:
	bool Walk(const std::string& dir, std::vector<Entry>& out)
	{
		FSTDirectoryIterator it;
		if (!m_volume->OpenDirectoryIterator(dir, it))
			return false;
		FSTFileHandle h;
		while (m_volume->Next(it, h))
		{
			// Link entries point elsewhere on the disc; Cemu's own file system refuses to open
			// them (fscDeviceWud), so they never reach a .wua it writes either.
			if (m_volume->HasLinkFlag(h))
				continue;
			std::string path = dir.empty() ? std::string(m_volume->GetName(h)) : dir + "/" + std::string(m_volume->GetName(h));
			if (m_volume->IsDirectory(h))
			{
				out.push_back({path, true, 0});
				if (!Walk(path, out))
					return false;
			}
			else
				out.push_back({path, false, m_volume->GetFileSize(h)});
		}
		return true;
	}

	std::unique_ptr<FSTVolume> m_volume;
};

class FolderSource : public TitleSource
{
public:
	explicit FolderSource(fs::path root) : m_root(std::move(root)) {}

	bool List(std::vector<Entry>& out) override
	{
		// Sorted per directory (in Walk) so the archive's layout does not depend on the
		// host's directory order.
		return Walk(m_root, "", out);
	}

	bool Read(const Entry& entry, const std::function<bool(const uint8*, size_t)>& sink) override
	{
		std::unique_ptr<FileStream> fs(FileStream::openFile2(m_root / _utf8ToPath(entry.path)));
		if (!fs)
			return false;
		std::vector<uint8> buf(TRANSFER);
		uint64 left = entry.size;
		while (left > 0)
		{
			uint32 want = (uint32)std::min<uint64>(buf.size(), left);
			uint32 got = fs->readData(buf.data(), want);
			if (got != want)
				return false;
			if (!sink(buf.data(), got))
				return false;
			left -= got;
		}
		return true;
	}

	std::optional<std::vector<uint8>> ReadWhole(std::string_view path) override
	{
		return FileStream::LoadIntoMemory(m_root / _utf8ToPath(path));
	}

private:
	bool Walk(const fs::path& dir, const std::string& rel, std::vector<Entry>& out)
	{
		std::error_code ec;
		std::vector<fs::directory_entry> entries;
		for (auto& e : fs::directory_iterator(dir, ec))
			entries.push_back(e);
		if (ec)
			return false;
		std::sort(entries.begin(), entries.end(), [](auto& a, auto& b) { return a.path().filename() < b.path().filename(); });
		for (auto& e : entries)
		{
			std::string name = _pathToUtf8(e.path().filename());
			std::string path = rel.empty() ? name : rel + "/" + name;
			if (e.is_directory(ec))
			{
				out.push_back({path, true, 0});
				if (!Walk(e.path(), path, out))
					return false;
			}
			else if (e.is_regular_file(ec))
				out.push_back({path, false, (uint64)e.file_size(ec)});
		}
		return true;
	}

	fs::path m_root;
};

bool is_disc_image(const fs::path& p)
{
	std::string ext = _pathToUtf8(p.extension());
	std::transform(ext.begin(), ext.end(), ext.begin(), [](unsigned char c) { return (char)std::tolower(c); });
	return ext == ".wud" || ext == ".wux";
}

// Opens one source. keys: candidates from keys.txt, tried after an explicit key and before a
// "<image>.key" beside the image (FSTVolume::FindDiscKey does the last two).
std::unique_ptr<TitleSource> open_source(const char* path_utf8, const uint8_t* key, const std::vector<std::array<uint8, 16>>& keys, Failure& fail)
{
	fs::path path = utf8_path(path_utf8);
	std::error_code ec;
	if (fs::is_directory(path, ec))
	{
		if (fs::exists(path / "title.tmd", ec))
		{
			FSTVolume::ErrorCode code = FSTVolume::ErrorCode::OK;
			std::unique_ptr<FSTVolume> volume(FSTVolume::OpenFromContentFolder(path, &code));
			if (!volume)
			{
				fail = {CEMUWUA_ERR_OPEN, code == FSTVolume::ErrorCode::TITLE_TIK_MISSING ? "the title folder has no title.tik" : "the title folder could not be read (title.tmd / title.tik / .app files)"};
				return nullptr;
			}
			return std::make_unique<FstSource>(std::move(volume));
		}
		if (fs::exists(path / "code" / "app.xml", ec))
			return std::make_unique<FolderSource>(path);
		fail = {CEMUWUA_ERR_OPEN, "the folder is neither a title folder (title.tmd) nor an extracted title (code/app.xml)"};
		return nullptr;
	}
	if (!is_disc_image(path))
	{
		fail = {CEMUWUA_ERR_OPEN, "not a .wud/.wux disc image or a title folder"};
		return nullptr;
	}
	std::unique_ptr<FSTVolume> volume;
	FSTVolume::ErrorCode code = FSTVolume::ErrorCode::OK;
	if (key)
	{
		NCrypto::AesKey k;
		std::memcpy(k.b, key, 16);
		volume.reset(FSTVolume::OpenFromDiscImage(path, k, &code));
	}
	else
	{
		{
			std::lock_guard lock(g_keyMutex);
			g_keys = keys;
			volume.reset(FSTVolume::OpenFromDiscImage(path, &code));
		}
	}
	if (!volume)
	{
		if (code == FSTVolume::ErrorCode::DISC_KEY_MISSING || key)
			fail = {CEMUWUA_ERR_KEY, key ? "the disc image could not be decrypted with this key" : "no key for this disc image (none given, none in keys.txt fits, no .key file beside it)"};
		else
			fail = {CEMUWUA_ERR_OPEN, "the disc image could not be opened or is damaged"};
		return nullptr;
	}
	return std::make_unique<FstSource>(std::move(volume));
}

bool parse_app_xml(const std::vector<uint8>& xml, uint64& titleId, uint32& version)
{
	// code/app.xml is a flat <app> element of typed values. Cemu parses it with pugixml and
	// reads both fields as hexadecimal (TitleInfo::ParseAppXml); this does the same without
	// an XML library, which these two leaf elements do not need.
	std::string_view s((const char*)xml.data(), xml.size());
	auto field = [&](std::string_view name, uint64& out) -> bool {
		std::string open = "<" + std::string(name);
		size_t p = s.find(open);
		if (p == std::string_view::npos)
			return false;
		p = s.find('>', p);
		if (p == std::string_view::npos)
			return false;
		size_t e = s.find('<', p);
		if (e == std::string_view::npos)
			return false;
		std::string text(s.substr(p + 1, e - p - 1));
		text.erase(std::remove_if(text.begin(), text.end(), [](unsigned char c) { return std::isspace(c); }), text.end());
		if (text.empty())
			return false;
		char* end = nullptr;
		out = std::strtoull(text.c_str(), &end, 16);
		return end && *end == '\0';
	};
	uint64 v = 0;
	if (!field("title_id", titleId) || !field("title_version", v))
		return false;
	version = (uint16)v; // a uint16 in Cemu
	return true;
}

bool title_of(TitleSource& src, uint64& titleId, uint32& version)
{
	auto xml = src.ReadWhole("code/app.xml");
	return xml && parse_app_xml(*xml, titleId, version);
}

// --- the manifest ------------------------------------------------------------------------

std::string json_string(std::string_view v)
{
	std::string out = "\"";
	for (unsigned char c : v)
	{
		switch (c)
		{
		case '"': out += "\\\""; break;
		case '\\': out += "\\\\"; break;
		case '\n': out += "\\n"; break;
		case '\r': out += "\\r"; break;
		case '\t': out += "\\t"; break;
		default:
			if (c < 0x20)
				out += fmt::format("\\u{:04x}", c);
			else
				out += (char)c;
		}
	}
	return out + "\"";
}

std::string hex(const uint8* d, size_t n)
{
	std::string out;
	for (size_t i = 0; i < n; i++)
		out += fmt::format("{:02x}", d[i]);
	return out;
}

struct HashedFile
{
	std::string path;
	uint64 size;
	std::string sha1;
};

// --- writing -----------------------------------------------------------------------------

struct Output
{
	std::unique_ptr<FileStream> fs;
	fs::path path;
	bool failed = false;
	bool opened = false;

	static void NewOutputFile(const int32_t, void* ctx)
	{
		auto* o = (Output*)ctx;
		o->fs.reset(FileStream::createFile2(o->path));
		o->opened = true;
		if (!o->fs)
			o->failed = true;
	}

	static void WriteOutputData(const void* data, size_t length, void* ctx)
	{
		auto* o = (Output*)ctx;
		if (!o->fs || o->failed)
			return;
		if (o->fs->writeData(data, (uint32)length) != (uint32)length)
			o->failed = true;
	}
};

} // namespace

// Cemu's key cache, backed by the keys.txt of the current call (see above). The mutex is held
// by open_source for the whole OpenFromDiscImage call that reads it.
void KeyCache_Prepare() {}

uint8* KeyCache_GetAES128(sint32 index)
{
	if (index < 0 || index >= (sint32)g_keys.size())
		return nullptr;
	return g_keys[index].data();
}

// ncrypto.cpp's console-certificate helpers ask whether Cemu's IOSU found a console dump
// (otp.bin / seeprom.bin). cemuwua never has one.
bool SEEPROM_IsPresent() { return false; }
bool OTP_IsPresent() { return false; }

// The same for the console-identity calls ncrypto.cpp links against (iosu_crypto.cpp in
// Cemu). They are only reached for personalized tickets and device certificates -- titles
// bought on one console -- which a disc never has; with no console, they say so.
bool hasOtpMem = false;
bool hasSeepromMem = false;
bool iosuCrypto_getDeviceId(uint32* deviceId)
{
	if (deviceId)
		*deviceId = 0;
	return false;
}
void iosuCrypto_getDeviceSerialString(char* serialString)
{
	if (serialString)
		serialString[0] = '\0';
}
void iosuCrypto_getDeviceCertificate(void* certOut, sint32 len)
{
	if (certOut && len > 0)
		std::memset(certOut, 0, (size_t)len);
}
void iosuCrypto_getDeviceCertPrivateKey(void* privKeyOut, sint32 len)
{
	if (privKeyOut && len > 0)
		std::memset(privKeyOut, 0, (size_t)len);
}
void iosuCrypto_readSeepromData(void* output, sint32 wordIndex, sint32 size)
{
	(void)wordIndex;
	if (output && size > 0)
		std::memset(output, 0, (size_t)size);
}

extern "C" {

CEMUWUA_API int cemuwua_abi_version(void)
{
	return CEMUWUA_ABI_VERSION;
}

CEMUWUA_API int cemuwua_title_info(const char* path, const uint8_t* key, const char* keys_txt,
                                   uint64_t* title_id, uint32_t* title_version,
                                   char* error, size_t error_len)
{
	// Nothing may unwind into the caller: a C++ exception crossing the C ABI aborts it.
	try
	{
		if (!path || !*path)
		{
			write_error(error, error_len, "no input path");
			return CEMUWUA_ERR_ARGS;
		}
		init_once();
		Failure fail{CEMUWUA_OK, {}};
		auto src = open_source(path, key, load_keys_txt(keys_txt), fail);
		if (!src)
		{
			write_error(error, error_len, fail.message);
			return fail.status;
		}
		uint64 tid = 0;
		uint32 ver = 0;
		if (!title_of(*src, tid, ver))
		{
			write_error(error, error_len, "the title has no readable code/app.xml");
			return CEMUWUA_ERR_READ;
		}
		if (title_id)
			*title_id = tid;
		if (title_version)
			*title_version = ver;
		return CEMUWUA_OK;
	}
	catch (const std::exception& e)
	{
		write_error(error, error_len, e.what());
		return CEMUWUA_ERR_READ;
	}
	catch (...)
	{
		write_error(error, error_len, "an unexpected internal error");
		return CEMUWUA_ERR_READ;
	}
}

CEMUWUA_API int cemuwua_create(const cemuwua_source* sources, size_t source_count,
                               const char* keys_txt, const char* source_json,
                               const char* out_path,
                               cemuwua_progress_fn progress, void* user,
                               char* error, size_t error_len)
{
	// Nothing may unwind into the caller: a C++ exception crossing the C ABI aborts it.
	try
	{
		if (!sources || source_count == 0 || !out_path || !*out_path)
		{
			write_error(error, error_len, "no sources or no output path");
			return CEMUWUA_ERR_ARGS;
		}
		init_once();
		auto keys = load_keys_txt(keys_txt);

		// Open everything and list it first, so a bad source fails before anything is written
		// and the progress total is known -- Cemu counts the same way before converting.
		struct Title
		{
			std::unique_ptr<TitleSource> src;
			std::string folder;
			uint64 titleId = 0;
			uint32 version = 0;
			std::vector<TitleSource::Entry> entries;
			std::vector<HashedFile> hashed;
		};
		std::vector<Title> titles;
		uint64 total = 0;
		for (size_t i = 0; i < source_count; i++)
		{
			if (!sources[i].path || !*sources[i].path)
			{
				write_error(error, error_len, "a source has no path");
				return CEMUWUA_ERR_ARGS;
			}
			Failure fail{CEMUWUA_OK, {}};
			Title t;
			t.src = open_source(sources[i].path, sources[i].key, keys, fail);
			if (!t.src)
			{
				write_error(error, error_len, fmt::format("{}: {}", sources[i].path, fail.message));
				return fail.status;
			}
			uint64 tid = 0;
			uint32 ver = 0;
			if (!title_of(*t.src, tid, ver))
			{
				write_error(error, error_len, fmt::format("{}: the title has no readable code/app.xml", sources[i].path));
				return CEMUWUA_ERR_READ;
			}
			// The folder name Cemu gives each title in a .wua (and looks titles up by).
			t.folder = fmt::format("{:016x}_v{}", tid, ver);
			t.titleId = tid;
			t.version = ver;
			for (auto& other : titles)
			{
				if (other.folder == t.folder)
				{
					write_error(error, error_len, fmt::format("{}: title {} is given twice", sources[i].path, t.folder));
					return CEMUWUA_ERR_ARGS;
				}
			}
			if (!t.src->List(t.entries))
			{
				write_error(error, error_len, fmt::format("{}: its file list could not be read", sources[i].path));
				return CEMUWUA_ERR_READ;
			}
			for (auto& e : t.entries)
				total += e.size;
			titles.push_back(std::move(t));
		}

		Output out;
		out.path = utf8_path(out_path);
		auto removeOutput = [&] {
			out.fs.reset();
			std::error_code ec;
			fs::remove(out.path, ec);
		};

		uint64 done = 0;
		bool cancelled = false;
		{
			ZArchiveWriter writer(&Output::NewOutputFile, &Output::WriteOutputData, &out);
			if (out.failed)
			{
				removeOutput();
				write_error(error, error_len, "the output file could not be created");
				return CEMUWUA_ERR_WRITE;
			}
			if (progress && progress(0, total, user) != 0)
				cancelled = true;
			for (auto& t : titles)
			{
				if (cancelled)
					break;
				writer.MakeDir(t.folder.c_str(), false);
				for (auto& e : t.entries)
				{
					std::string archivePath = t.folder + "/" + e.path;
					if (e.isDir)
					{
						writer.MakeDir(archivePath.c_str(), false);
						continue;
					}
					if (!writer.StartNewFile(archivePath.c_str()))
					{
						removeOutput();
						write_error(error, error_len, fmt::format("{}: could not be added to the archive", archivePath));
						return CEMUWUA_ERR_WRITE;
					}
					std::unique_ptr<EVP_MD_CTX, decltype(&EVP_MD_CTX_free)> sha(EVP_MD_CTX_new(), &EVP_MD_CTX_free);
					EVP_DigestInit(sha.get(), EVP_sha1());
					bool ok = t.src->Read(e, [&](const uint8* data, size_t n) {
						writer.AppendData(data, n);
						EVP_DigestUpdate(sha.get(), data, n);
						done += n;
						if (out.failed)
							return false;
						if (progress && progress(done, total, user) != 0)
						{
							cancelled = true;
							return false;
						}
						return true;
					});
					if (cancelled)
						break;
					if (out.failed)
					{
						removeOutput();
						write_error(error, error_len, "writing the output file failed (disk full?)");
						return CEMUWUA_ERR_WRITE;
					}
					if (!ok)
					{
						removeOutput();
						write_error(error, error_len, fmt::format("{}: reading or decrypting it failed -- the image may be damaged", e.path));
						return CEMUWUA_ERR_READ;
					}
					uint8 digest[SHA_DIGEST_LENGTH];
					EVP_DigestFinal_ex(sha.get(), digest, nullptr);
					t.hashed.push_back({e.path, e.size, hex(digest, sizeof(digest))});
				}
			}
			if (cancelled)
			{
				removeOutput();
				write_error(error, error_len, "cancelled");
				return CEMUWUA_CANCELLED;
			}
			// The manifest (see cemuwua.h), at the root where Cemu does not look.
			std::string m = "{\"format\":1,\"tool\":\"cemuwua\",\"source\":";
			m += (source_json && *source_json) ? std::string(source_json) : std::string("null");
			m += ",\"titles\":[";
			for (size_t i = 0; i < titles.size(); i++)
			{
				auto& t = titles[i];
				if (i)
					m += ",";
				m += fmt::format("\n{{\"folder\":{},\"title_id\":\"{:016x}\",\"version\":{},\"files\":[", json_string(t.folder), t.titleId, t.version);
				for (size_t j = 0; j < t.hashed.size(); j++)
				{
					auto& h = t.hashed[j];
					m += fmt::format("{}\n{{\"path\":{},\"size\":{},\"sha1\":\"{}\"}}", j ? "," : "", json_string(h.path), h.size, h.sha1);
				}
				m += "]}";
			}
			m += "]}\n";
			if (!writer.StartNewFile(CEMUWUA_MANIFEST_NAME))
			{
				removeOutput();
				write_error(error, error_len, "the manifest could not be added to the archive");
				return CEMUWUA_ERR_WRITE;
			}
			writer.AppendData(m.data(), m.size());
			writer.Finalize();
		}
		if (out.failed || !out.fs)
		{
			removeOutput();
			write_error(error, error_len, "writing the output file failed (disk full?)");
			return CEMUWUA_ERR_WRITE;
		}
#ifndef _WIN32
		out.fs->Flush(); // Windows' FileStream writes straight through and has no Flush
#endif
		out.fs.reset();

		// Read it back, as Cemu does after converting: every title folder has to be there.
		std::unique_ptr<ZArchiveReader> reader(ZArchiveReader::OpenFromFile(out.path));
		bool verified = reader != nullptr;
		for (auto& t : titles)
			verified = verified && reader->LookUp(t.folder, false, true) != ZARCHIVE_INVALID_NODE;
		verified = verified && reader->LookUp(CEMUWUA_MANIFEST_NAME, true, false) != ZARCHIVE_INVALID_NODE;
		reader.reset();
		if (!verified)
		{
			removeOutput();
			write_error(error, error_len, "the written archive did not read back");
			return CEMUWUA_ERR_VERIFY;
		}
		return CEMUWUA_OK;
	}
	catch (const std::exception& e)
	{
		write_error(error, error_len, e.what());
		return CEMUWUA_ERR_READ;
	}
	catch (...)
	{
		write_error(error, error_len, "an unexpected internal error");
		return CEMUWUA_ERR_READ;
	}
}

static bool parse_title_folder(const std::string& name, uint64& tid, uint32& version)
{
	// "<16 hex digits>_v<decimal version>", as cemuwua_create and Cemu name them.
	if (name.size() < 19 || name[16] != '_' || name[17] != 'v')
		return false;
	char* end = nullptr;
	std::string idPart = name.substr(0, 16);
	tid = std::strtoull(idPart.c_str(), &end, 16);
	if (!end || *end != '\0')
		return false;
	uint64 ver = std::strtoull(name.c_str() + 18, &end, 10);
	if (!end || *end != '\0' || ver > 0xFFFF)
		return false;
	version = (uint32)ver;
	return true;
}

CEMUWUA_API int cemuwua_wua_titles(const char* wua_path, cemuwua_title_fn each, void* user,
                                   char* error, size_t error_len)
{
	// Nothing may unwind into the caller: a C++ exception crossing the C ABI aborts it.
	try
	{
		if (!wua_path || !*wua_path)
		{
			write_error(error, error_len, "no input path");
			return CEMUWUA_ERR_ARGS;
		}
		std::unique_ptr<ZArchiveReader> reader(ZArchiveReader::OpenFromFile(utf8_path(wua_path)));
		if (!reader)
		{
			write_error(error, error_len, "not a readable .wua");
			return CEMUWUA_ERR_OPEN;
		}
		ZArchiveNodeHandle root = reader->LookUp("", false, true);
		if (root == ZARCHIVE_INVALID_NODE)
		{
			write_error(error, error_len, "the .wua has no root directory");
			return CEMUWUA_ERR_OPEN;
		}
		uint32 count = reader->GetDirEntryCount(root);
		for (uint32 i = 0; i < count; i++)
		{
			ZArchiveReader::DirEntry entry;
			if (!reader->GetDirEntry(root, i, entry) || !entry.isDirectory)
				continue;
			uint64 tid = 0;
			uint32 ver = 0;
			if (!parse_title_folder(std::string(entry.name), tid, ver))
				continue;
			if (each)
				each(tid, ver, user);
		}
		return CEMUWUA_OK;
	}
	catch (const std::exception& e)
	{
		write_error(error, error_len, e.what());
		return CEMUWUA_ERR_READ;
	}
	catch (...)
	{
		write_error(error, error_len, "an unexpected internal error");
		return CEMUWUA_ERR_READ;
	}
}

CEMUWUA_API int cemuwua_wua_manifest(const char* wua_path, char* out, size_t out_len,
                                     size_t* needed, char* error, size_t error_len)
{
	// Nothing may unwind into the caller: a C++ exception crossing the C ABI aborts it.
	try
	{
		if (!wua_path || !*wua_path)
		{
			write_error(error, error_len, "no input path");
			return CEMUWUA_ERR_ARGS;
		}
		std::unique_ptr<ZArchiveReader> reader(ZArchiveReader::OpenFromFile(utf8_path(wua_path)));
		if (!reader)
		{
			write_error(error, error_len, "not a readable .wua");
			return CEMUWUA_ERR_OPEN;
		}
		ZArchiveNodeHandle node = reader->LookUp(CEMUWUA_MANIFEST_NAME, true, false);
		if (node == ZARCHIVE_INVALID_NODE)
		{
			write_error(error, error_len, "the .wua has no " CEMUWUA_MANIFEST_NAME);
			return CEMUWUA_NO_MANIFEST;
		}
		uint64 size = reader->GetFileSize(node);
		if (needed)
			*needed = (size_t)size + 1;
		if (!out || out_len < size + 1)
		{
			write_error(error, error_len, "buffer too small");
			return CEMUWUA_ERR_ARGS;
		}
		if (reader->ReadFromFile(node, 0, size, out) != size)
		{
			write_error(error, error_len, "the manifest could not be read");
			return CEMUWUA_ERR_READ;
		}
		out[size] = '\0';
		return CEMUWUA_OK;
	}
	catch (const std::exception& e)
	{
		write_error(error, error_len, e.what());
		return CEMUWUA_ERR_READ;
	}
	catch (...)
	{
		write_error(error, error_len, "an unexpected internal error");
		return CEMUWUA_ERR_READ;
	}
}

CEMUWUA_API int cemuwua_read_file(const char* archive_path, const char* inner_path,
                                  uint64_t offset, uint8_t* out, size_t out_len,
                                  size_t* got, uint64_t* file_size,
                                  char* error, size_t error_len)
{
	// Nothing may unwind into the caller: a C++ exception crossing the C ABI aborts it.
	try
	{
		if (got)
			*got = 0;
		if (!archive_path || !*archive_path || !inner_path || !*inner_path || !got || (!out && out_len))
		{
			write_error(error, error_len, "no input path, file name or output");
			return CEMUWUA_ERR_ARGS;
		}
		std::unique_ptr<ZArchiveReader> reader(ZArchiveReader::OpenFromFile(utf8_path(archive_path)));
		if (!reader)
		{
			write_error(error, error_len, "not a readable ZArchive");
			return CEMUWUA_ERR_OPEN;
		}
		ZArchiveNodeHandle node = reader->LookUp(inner_path, true, false);
		if (node == ZARCHIVE_INVALID_NODE)
		{
			write_error(error, error_len, std::string(inner_path) + ": not in the archive");
			return CEMUWUA_NOT_FOUND;
		}
		uint64 size = reader->GetFileSize(node);
		if (file_size)
			*file_size = size;
		if (offset >= size || out_len == 0)
			return CEMUWUA_OK;
		uint64 want = std::min<uint64>(out_len, size - offset);
		if (reader->ReadFromFile(node, offset, want, out) != want)
		{
			write_error(error, error_len, std::string(inner_path) + ": could not be read");
			return CEMUWUA_ERR_READ;
		}
		*got = (size_t)want;
		return CEMUWUA_OK;
	}
	catch (const std::exception& e)
	{
		write_error(error, error_len, e.what());
		return CEMUWUA_ERR_READ;
	}
	catch (...)
	{
		write_error(error, error_len, "an unexpected internal error");
		return CEMUWUA_ERR_READ;
	}
}

namespace {

int stream_file(ZArchiveReader& reader, ZArchiveNodeHandle node, const std::string& path,
                cemuwua_file_fn begin, cemuwua_data_fn data, void* user, std::vector<uint8>& buf, std::string& failure)
{
	if (node == ZARCHIVE_INVALID_NODE)
	{
		failure = path + ": not found";
		return CEMUWUA_ERR_READ;
	}
	uint64 size = reader.GetFileSize(node);
	if (begin && begin(path.c_str(), size, user) != 0)
		return CEMUWUA_CANCELLED;
	uint64 offset = 0;
	while (offset < size)
	{
		uint64 want = std::min<uint64>(buf.size(), size - offset);
		uint64 got = reader.ReadFromFile(node, offset, want, buf.data());
		if (got != want)
		{
			failure = path + ": reading it failed -- the archive may be damaged";
			return CEMUWUA_ERR_READ;
		}
		if (data && data(buf.data(), (size_t)got, user) != 0)
			return CEMUWUA_CANCELLED;
		offset += got;
	}
	return CEMUWUA_OK;
}

int stream_dir(ZArchiveReader& reader, ZArchiveNodeHandle dir, const std::string& prefix,
               cemuwua_file_fn begin, cemuwua_data_fn data, void* user, std::vector<uint8>& buf, std::string& failure)
{
	uint32 count = reader.GetDirEntryCount(dir);
	for (uint32 i = 0; i < count; i++)
	{
		ZArchiveReader::DirEntry entry;
		if (!reader.GetDirEntry(dir, i, entry))
		{
			failure = prefix + ": a directory entry could not be read";
			return CEMUWUA_ERR_READ;
		}
		std::string path = prefix + "/" + std::string(entry.name);
		ZArchiveNodeHandle node = reader.LookUp(path, true, true);
		if (node == ZARCHIVE_INVALID_NODE)
		{
			failure = path + ": not found";
			return CEMUWUA_ERR_READ;
		}
		if (entry.isDirectory)
		{
			int r = stream_dir(reader, node, path, begin, data, user, buf, failure);
			if (r != CEMUWUA_OK)
				return r;
			continue;
		}
		int r = stream_file(reader, node, path, begin, data, user, buf, failure);
		if (r != CEMUWUA_OK)
			return r;
	}
	return CEMUWUA_OK;
}

} // namespace

CEMUWUA_API int cemuwua_wua_stream(const char* wua_path, cemuwua_file_fn begin,
                                   cemuwua_data_fn data, void* user,
                                   char* error, size_t error_len)
{
	// Nothing may unwind into the caller: a C++ exception crossing the C ABI aborts it.
	try
	{
		if (!wua_path || !*wua_path)
		{
			write_error(error, error_len, "no input path");
			return CEMUWUA_ERR_ARGS;
		}
		std::unique_ptr<ZArchiveReader> reader(ZArchiveReader::OpenFromFile(utf8_path(wua_path)));
		if (!reader)
		{
			write_error(error, error_len, "not a readable .wua");
			return CEMUWUA_ERR_OPEN;
		}
		ZArchiveNodeHandle root = reader->LookUp("", false, true);
		if (root == ZARCHIVE_INVALID_NODE)
		{
			write_error(error, error_len, "the .wua has no root directory");
			return CEMUWUA_ERR_OPEN;
		}
		std::vector<uint8> buf(TRANSFER);
		std::string failure;
		uint32 count = reader->GetDirEntryCount(root);
		for (uint32 i = 0; i < count; i++)
		{
			ZArchiveReader::DirEntry entry;
			if (!reader->GetDirEntry(root, i, entry) || !entry.isDirectory)
				continue;
			std::string name(entry.name);
			uint64 tid;
			uint32 ver;
			if (!parse_title_folder(name, tid, ver))
				continue;
			ZArchiveNodeHandle node = reader->LookUp(name, false, true);
			int r = stream_dir(*reader, node, name, begin, data, user, buf, failure);
			if (r != CEMUWUA_OK)
			{
				write_error(error, error_len, r == CEMUWUA_CANCELLED ? std::string("cancelled") : failure);
				return r;
			}
		}
		return CEMUWUA_OK;
	}
	catch (const std::exception& e)
	{
		write_error(error, error_len, e.what());
		return CEMUWUA_ERR_READ;
	}
	catch (...)
	{
		write_error(error, error_len, "an unexpected internal error");
		return CEMUWUA_ERR_READ;
	}
}

} // extern "C"


// --- any ZArchive (.zar) -------------------------------------------------------------------
// The same writer and reader, for archives whose files the caller supplies itself (Xenia's
// .zar of an Xbox 360 disc's files). The caller walks the source and hashes what it adds; the
// manifest it builds is stored like a .wua's.

struct cemuwua_zar
{
	Output out;
	std::unique_ptr<ZArchiveWriter> writer;
	bool inFile = false;
};

extern "C" {

CEMUWUA_API cemuwua_zar* cemuwua_zar_create(const char* out_path, char* error, size_t error_len)
{
	// Nothing may unwind into the caller: a C++ exception crossing the C ABI aborts it.
	try
	{
		if (!out_path || !*out_path)
		{
			write_error(error, error_len, "no output path");
			return nullptr;
		}
		auto* z = new cemuwua_zar();
		z->out.path = utf8_path(out_path);
		z->writer = std::make_unique<ZArchiveWriter>(&Output::NewOutputFile, &Output::WriteOutputData, &z->out);
		if (z->out.failed)
		{
			z->writer.reset();
			write_error(error, error_len, "the output file could not be created");
			delete z;
			return nullptr;
		}
		return z;
	}
	catch (const std::exception& e)
	{
		write_error(error, error_len, e.what());
		return nullptr;
	}
	catch (...)
	{
		write_error(error, error_len, "an unexpected internal error");
		return nullptr;
	}
}

CEMUWUA_API int cemuwua_zar_dir(cemuwua_zar* z, const char* path)
{
	// Nothing may unwind into the caller: a C++ exception crossing the C ABI aborts it.
	try
	{
		if (!z || !path || !*path)
			return CEMUWUA_ERR_ARGS;
		return z->writer->MakeDir(path, true) ? CEMUWUA_OK : CEMUWUA_ERR_WRITE;
	}
	catch (const std::exception& e)
	{
		return CEMUWUA_ERR_READ;
	}
	catch (...)
	{
		return CEMUWUA_ERR_READ;
	}
}

CEMUWUA_API int cemuwua_zar_file(cemuwua_zar* z, const char* path)
{
	// Nothing may unwind into the caller: a C++ exception crossing the C ABI aborts it.
	try
	{
		if (!z || !path || !*path || std::string_view(path) == CEMUWUA_MANIFEST_NAME)
			return CEMUWUA_ERR_ARGS;
		z->inFile = z->writer->StartNewFile(path);
		return z->inFile ? CEMUWUA_OK : CEMUWUA_ERR_WRITE;
	}
	catch (const std::exception& e)
	{
		return CEMUWUA_ERR_READ;
	}
	catch (...)
	{
		return CEMUWUA_ERR_READ;
	}
}

CEMUWUA_API int cemuwua_zar_write(cemuwua_zar* z, const uint8_t* data, size_t len)
{
	// Nothing may unwind into the caller: a C++ exception crossing the C ABI aborts it.
	try
	{
		if (!z || !z->inFile || (!data && len))
			return CEMUWUA_ERR_ARGS;
		z->writer->AppendData(data, len);
		return z->out.failed ? CEMUWUA_ERR_WRITE : CEMUWUA_OK;
	}
	catch (const std::exception& e)
	{
		return CEMUWUA_ERR_READ;
	}
	catch (...)
	{
		return CEMUWUA_ERR_READ;
	}
}

CEMUWUA_API void cemuwua_zar_abort(cemuwua_zar* z)
{
	// Nothing may unwind into the caller: a C++ exception crossing the C ABI aborts it.
	try
	{
		if (!z)
			return;
		z->writer.reset(); // without Finalize: nothing more is written
		z->out.fs.reset();
		std::error_code ec;
		fs::remove(z->out.path, ec);
		delete z;
	}
	catch (const std::exception& e)
	{
		return;
	}
	catch (...)
	{
		return;
	}
}

CEMUWUA_API int cemuwua_zar_finish(cemuwua_zar* z, const char* manifest_json, char* error, size_t error_len)
{
	// Nothing may unwind into the caller: a C++ exception crossing the C ABI aborts it.
	try
	{
		if (!z)
			return CEMUWUA_ERR_ARGS;
		if (manifest_json && *manifest_json)
		{
			if (!z->writer->StartNewFile(CEMUWUA_MANIFEST_NAME))
			{
				write_error(error, error_len, "the manifest could not be added to the archive");
				cemuwua_zar_abort(z);
				return CEMUWUA_ERR_WRITE;
			}
			z->writer->AppendData(manifest_json, std::strlen(manifest_json));
		}
		z->writer->Finalize();
		z->writer.reset();
		if (z->out.failed || !z->out.fs)
		{
			write_error(error, error_len, "writing the output file failed (disk full?)");
			cemuwua_zar_abort(z);
			return CEMUWUA_ERR_WRITE;
		}
#ifndef _WIN32
		z->out.fs->Flush();
#endif
		z->out.fs.reset();
		std::unique_ptr<ZArchiveReader> reader(ZArchiveReader::OpenFromFile(z->out.path));
		if (!reader || reader->LookUp("", false, true) == ZARCHIVE_INVALID_NODE)
		{
			reader.reset();
			write_error(error, error_len, "the written archive did not read back");
			cemuwua_zar_abort(z);
			return CEMUWUA_ERR_VERIFY;
		}
		delete z;
		return CEMUWUA_OK;
	}
	catch (const std::exception& e)
	{
		write_error(error, error_len, e.what());
		return CEMUWUA_ERR_READ;
	}
	catch (...)
	{
		write_error(error, error_len, "an unexpected internal error");
		return CEMUWUA_ERR_READ;
	}
}

CEMUWUA_API int cemuwua_zar_stream(const char* zar_path, cemuwua_file_fn begin,
                                   cemuwua_data_fn data, void* user,
                                   char* error, size_t error_len)
{
	// Nothing may unwind into the caller: a C++ exception crossing the C ABI aborts it.
	try
	{
		if (!zar_path || !*zar_path)
		{
			write_error(error, error_len, "no input path");
			return CEMUWUA_ERR_ARGS;
		}
		std::unique_ptr<ZArchiveReader> reader(ZArchiveReader::OpenFromFile(utf8_path(zar_path)));
		if (!reader)
		{
			write_error(error, error_len, "not a readable ZArchive");
			return CEMUWUA_ERR_OPEN;
		}
		ZArchiveNodeHandle root = reader->LookUp("", false, true);
		if (root == ZARCHIVE_INVALID_NODE)
		{
			write_error(error, error_len, "the archive has no root directory");
			return CEMUWUA_ERR_OPEN;
		}
		std::vector<uint8> buf(TRANSFER);
		std::string failure;
		uint32 count = reader->GetDirEntryCount(root);
		for (uint32 i = 0; i < count; i++)
		{
			ZArchiveReader::DirEntry entry;
			if (!reader->GetDirEntry(root, i, entry))
			{
				write_error(error, error_len, "a root entry could not be read");
				return CEMUWUA_ERR_READ;
			}
			std::string name(entry.name);
			if (!entry.isDirectory && name == CEMUWUA_MANIFEST_NAME)
				continue;
			int r;
			if (entry.isDirectory)
				r = stream_dir(*reader, reader->LookUp(name, false, true), name, begin, data, user, buf, failure);
			else
				r = stream_file(*reader, reader->LookUp(name, true, false), name, begin, data, user, buf, failure);
			if (r != CEMUWUA_OK)
			{
				write_error(error, error_len, r == CEMUWUA_CANCELLED ? std::string("cancelled") : failure);
				return r;
			}
		}
		return CEMUWUA_OK;
	}
	catch (const std::exception& e)
	{
		write_error(error, error_len, e.what());
		return CEMUWUA_ERR_READ;
	}
	catch (...)
	{
		write_error(error, error_len, "an unexpected internal error");
		return CEMUWUA_ERR_READ;
	}
}

} // extern "C"
