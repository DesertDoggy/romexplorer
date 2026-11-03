#include "../rahasher_stream.h"

#include <rcheevos/include/rc_hash.h>

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

namespace
{
  bool write_file(const std::string& path, const uint8_t* data, size_t size)
  {
    FILE* fp = std::fopen(path.c_str(), "wb");
    if (!fp)
      return false;

    const size_t written = std::fwrite(data, 1, size, fp);
    std::fclose(fp);
    return written == size;
  }
}

int main()
{
  const std::string temp_path = "/tmp/rahasher_stream_test.gba";

  std::vector<uint8_t> data;
  data.resize(1024 * 1024);
  for (size_t i = 0; i < data.size(); ++i)
    data[i] = static_cast<uint8_t>((i * 131u) & 0xFFu);

  char expected_hash[33] = {};
  if (!write_file(temp_path, data.data(), data.size()) ||
      !rc_hash_generate_from_file(expected_hash, RC_CONSOLE_GAMEBOY_ADVANCE, temp_path.c_str()))
  {
    std::fprintf(stderr, "reference hash generation failed\n");
    std::remove(temp_path.c_str());
    return 1;
  }

  /* Case 1: whole file already on disk -- one call, done immediately, matches the
   * reference hash. */
  {
    char hash[33] = {};
    int done = 0;
    const int rc = rahasher_hash_file(RC_CONSOLE_GAMEBOY_ADVANCE, temp_path.c_str(), ".",
      static_cast<uint64_t>(data.size()), hash, &done);

    if (rc != RAHASHER_FILE_OK || !done || std::strcmp(hash, expected_hash) != 0)
    {
      std::fprintf(stderr, "whole-file case failed (rc=%d done=%d)\n", rc, done);
      std::remove(temp_path.c_str());
      return 1;
    }
  }

  /* Case 2: file written incrementally -- *out_done must stay false until every byte
   * expected_total_bytes promised is actually on disk (proving we never compute a
   * hash from a still-partial whole-file-hash format), then flip true with the same
   * hash as the reference once the file reaches full size. */
  {
    const std::string partial_path = temp_path + ".partial";
    const size_t step = 64 * 1024;
    bool saw_not_done = false;
    char hash[33] = {};
    int done = 0;
    size_t written = 0;

    while (true)
    {
      const size_t len = written < data.size() ? written : data.size();
      if (!write_file(partial_path, data.data(), len))
      {
        std::fprintf(stderr, "failed to write partial file\n");
        std::remove(partial_path.c_str());
        std::remove(temp_path.c_str());
        return 1;
      }

      done = 0;
      const int rc = rahasher_hash_file(RC_CONSOLE_GAMEBOY_ADVANCE, partial_path.c_str(), ".",
        static_cast<uint64_t>(data.size()), hash, &done);

      if (rc != RAHASHER_FILE_OK)
      {
        std::fprintf(stderr, "incremental case failed at %zu bytes (rc=%d): %s\n",
          len, rc, rahasher_get_last_error());
        std::remove(partial_path.c_str());
        std::remove(temp_path.c_str());
        return 1;
      }

      if (!done)
        saw_not_done = true;

      if (len >= data.size())
        break;

      written += step;
    }

    std::remove(partial_path.c_str());

    if (!saw_not_done || !done || std::strcmp(hash, expected_hash) != 0)
    {
      std::fprintf(stderr, "incremental case did not behave as expected (saw_not_done=%d done=%d)\n",
        saw_not_done, done);
      std::remove(temp_path.c_str());
      return 1;
    }
  }

  std::remove(temp_path.c_str());

  std::printf("file hash test passed\n");
  return 0;
}
