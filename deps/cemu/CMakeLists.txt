# Builds cemuwua: Wii U .wud/.wux and title folders into Cemu's .wua, as one shared library.
#
# Its own project, compiling the handful of Cemu sources the conversion needs (disc reader,
# FST decryption, ticket/TMD parsing, AES) plus ZArchive and zstd, against the stand-in
# headers in shim/ (see shim/Common/precompiled.h and shim/openssl/openssl_min.h for what
# they replace). Nothing upstream is edited. scripts/build.sh is the entry point.
cmake_minimum_required(VERSION 3.16)
project(cemuwua C CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_C_STANDARD 11)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
# Only the cemuwua_* functions are exported (CEMUWUA_API); Cemu's, ZArchive's and zstd's
# own symbols stay inside the library.
set(CMAKE_C_VISIBILITY_PRESET hidden)
set(CMAKE_CXX_VISIBILITY_PRESET hidden)
set(CMAKE_VISIBILITY_INLINES_HIDDEN ON)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

# The upstream trees, all submodules under external/ by default.
get_filename_component(_ext "${CMAKE_CURRENT_SOURCE_DIR}/../../external" ABSOLUTE)
set(CEMU_ROOT "${_ext}/cemu" CACHE PATH "Cemu source tree")
set(ZARCHIVE_ROOT "${_ext}/ZArchive" CACHE PATH "ZArchive source tree")
set(ZSTD_ROOT "${_ext}/zstd" CACHE PATH "zstd source tree")
set(FMT_ROOT "${_ext}/fmt" CACHE PATH "fmt source tree")
foreach(_f "${CEMU_ROOT}/src/Cafe/Filesystem/FST/FST.cpp" "${ZARCHIVE_ROOT}/src/zarchivewriter.cpp"
           "${ZSTD_ROOT}/lib/zstd.h" "${FMT_ROOT}/include/fmt/format.h")
  if(NOT EXISTS "${_f}")
    message(FATAL_ERROR "missing ${_f} -- check out the submodule (git submodule update --init external/...)")
  endif()
endforeach()

if(CMAKE_SYSTEM_PROCESSOR MATCHES "^(x86_64|AMD64|amd64|x64)$")
  set(_x64 ON)
endif()

# --- zstd (ZArchive's codec), the library parts only ---------------------------------------
file(GLOB ZSTD_SOURCES
  "${ZSTD_ROOT}/lib/common/*.c"
  "${ZSTD_ROOT}/lib/compress/*.c"
  "${ZSTD_ROOT}/lib/decompress/*.c")
add_library(cemuwua_zstd STATIC ${ZSTD_SOURCES})
target_include_directories(cemuwua_zstd PUBLIC "${ZSTD_ROOT}/lib")
# No hand-written assembly: the one .S file is an x86-64 Huffman decoder, and decoding is
# only used to verify the archive once it is written.
target_compile_definitions(cemuwua_zstd PRIVATE ZSTD_DISABLE_ASM=1 XXH_NAMESPACE=cemuwua_)
# zstd marks its API visibility("default"); keep it inside cemuwua like everything else.
target_compile_definitions(cemuwua_zstd PUBLIC ZSTDLIB_VISIBLE= ZSTDERRORLIB_VISIBLE= ZDICTLIB_VISIBLE=)

# --- ZArchive -------------------------------------------------------------------------------
add_library(cemuwua_zarchive STATIC
  "${ZARCHIVE_ROOT}/src/zarchivewriter.cpp"
  "${ZARCHIVE_ROOT}/src/zarchivereader.cpp"
  "${ZARCHIVE_ROOT}/src/sha_256.c")
target_include_directories(cemuwua_zarchive PUBLIC "${ZARCHIVE_ROOT}/include" "${ZARCHIVE_ROOT}/src")
target_link_libraries(cemuwua_zarchive PRIVATE cemuwua_zstd)

# --- Cemu's sources and the library ---------------------------------------------------------
set(CEMU_SOURCES
  "${CEMU_ROOT}/src/Cafe/Filesystem/WUD/wud.cpp"
  "${CEMU_ROOT}/src/Cafe/Filesystem/FST/FST.cpp"
  "${CEMU_ROOT}/src/Cemu/ncrypto/ncrypto.cpp"
  "${CEMU_ROOT}/src/util/crypto/aes128.cpp"
  "${CEMU_ROOT}/src/Common/cpu_features.cpp")
if(WIN32)
  list(APPEND CEMU_SOURCES "${CEMU_ROOT}/src/Common/windows/FileStream_win32.cpp")
else()
  list(APPEND CEMU_SOURCES "${CEMU_ROOT}/src/Common/unix/FileStream_unix.cpp")
endif()

add_library(cemuwua SHARED
  ${CEMU_SOURCES}
  "${CMAKE_CURRENT_SOURCE_DIR}/cemuwua.cpp"
  "${CMAKE_CURRENT_SOURCE_DIR}/openssl_min.cpp")
set_target_properties(cemuwua PROPERTIES PREFIX "lib")
if(WIN32)
  # cemuwua.dll, like the other libraries the app loads on Windows.
  set_target_properties(cemuwua PROPERTIES PREFIX "")
endif()
# shim/ first: its headers stand in for Cemu's precompiled header, boost and OpenSSL.
target_include_directories(cemuwua PRIVATE
  "${CMAKE_CURRENT_SOURCE_DIR}/shim"
  "${CEMU_ROOT}/src"
  "${FMT_ROOT}/include"
  "${CMAKE_CURRENT_SOURCE_DIR}")
target_compile_definitions(cemuwua PRIVATE FMT_HEADER_ONLY=1)
if(_x64)
  target_compile_definitions(cemuwua PRIVATE ARCH_X86_64=1)
endif()
# Cemu force-includes its precompiled header into every file; some of them rely on it.
if(MSVC)
  target_compile_options(cemuwua PRIVATE "/FI${CMAKE_CURRENT_SOURCE_DIR}/shim/Common/precompiled.h")
else()
  target_compile_options(cemuwua PRIVATE -include "${CMAKE_CURRENT_SOURCE_DIR}/shim/Common/precompiled.h"
    -Wno-multichar)
endif()
target_link_libraries(cemuwua PRIVATE cemuwua_zarchive cemuwua_zstd)
if(CMAKE_SYSTEM_NAME MATCHES "Linux|Android")
  # Fail the link, not the dlopen: a symbol Cemu's sources expect from a file this library
  # leaves out would otherwise only show up when the app tries to load it.
  target_link_options(cemuwua PRIVATE -Wl,--no-undefined)
endif()
if(MINGW)
  # No libstdc++/libgcc/winpthread DLLs to deploy beside it, as for 7z.dll.
  target_link_options(cemuwua PRIVATE -static -static-libgcc -static-libstdc++)
endif()
if(APPLE)
  set_target_properties(cemuwua PROPERTIES INSTALL_NAME_DIR "@rpath")
endif()
