// Minimal POSIX osdlib.h implementation for the chdman shared library build.
//
// MAME's own POSIX osdlib (src/osd/modules/lib/osdlib_unix.cpp / osdlib_macosx.cpp)
// unconditionally #include <SDL2/SDL.h> -- appropriate for the full SDLMAME emulator,
// but a needless heavy dependency for chdman, which is a batch CHD conversion tool and
// never touches the clipboard or the desktop environment. This file implements just the
// handful of osdlib.h functions chdman's link graph actually pulls in, with no SDL
// dependency. See user/Makefile.chdman_lib for how this replaces osdlib_unix.cpp /
// osdlib_macosx.cpp on non-Windows targets.
//
// If a future chdman/MAME util change references another osd_* symbol not implemented
// in osdcore.cpp (shared, unchanged across platforms) or here, add a minimal
// implementation here rather than pulling in the full SDL-based
// osdlib_unix.cpp/osdlib_macosx.cpp. A shared library links fine with undefined symbols
// and only fails when the missing one is first called, so the Linux link uses
// -Wl,-z,defs (see Makefile.chdman_lib) to surface it at build time instead.

#include "osdcore.h"
#include "modules/lib/osdlib.h"

#include <cstdlib>
#include <string>
#include <string_view>
#include <system_error>

#if !defined(_WIN32)
#include <unistd.h>
#endif

void osd_process_kill()
{
#if defined(_WIN32)
    std::abort();
#else
    _exit(-1);
#endif
}

const char *osd_getenv(const char *name)
{
    return getenv(name);
}

int osd_setenv(const char *name, const char *value, int overwrite)
{
    return setenv(name, value, overwrite);
}

std::string osd_get_clipboard_text() noexcept
{
    // No desktop clipboard in this batch build; nothing to return.
    return std::string();
}

std::error_condition osd_set_clipboard_text(std::string_view text) noexcept
{
    (void)text;
    return std::make_error_condition(std::errc::not_supported);
}
