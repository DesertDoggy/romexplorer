// Streaming build only (DOLPHINRVZ_WITH_STREAMING, target dolphinrvz_streaming in
// dolphinrvz_target.cmake): dolphinrvz_extract_stream, dolphinrvz_convert_stream and the
// dolphinrvz_reader_* API declared in dolphinrvz.h. Everything here goes through DiscIO's
// public BlobReader interface -- no Dolphin source is modified.

#include "dolphinrvz.h"
#include "dolphinrvz_internal.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <filesystem>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <vector>

#include "Common/FileUtil.h"
#include "Common/IOFile.h"

#include "DiscIO/Blob.h"

// ---------------------------------------------------------------------------
// Compressor thread count
// ---------------------------------------------------------------------------
//
// DiscIO's MultithreadedCompressor (GCZ/WIA/RVZ creation) sizes its pool from
// std::thread::hardware_concurrency(), with no setting. On toolchains where that function is
// out of line in the C++ runtime (libstdc++, libc++), defining it here makes every call inside
// this shared library bind to this definition at link time, so dolphinrvz_set_compress_threads
// can choose the count without touching Dolphin's source. Hidden, so it never interposes on
// other libraries in the process. MSVC's is inline and calls into its runtime DLL, so there the
// setting is ignored.
namespace
{
std::atomic<unsigned int> g_compress_threads{0};
}

#if !defined(_WIN32)
#include <unistd.h>

__attribute__((visibility("hidden"))) unsigned int std::thread::hardware_concurrency() noexcept
{
    const unsigned int forced = g_compress_threads.load(std::memory_order_relaxed);
    if (forced)
        return forced;
    const long online = sysconf(_SC_NPROCESSORS_ONLN);
    return online > 0 ? static_cast<unsigned int>(online) : 0;
}
#endif

void dolphinrvz_set_compress_threads(uint32_t threads)
{
    g_compress_threads.store(threads, std::memory_order_relaxed);
}

namespace
{
// Same chunking as DiscIO::ConvertToPlain (FileBlob.cpp): the container's block size,
// doubled until it reaches 512 KiB, or 512 KiB for formats without blocks.
u64 PlainBufferSize(const DiscIO::BlobReader& blob)
{
    constexpr u64 DESIRED_BUFFER_SIZE = 0x80000;
    u64 buffer_size = blob.GetBlockSize();
    if (buffer_size == 0)
        return DESIRED_BUFFER_SIZE;
    while (buffer_size < DESIRED_BUFFER_SIZE)
        buffer_size *= 2;
    return buffer_size;
}

// Turns whatever reads a converter makes into one complete, in-order stream of the input's
// bytes: re-reads of already-delivered ranges are dropped, and a forward jump first reads
// and delivers the skipped range from the same reader.
class InputTap
{
public:
    InputTap(std::string name, u64 data_size, DolphinRvzDataCb cb, void* user_data)
        : m_name(std::move(name)), m_data_size(data_size), m_cb(cb), m_user_data(user_data)
    {
    }

    void SetSource(DiscIO::BlobReader* source) { m_source = source; }

    // Called after every successful read the converter makes.
    void Observe(u64 offset, u64 size, const u8* data)
    {
        if (m_stopped || m_failed)
            return;
        const u64 end = std::min(offset + size, m_data_size);
        if (end <= m_cursor)
            return;
        if (offset > m_cursor && !Fill(offset))
            return;
        const u64 skip = m_cursor - offset;
        Deliver(data + skip, end - m_cursor);
    }

    // Delivers everything the converter never read.
    bool Finish() { return Fill(m_data_size) && !m_stopped; }

private:
    bool Fill(u64 until)
    {
        if (m_buffer.empty())
            m_buffer.resize(0x80000);
        while (m_cursor < until && !m_stopped)
        {
            const u64 n = std::min<u64>(m_buffer.size(), until - m_cursor);
            if (!m_source->Read(m_cursor, n, m_buffer.data()))
            {
                m_failed = true;
                return false;
            }
            Deliver(m_buffer.data(), n);
        }
        return !m_failed;
    }

    void Deliver(const u8* data, u64 size)
    {
        while (size && !m_stopped)
        {
            const u32 n = static_cast<u32>(std::min<u64>(size, 0x40000000));
            if (m_cb && !m_cb(0, m_name.c_str(), m_cursor, data, n, m_user_data))
                m_stopped = true;
            m_cursor += n;
            data += n;
            size -= n;
        }
    }

