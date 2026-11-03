#include "rahasher_stream.h"

#include "../src/Util.h"

#include <rcheevos/include/rc_hash.h>

#include <algorithm>
#include <cctype>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <string>

#ifdef HAVE_CHD
void rc_hash_init_chd_cdreader(); /* in HashCHD.cpp */
#endif

void initHash3DS(const std::string& systemDir); /* in Hash3DS.cpp */

namespace
{
  static const int RC_CONSOLE_MAX = 90;

  /* Serializes calls into rc_hash_generate, which relies on process-wide static state
   * inside rcheevos (cdreader setup, Hash3DS init, etc.) that isn't safe to touch from
   * multiple threads at once. */
  static std::mutex g_hash_mutex;

  static thread_local std::string g_last_error;

  struct rahasher_file_ctx
  {
    uint64_t expected_total_bytes = 0;
    uint64_t bytes_hashed = 0;
    /* Set by filereader_read when a read hits a short read before reaching
     * expected_total_bytes, meaning the hash algorithm needed data that isn't on
     * disk yet rather than having genuinely reached the end of the file. */
    bool need_more_data = false;
  };

  /* rc_hash's filereader.read callback has no userdata parameter, so the context for
   * the in-flight call is reached via this thread-local instead. */
  static thread_local rahasher_file_ctx* g_active_ctx = nullptr;

  /* rahasher_hash_vfs: every file comes from the caller. Same thread-local reasoning. */
  static thread_local const rahasher_vfs* g_active_vfs = nullptr;

  /* The directory rahasher_hash_vfs pretends its files are in; only base names reach the vfs. */
  static const char* const VFS_ROOT = "/rahasher-vfs/";

  struct vfs_file
  {
    int64_t handle;
    uint64_t size;
    uint64_t pos;
  };

  static std::string to_lower(std::string value)
  {
    for (char& c : value)
      c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    return value;
  }

  static std::string get_basename(const std::string& path)
  {
    const size_t slash_pos = path.find_last_of("/\\");
    if (slash_pos == std::string::npos)
      return path;

    return path.substr(slash_pos + 1);
  }

  static std::string get_extension(const std::string& path)
  {
    const std::string base = get_basename(path);
    const size_t dot_pos = base.find_last_of('.');
    if (dot_pos == std::string::npos)
      return std::string();

    return to_lower(base.substr(dot_pos));
  }

  static void RC_CCONV iterator_verbose_bridge(const char* /*message*/, const rc_hash_iterator_t* /*iterator*/)
  {
    /* No verbose logging sink in the simplified file-path API. */
  }

  static void RC_CCONV iterator_error_bridge(const char* message, const rc_hash_iterator_t* /*iterator*/)
  {
    g_last_error = message ? message : "unknown error";
  }

  static void* RC_CCONV vfs_open(const char* path_utf8)
  {
    const rahasher_vfs* vfs = g_active_vfs;
    uint64_t size = 0;
    const int64_t handle = vfs->open(vfs->userdata, get_basename(path_utf8 ? path_utf8 : "").c_str(), &size);
    if (handle <= 0)
      return nullptr;
    return new vfs_file{handle, size, 0};
  }

  static void RC_CCONV vfs_seek(void* file_handle, int64_t offset, int origin)
  {
    vfs_file* f = static_cast<vfs_file*>(file_handle);
    int64_t base = origin == SEEK_END ? static_cast<int64_t>(f->size) : origin == SEEK_CUR ? static_cast<int64_t>(f->pos) : 0;
    const int64_t at = base + offset;
    f->pos = at < 0 ? 0 : static_cast<uint64_t>(at);
  }

  static int64_t RC_CCONV vfs_tell(void* file_handle)
  {
    return static_cast<int64_t>(static_cast<vfs_file*>(file_handle)->pos);
  }

