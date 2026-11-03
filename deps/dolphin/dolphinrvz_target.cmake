# Included (deferred) by dolphinrvz_project_include.cmake once dolphin/CMakeLists.txt has
# finished processing -- discio/uicommon exist as targets by this point.
#
# Defines dolphinrvz: a shared library exposing DiscIO's ISO/GCZ/WIA/RVZ converter (the
# same functionality as Dolphin's own `dolphin-tool convert`) through the C API in
# dolphinrvz.h, instead of a CLI executable. Links the same discio + uicommon targets (and
# therefore the same third-party dependencies) the official dolphin-tool CLI links -- see
# README.dolphinrvz.md for why.

# dolphinrvz links discio, which links PUBLIC core (see README.dolphinrvz.md) -- and as
# a shared library, every symbol core's object files reference must resolve at link time
# (unlike a plain .lib, which can defer that to whatever executable eventually links it).
# core calls out to a set of Host_* callback functions that it expects some embedding
# frontend to implement (update window title, report focus state, etc) -- confirmed
# directly: the official dolphin-tool CLI satisfies these the same way, by compiling in
# this exact no-op stub file rather than defining its own.
#
# Two targets share everything below: dolphinrvz (the default library) and
# dolphinrvz_streaming (the same plus user/dolphinrvz_stream.cpp and
# -DDOLPHINRVZ_WITH_STREAMING -- see dolphinrvz.h). Both link the same already-built
# discio/uicommon static libraries from this one build tree, so building the streaming
# variant never rebuilds Dolphin, and the default target is unaffected by its existence.
# The streaming library is also named dolphinrvz, in its own with_streaming/ output dir.
function(dolphinrvz_add_library name)
add_library(${name} SHARED
  ${DOLPHINRVZ_USER_DIR}/dolphinrvz.cpp
  ${DOLPHINRVZ_USER_DIR}/dolphinrvz.h
  ${DOLPHINRVZ_USER_DIR}/dolphinrvz_internal.h
  ${CMAKE_SOURCE_DIR}/Source/Core/DolphinTool/ToolHeadlessPlatform.cpp
  ${ARGN}
)

target_include_directories(${name} PRIVATE ${DOLPHINRVZ_USER_DIR})
target_compile_definitions(${name} PRIVATE DOLPHINRVZ_BUILDING_DLL)

# Source/CMakeLists.txt sets these via add_definitions() (directory-scoped) for
# everything under add_subdirectory(Source) -- discio/uicommon/core included. dolphinrvz
# is added via a deferred call in dolphin/CMakeLists.txt's own top-level scope, outside
# that subtree, so it never inherits them and needs its own copy (confirmed directly:
# without NOMINMAX, DiscIO/WIABlob.h's std::numeric_limits<...>::max() gets mangled by
# <windows.h>'s max() macro; without UNICODE/_UNICODE, Common/StringUtil.h's Windows
# code paths pick different overloads than the rest of Dolphin was built with).
if(CMAKE_SYSTEM_NAME MATCHES "Windows")
  target_compile_definitions(${name} PRIVATE
    NOMINMAX
    UNICODE
    _UNICODE
    WIN32_LEAN_AND_MEAN
    _SCL_SECURE_NO_WARNINGS
    _CRT_SECURE_NO_WARNINGS
    _CRT_SECURE_NO_DEPRECATE
    _CRT_NONSTDC_NO_WARNINGS
    _SILENCE_CXX17_CODECVT_HEADER_DEPRECATION_WARNING
  )
endif()

# Source/CMakeLists.txt sets CMAKE_CXX_STANDARD 23 (directory-scoped, non-MSVC) /
# CMAKE_CXX_STANDARD 23 (MSVC) for everything under add_subdirectory(Source) -- same
# out-of-subtree situation as the NOMINMAX/UNICODE defines above, so dolphinrvz needs its
# own copy here too (confirmed directly: without this, dolphinrvz.cpp's includes of
# DiscIO/WIABlob.h and Common/StringUtil.h fail to compile under GCC's default -std=gnu++20,
# since MultithreadedCompressor.h's std::expected and StringUtil.h's std::to_underlying are
# both C++23-only).
set_target_properties(${name} PROPERTIES
  CXX_STANDARD 23
  CXX_STANDARD_REQUIRED ON
  CXX_EXTENSIONS OFF
  CXX_VISIBILITY_PRESET hidden
  VISIBILITY_INLINES_HIDDEN ON
)

target_link_libraries(${name} PRIVATE
  discio
  uicommon
  fmt::fmt
)

# Deliberately NOT linking use_pch: dolphin/CMakeLists.txt's own /W4 /WX
# (add_compile_options, top-level directory scope) already apply to dolphinrvz
# regardless, since it's added via a deferred call in that same scope -- but the shared
# PCH (Source/PCH) requires exactly matching preprocessor defines across every
# consumer, or MSVC emits a PCH-consistency warning (C4651) that /WX then turns into a
# hard error (confirmed directly). dolphinrvz.cpp is one small file; it doesn't need
# the PCH's compile-time savings, so it's simplest to just not use it here.

if(APPLE)
  set_target_properties(${name} PROPERTIES INSTALL_NAME_DIR "@rpath")
endif()
endfunction()

dolphinrvz_add_library(dolphinrvz)

dolphinrvz_add_library(dolphinrvz_streaming ${DOLPHINRVZ_USER_DIR}/dolphinrvz_stream.cpp)
target_compile_definitions(dolphinrvz_streaming PRIVATE DOLPHINRVZ_WITH_STREAMING)
set_target_properties(dolphinrvz_streaming PROPERTIES
  OUTPUT_NAME dolphinrvz
  LIBRARY_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/with_streaming
  RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/with_streaming
  ARCHIVE_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/with_streaming
)
if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
  target_link_options(dolphinrvz_streaming PRIVATE
    "LINKER:--version-script=${DOLPHINRVZ_USER_DIR}/dolphinrvz_streaming.map")
  set_property(TARGET dolphinrvz_streaming APPEND PROPERTY LINK_DEPENDS ${DOLPHINRVZ_USER_DIR}/dolphinrvz_streaming.map)
endif()