    std::string m_name;
    u64 m_data_size;
    DolphinRvzDataCb m_cb;
    void* m_user_data;
    DiscIO::BlobReader* m_source = nullptr;
    u64 m_cursor = 0;
    bool m_stopped = false;
    bool m_failed = false;
    std::vector<u8> m_buffer;
};

// Forwards everything to the wrapped reader, reporting each Read to an InputTap.
// (Pattern: DiscIO::ScrubbedBlob.)
class ObservingBlob final : public DiscIO::BlobReader
{
public:
    ObservingBlob(std::unique_ptr<DiscIO::BlobReader> inner, InputTap* tap)
        : m_inner(std::move(inner)), m_tap(tap)
    {
        m_tap->SetSource(m_inner.get());
    }

    DiscIO::BlobType GetBlobType() const override { return m_inner->GetBlobType(); }
    // Copies read unobserved; InputTap fills in whatever they covered.
    std::unique_ptr<DiscIO::BlobReader> CopyReader() const override { return m_inner->CopyReader(); }

    u64 GetRawSize() const override { return m_inner->GetRawSize(); }
    u64 GetDataSize() const override { return m_inner->GetDataSize(); }
    DiscIO::DataSizeType GetDataSizeType() const override { return m_inner->GetDataSizeType(); }

    u64 GetBlockSize() const override { return m_inner->GetBlockSize(); }
    bool HasFastRandomAccessInBlock() const override { return m_inner->HasFastRandomAccessInBlock(); }
    std::string GetCompressionMethod() const override { return m_inner->GetCompressionMethod(); }
    std::optional<int> GetCompressionLevel() const override { return m_inner->GetCompressionLevel(); }

    bool Read(u64 offset, u64 size, u8* out_ptr) override
    {
        if (!m_inner->Read(offset, size, out_ptr))
            return false;
        m_tap->Observe(offset, size, out_ptr);
        return true;
    }

    bool SupportsReadWiiDecrypted(u64 offset, u64 size, u64 partition_data_offset) const override
    {
        return m_inner->SupportsReadWiiDecrypted(offset, size, partition_data_offset);
    }
    bool ReadWiiDecrypted(u64 offset, u64 size, u8* out_ptr, u64 partition_data_offset) override
    {
        return m_inner->ReadWiiDecrypted(offset, size, out_ptr, partition_data_offset);
    }
    bool IsCached() const override { return m_inner->IsCached(); }

private:
    std::unique_ptr<DiscIO::BlobReader> m_inner;
    InputTap* m_tap;
};
}  // namespace

int dolphinrvz_extract_stream(const char* input_path, const char* output_path, const char* user_dir,
                              DolphinRvzDataCb on_data, DolphinRvzProgressCb on_progress,
                              void* user_data)
{
    using dolphinrvz_internal::SetLastError;
    if (!input_path || !*input_path)
    {
        SetLastError("input_path is required");
        return -1;
    }
    dolphinrvz_internal::EnsureMinimalInit(user_dir);

    std::unique_ptr<DiscIO::BlobReader> blob = DiscIO::CreateBlobReader(input_path);
    if (!blob)
    {
        SetLastError("The input file could not be opened");
        return -2;
    }
    if (blob->GetDataSizeType() != DiscIO::DataSizeType::Accurate)
    {
        SetLastError("The input's plain size is not known exactly; it can't be unpacked");
        return -2;
    }

    const std::string out_name = output_path ? output_path : "";
    std::optional<File::IOFile> out;
    if (output_path && *output_path)
    {
        out.emplace(out_name, "wb");
        if (!out->IsOpen())
        {
            SetLastError("Failed to open the output file");
            return -5;
        }
    }

    const u64 data_size = blob->GetDataSize();
    std::vector<u8> buffer(PlainBufferSize(*blob));
    const u64 num_buffers = (data_size + buffer.size() - 1) / buffer.size();
    const u64 progress_monitor = std::max<u64>(1, num_buffers / 100);

    int rc = 0;
    for (u64 i = 0; i < num_buffers; i++)
    {
        if (i % progress_monitor == 0 && on_progress &&
            !on_progress("Unpacking", 100.0f * static_cast<float>(i) / static_cast<float>(num_buffers),
                         user_data))
        {
            SetLastError("cancelled by caller");
            rc = -6;
            break;
        }
        const u64 pos = i * buffer.size();
        const u64 n = std::min<u64>(buffer.size(), data_size - pos);
        if (!blob->Read(pos, n, buffer.data()))
        {
            SetLastError("Failed to read from the input file");
            rc = -5;
            break;
        }
        if (out && !out->WriteBytes(buffer.data(), n))
        {
            SetLastError("Failed to write the output file");
            rc = -5;
            break;
        }
        if (on_data && !on_data(0, out_name.c_str(), pos, buffer.data(), static_cast<u32>(n), user_data))
        {
            SetLastError("stopped by data callback");
            rc = -6;
            break;
        }
    }

    if (out)
    {
        out->Close();
        if (rc != 0)
            File::Delete(out_name);
    }
    if (rc == 0)
        SetLastError("");
    return rc;
}