  static size_t RC_CCONV vfs_read(void* file_handle, void* buffer, size_t requested_bytes)
  {
    vfs_file* f = static_cast<vfs_file*>(file_handle);
    if (f->pos >= f->size || requested_bytes == 0)
      return 0;
    const uint64_t len = std::min<uint64_t>(requested_bytes, f->size - f->pos);
    const rahasher_vfs* vfs = g_active_vfs;
    const int64_t got = vfs->read(vfs->userdata, f->handle, f->pos, buffer, len);
    if (got <= 0)
      return 0;
    f->pos += static_cast<uint64_t>(got);
    return static_cast<size_t>(got);
  }

  static void RC_CCONV vfs_close(void* file_handle)
  {
    vfs_file* f = static_cast<vfs_file*>(file_handle);
    const rahasher_vfs* vfs = g_active_vfs;
    if (vfs && vfs->close)
      vfs->close(vfs->userdata, f->handle);
    delete f;
  }

  static void* RC_CCONV filereader_open(const char* path_utf8)
  {
    return util::openFile(nullptr, path_utf8 ? path_utf8 : "", "rb");
  }

  static void RC_CCONV filereader_seek(void* file_handle, int64_t offset, int origin)
  {
#if defined(_WIN32)
    _fseeki64(static_cast<FILE*>(file_handle), offset, origin);
#else
    fseeko(static_cast<FILE*>(file_handle), static_cast<off_t>(offset), origin);
#endif
  }

  static int64_t RC_CCONV filereader_tell(void* file_handle)
  {
#if defined(_WIN32)
    return _ftelli64(static_cast<FILE*>(file_handle));
#else
    return static_cast<int64_t>(ftello(static_cast<FILE*>(file_handle)));
#endif
  }

  static size_t RC_CCONV filereader_read(void* file_handle, void* buffer, size_t requested_bytes)
  {
    size_t read_bytes = fread(buffer, 1, requested_bytes, static_cast<FILE*>(file_handle));

    rahasher_file_ctx* ctx = g_active_ctx;
    if (ctx)
    {
      ctx->bytes_hashed += static_cast<uint64_t>(read_bytes);

      /* A short read below the expected final size means the algorithm asked for
       * data that isn't available yet, not that it legitimately reached the end. */
      if (read_bytes < requested_bytes && ctx->expected_total_bytes != 0 &&
          ctx->bytes_hashed < ctx->expected_total_bytes)
      {
        ctx->need_more_data = true;
      }
    }

    return read_bytes;
  }

  static void RC_CCONV filereader_close(void* file_handle)
  {
    fclose(static_cast<FILE*>(file_handle));
  }
}

/* Declared by Hash3DS.cpp (and others) as an extern hook; must stay at global scope
 * with C++ linkage matching that declaration. */
void rhash_log_error_message(const char* message)
{
  g_last_error = message ? message : "unknown error";
  std::fprintf(stderr, "%s\n", message ? message : "");
}

const char* rahasher_get_last_error(void)
{
  return g_last_error.c_str();
}

