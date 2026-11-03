#include "rahasher_stream.h"

#include <cstdio>
#include <cstdlib>

int main(int argc, char** argv)
{
  if (argc < 3)
  {
    std::fprintf(stderr, "Usage: %s <console_id> <file_path> [system_dir]\n", argv[0]);
    return 1;
  }

  const uint32_t console_id = static_cast<uint32_t>(std::strtoul(argv[1], nullptr, 10));
  const char* file_path = argv[2];
  const char* system_dir = (argc >= 4) ? argv[3] : ".";

  char hash[33] = {};
  int done = 0;
  const int rc = rahasher_hash_file(console_id, file_path, system_dir, 0, hash, &done);

  if (rc != RAHASHER_FILE_OK)
  {
    std::fprintf(stderr, "hash failed: %s\n", rahasher_get_last_error());
    return 1;
  }

  if (!done)
  {
    /* expected_total_bytes was 0 (unknown) here, so this shouldn't happen -- rahasher_
     * hash_file only reports "not done" when it was told a total to wait for. */
    std::fprintf(stderr, "hash incomplete\n");
    return 1;
  }

  std::printf("%s\n", hash);
  return 0;
}