namespace
{
// Convert with an InputTap on the input: `on_input_data` receives every input byte once, in
// order. open_input: see dolphinrvz_internal::OpenInput (nullptr = options->input_path).
int ConvertObserved(const DolphinRvzConvertOptions* options, DolphinRvzDataCb on_input_data,
                    const dolphinrvz_internal::OpenInput& open_input)
{
    // The relay records a stop request in `stopped`, which Convert polls at each progress
    // point (it can't be signalled from inside a read without faking a read error).
    bool stopped = false;
    struct Relay
    {
        DolphinRvzDataCb cb;
        void* user;
        bool* stopped;
    } relay{on_input_data, options->user_data, &stopped};
    const DolphinRvzDataCb relay_cb = [](uint32_t index, const char* name, uint64_t offset,
                                         const void* data, uint32_t size, void* user) -> int {
        auto* r = static_cast<Relay*>(user);
        if (r->cb && !r->cb(index, name, offset, data, size, r->user))
        {
            *r->stopped = true;
            return 0;
        }
        return 1;
    };

    const std::string name = options->input_path ? options->input_path : "";
    std::unique_ptr<InputTap> tap;
    const auto wrap = [&](std::unique_ptr<DiscIO::BlobReader> inner) -> std::unique_ptr<DiscIO::BlobReader> {
        tap = std::make_unique<InputTap>(name, inner->GetDataSize(), relay_cb, &relay);
        return std::make_unique<ObservingBlob>(std::move(inner), tap.get());
    };

    const int rc = dolphinrvz_internal::Convert(options, wrap, &stopped, open_input);
    if (rc != 0 || !tap)
        return rc;

    // The observing reader died with Convert; deliver whatever the converter never read
    // from a fresh reader.
    std::unique_ptr<DiscIO::BlobReader> tail =
        open_input ? open_input() : DiscIO::CreateBlobReader(options->input_path);
    if (!tail)
    {
        dolphinrvz_internal::SetLastError("The input could not be reopened");
        return -5;
    }
    tap->SetSource(tail.get());
    if (!tap->Finish())
    {
        dolphinrvz_internal::SetLastError(stopped ? "stopped by data callback" : "Failed to read from the input");
        return stopped ? -6 : -5;
    }
    return 0;
}

// Plain-data BlobReader over a random-access DolphinRvzSource. Copies share one lock, since
// Dolphin reads through several copies (volume, converter), partly from its own threads.
class SourceBlob final : public DiscIO::BlobReader
{
public:
    struct Shared
    {
        const DolphinRvzSource* source;
        std::mutex mutex;
    };

    explicit SourceBlob(std::shared_ptr<Shared> shared) : m_shared(std::move(shared)) {}

    DiscIO::BlobType GetBlobType() const override { return DiscIO::BlobType::PLAIN; }
    std::unique_ptr<DiscIO::BlobReader> CopyReader() const override
    {
        return std::make_unique<SourceBlob>(m_shared);
    }
    u64 GetRawSize() const override { return m_shared->source->size; }
    u64 GetDataSize() const override { return m_shared->source->size; }
    DiscIO::DataSizeType GetDataSizeType() const override { return DiscIO::DataSizeType::Accurate; }
    u64 GetBlockSize() const override { return 0; }
    bool HasFastRandomAccessInBlock() const override { return true; }
    std::string GetCompressionMethod() const override { return {}; }
    std::optional<int> GetCompressionLevel() const override { return std::nullopt; }

    bool Read(u64 offset, u64 size, u8* out_ptr) override
    {
        const DolphinRvzSource& src = *m_shared->source;
        if (offset > src.size || size > src.size - offset)
            return false;
        std::lock_guard<std::mutex> lock(m_shared->mutex);
        return size == 0 || src.read_at(src.user_data, offset, out_ptr, size) == 0;
    }

private:
    std::shared_ptr<Shared> m_shared;
};
}  // namespace

int dolphinrvz_convert_stream(const DolphinRvzConvertOptions* options, DolphinRvzDataCb on_input_data)
{
    if (!options || !options->input_path)
        return dolphinrvz_internal::Convert(options, nullptr, nullptr);  // reports the error
    return ConvertObserved(options, on_input_data, nullptr);
}