int rahasher_hash_file(
  uint32_t console_id,
  const char* file_path,
  const char* system_dir,
  uint64_t expected_total_bytes,
  char out_hash[33],
  int* out_done)
{
  if (!file_path || !out_hash || !out_done || console_id == 0)
    return RAHASHER_FILE_ERR_INVALID_ARG;

  *out_done = 0;

  if (console_id > RC_CONSOLE_MAX)
  {
    g_last_error = "console_id must be a specific console id (<= 90)";
    return RAHASHER_FILE_ERR_INVALID_ARG;
  }

  const std::string extension = get_extension(file_path);
  if (extension == ".m3u")
  {
    g_last_error = "m3u input is not supported; provide the referenced disc file instead";
    return RAHASHER_FILE_ERR_UNSUPPORTED;
  }

  if (expected_total_bytes != 0)
  {
    /* Some formats (whole-file hashes: Game Boy, NES, SNES, etc.) determine their own
     * size via a plain seek-to-end/tell on file_path as it exists right now, rather
     * than expecting a specific total -- they would happily hash a partial file to a
     * "successful" but wrong result instead of ever hitting a short read. Gate on the
     * file's current size up front so we only ever attempt a hash once it has reached
     * the size the caller told us to expect. */
    FILE* probe = util::openFile(nullptr, file_path, "rb");
    if (!probe)
    {
      g_last_error = "failed to open file";
      return RAHASHER_FILE_ERR_IO;
    }

#if defined(_WIN32)
    _fseeki64(probe, 0, SEEK_END);
    const uint64_t current_size = static_cast<uint64_t>(_ftelli64(probe));
#else
    fseeko(probe, 0, SEEK_END);
    const uint64_t current_size = static_cast<uint64_t>(ftello(probe));
#endif
    fclose(probe);

    if (current_size < expected_total_bytes)
      return RAHASHER_FILE_OK; /* *out_done stays 0: not enough written yet */
  }

  rahasher_file_ctx ctx;
  ctx.expected_total_bytes = expected_total_bytes;

  char hash[33] = {};

  rc_hash_iterator_t iterator;
  std::memset(&iterator, 0, sizeof(iterator));
  rc_hash_initialize_iterator(&iterator, file_path, nullptr, 0);

  iterator.callbacks.verbose_message = iterator_verbose_bridge;
  iterator.callbacks.error_message = iterator_error_bridge;
  iterator.callbacks.filereader.open = filereader_open;
  iterator.callbacks.filereader.seek = filereader_seek;
  iterator.callbacks.filereader.tell = filereader_tell;
  iterator.callbacks.filereader.read = filereader_read;
  iterator.callbacks.filereader.close = filereader_close;

  g_last_error.clear();

  std::lock_guard<std::mutex> lock(g_hash_mutex);

  if (console_id == RC_CONSOLE_NINTENDO_3DS)
    initHash3DS(system_dir ? system_dir : ".");

#ifdef HAVE_CHD
  if (extension == ".chd")
    rc_hash_init_chd_cdreader();
  else
#endif
    rc_hash_init_default_cdreader();

  g_active_ctx = &ctx;
  const int ok = rc_hash_generate(hash, console_id, &iterator);
  g_active_ctx = nullptr;

  rc_hash_destroy_iterator(&iterator);

  if (ok)
  {
    std::memcpy(out_hash, hash, 33);
    *out_done = 1;
    return RAHASHER_FILE_OK;
  }

  if (ctx.need_more_data)
    return RAHASHER_FILE_OK; /* not an error: just don't have enough data yet */

  if (g_last_error.empty())
    g_last_error = "hash generation failed";

  return RAHASHER_FILE_ERR_HASH;
}

int rahasher_hash_vfs(uint32_t console_id, const char* file_name, const rahasher_vfs* vfs, char out_hash[33])
{
  if (!file_name || !vfs || !vfs->open || !vfs->read || !out_hash || console_id == 0 || console_id > RC_CONSOLE_MAX)
    return RAHASHER_FILE_ERR_INVALID_ARG;

  const std::string path = std::string(VFS_ROOT) + get_basename(file_name);
  char hash[33] = {};

  rc_hash_iterator_t iterator;
  std::memset(&iterator, 0, sizeof(iterator));
  rc_hash_initialize_iterator(&iterator, path.c_str(), nullptr, 0);
  iterator.callbacks.verbose_message = iterator_verbose_bridge;
  iterator.callbacks.error_message = iterator_error_bridge;
  iterator.callbacks.filereader.open = vfs_open;
  iterator.callbacks.filereader.seek = vfs_seek;
  iterator.callbacks.filereader.tell = vfs_tell;
  iterator.callbacks.filereader.read = vfs_read;
  iterator.callbacks.filereader.close = vfs_close;

  g_last_error.clear();
  std::lock_guard<std::mutex> lock(g_hash_mutex);
  rc_hash_init_default_cdreader();
  g_active_vfs = vfs;
  const int ok = rc_hash_generate(hash, console_id, &iterator);
  g_active_vfs = nullptr;
  rc_hash_destroy_iterator(&iterator);

  if (ok)
  {
    std::memcpy(out_hash, hash, 33);
    return RAHASHER_FILE_OK;
  }
  if (g_last_error.empty())
    g_last_error = "hash generation failed";
  return RAHASHER_FILE_ERR_HASH;
}
