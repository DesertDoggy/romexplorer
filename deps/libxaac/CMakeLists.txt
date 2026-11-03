# Builds rxaac: libxaac's encoder plus the flat shim in rxaac.c, as one shared library.
#
# Its own project rather than a target added to the top-level CMakeLists.txt, so nothing
# upstream is edited: this file includes upstream's own source lists and compile settings
# and adds one target. The decoder, the test benches and the fuzzers are not built.
cmake_minimum_required(VERSION 3.5.1)
project(rxaac C)

# The upstream libxaac tree: external/libxaac, or XAAC_ROOT when given (scripts/build.sh
# passes it). This directory, deps/libxaac, holds only the shim and this file.
if(NOT XAAC_ROOT)
  get_filename_component(XAAC_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../../external/libxaac" ABSOLUTE)
endif()
set(XAAC_CONFIG_DIR "${CMAKE_CURRENT_BINARY_DIR}")
set(CMAKE_STATIC_LIBRARY_PREFIX "")
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
# Everything is compiled hidden, libxaac's own objects included: only the rxaac_* functions
# (marked RXAAC_API) are exported. Otherwise an ELF build exports several hundred libxaac
# symbols, some with names as plain as `filter`, and the dynamic linker would let a symbol of
# the same name in the host program stand in for the library's own.
set(CMAKE_C_VISIBILITY_PRESET hidden)
if(POLICY CMP0063)
  cmake_policy(SET CMP0063 NEW)
endif()
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

# utils.cmake keys its flags off CMAKE_SYSTEM_PROCESSOR and knows these spellings only.
if(CMAKE_SYSTEM_PROCESSOR MATCHES "^(AMD64|amd64|x64)$")
  set(CMAKE_SYSTEM_PROCESSOR "x86_64")
elseif(CMAKE_SYSTEM_PROCESSOR MATCHES "^(ARM64|arm64)$")
  set(CMAKE_SYSTEM_PROCESSOR "aarch64")
endif()

# utils.cmake probes C++ flags for the fuzzers; this project is C only, so the probe is
# replaced by the two things it would have set.
set(CMAKE_C_STANDARD 99)
if(CMAKE_SYSTEM_PROCESSOR STREQUAL "aarch64")
  add_compile_options(-std=gnu99 -march=armv8-a)
  add_definitions(-DARMV8)
elseif(CMAKE_SYSTEM_PROCESSOR STREQUAL "x86_64")
  add_compile_options(-D_X86_ -O3 -fwrapv)
  add_definitions(-DX86_64 -D_X86_64_)
endif()
add_definitions(-DLOUDNESS_LEVELING_SUPPORT)

include("${XAAC_ROOT}/common/common.cmake")
include("${XAAC_ROOT}/encoder/libxaacenc.cmake")

add_library(rxaac SHARED "${CMAKE_CURRENT_SOURCE_DIR}/rxaac.c")
target_include_directories(rxaac PRIVATE "${XAAC_ROOT}/common" "${XAAC_ROOT}/encoder" "${XAAC_ROOT}/encoder/drc_src")
target_link_libraries(rxaac PRIVATE libxaacenc)
if(NOT WIN32)
  target_link_libraries(rxaac PRIVATE m)
endif()
if(WIN32)
  # rxaac.dll, not librxaac.dll: the name src/rom/native_lib.rs looks for on Windows.
  set_target_properties(rxaac PROPERTIES PREFIX "")
  if(MINGW)
    # No dependency on libgcc / winpthread DLLs: the library is deployed alone.
    target_link_options(rxaac PRIVATE -static-libgcc -static)
  endif()
endif()