int dolphinrvz_convert_from_source(const DolphinRvzConvertOptions* options, const DolphinRvzSource* source,
                                   DolphinRvzDataCb on_input_data)
{
    using dolphinrvz_internal::SetLastError;
    if (!options || !source || (!source->read && !source->read_at))
    {
        SetLastError("options and a source with read or read_at are required");
        return -1;
    }
    dolphinrvz_internal::EnsureMinimalInit(options->user_dir);

    if (source->read_at)
    {
        auto shared = std::make_shared<SourceBlob::Shared>();
        shared->source = source;
        const dolphinrvz_internal::OpenInput open_input = [shared]() -> std::unique_ptr<DiscIO::BlobReader> {
            return std::make_unique<SourceBlob>(shared);
        };
        if (on_input_data)
            return ConvertObserved(options, on_input_data, open_input);
        return dolphinrvz_internal::Convert(options, nullptr, nullptr, open_input);
    }

    // Sequential only: the converter needs random access, so spool to a temporary file,
    // handing the bytes to on_input_data on the way, then convert that file.
    std::error_code ec;
    const std::filesystem::path dir =
        options->user_dir && *options->user_dir ?
            std::filesystem::path(reinterpret_cast<const char8_t*>(options->user_dir)) :
            std::filesystem::temp_directory_path(ec);
    std::filesystem::create_directories(dir, ec);
    const auto stamp = std::chrono::steady_clock::now().time_since_epoch().count();
    const std::filesystem::path spool = dir / ("dolphinrvz_spool_" + std::to_string(stamp) + ".iso");
    const std::u8string spool_u8 = spool.u8string();
    const std::string spool_path(reinterpret_cast<const char*>(spool_u8.c_str()), spool_u8.size());

    int rc = 0;
    {
        File::IOFile out(spool_path, "wb");
        if (!out.IsOpen())
        {
            SetLastError("Cannot create the spool file");
            return -5;
        }
        const std::string name = options->input_path ? options->input_path : "";
        std::vector<u8> buffer(0x80000);
        u64 pos = 0;
        while (pos < source->size)
        {
            const u64 want = std::min<u64>(buffer.size(), source->size - pos);
            const int64_t n = source->read(source->user_data, buffer.data(), want);
            if (n <= 0 || static_cast<u64>(n) > want)
            {
                SetLastError("Source read failed");
                rc = -5;
                break;
            }
            if (!out.WriteBytes(buffer.data(), static_cast<size_t>(n)))
            {
                SetLastError("Failed to write the spool file");
                rc = -5;
                break;
            }
            if (on_input_data &&
                !on_input_data(0, name.c_str(), pos, buffer.data(), static_cast<u32>(n), options->user_data))
            {
                SetLastError("stopped by data callback");
                rc = -6;
                break;
            }
            pos += static_cast<u64>(n);
        }
    }
    if (rc == 0)
    {
        DolphinRvzConvertOptions spooled = *options;
        spooled.input_path = spool_path.c_str();
        rc = dolphinrvz_internal::Convert(&spooled, nullptr, nullptr);
    }
    File::Delete(spool_path);
    return rc;
}

// ---------------------------------------------------------------------------
// Random access reader
// ---------------------------------------------------------------------------

struct DolphinRvzReader
{
    std::unique_ptr<DiscIO::BlobReader> blob;
};

DolphinRvzReader* dolphinrvz_reader_open(const char* path, const char* user_dir)
{
    if (!path || !*path)
    {
        dolphinrvz_internal::SetLastError("path is required");
        return nullptr;
    }
    dolphinrvz_internal::EnsureMinimalInit(user_dir);
    std::unique_ptr<DiscIO::BlobReader> blob = DiscIO::CreateBlobReader(path);
    if (!blob)
    {
        dolphinrvz_internal::SetLastError("The input file could not be opened");
        return nullptr;
    }
    return new DolphinRvzReader{std::move(blob)};
}

void dolphinrvz_reader_close(DolphinRvzReader* reader)
{
    delete reader;
}

int dolphinrvz_reader_get_info(DolphinRvzReader* reader, DolphinRvzReaderInfo* out)
{
    if (!reader || !out)
    {
        dolphinrvz_internal::SetLastError("NULL argument");
        return -1;
    }
    out->data_size = reader->blob->GetDataSize();
    out->raw_size = reader->blob->GetRawSize();
    out->block_size = reader->blob->GetBlockSize();
    out->blob_type = static_cast<int32_t>(reader->blob->GetBlobType());
    out->data_size_accurate = reader->blob->GetDataSizeType() == DiscIO::DataSizeType::Accurate;
    return 0;
}

int dolphinrvz_reader_read(DolphinRvzReader* reader, uint64_t offset, void* buffer, uint64_t size)
{
    if (!reader || (!buffer && size))
    {
        dolphinrvz_internal::SetLastError("NULL argument");
        return -1;
    }
    const u64 data_size = reader->blob->GetDataSize();
    if (offset > data_size || size > data_size - offset)
    {
        dolphinrvz_internal::SetLastError("read past end of disc image");
        return -1;
    }
    if (size && !reader->blob->Read(offset, size, static_cast<u8*>(buffer)))
    {
        dolphinrvz_internal::SetLastError("read failed");
        return -5;
    }
    return 0;
}
