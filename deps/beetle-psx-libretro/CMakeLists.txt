# Builds psxpbp: Beetle PSX's PBP reader as one shared library (see psxpbp.h). Its own
# project, compiling the handful of Beetle and libretro-common sources the reader needs;
# nothing upstream is edited.
cmake_minimum_required(VERSION 3.16)
project(psxpbp C)

set(CMAKE_C_STANDARD 99)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(CMAKE_C_VISIBILITY_PRESET hidden)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

get_filename_component(_ext "${CMAKE_CURRENT_SOURCE_DIR}/../../external" ABSOLUTE)
set(BEETLE_ROOT "${_ext}/beetle-psx-libretro" CACHE PATH "beetle-psx-libretro source tree")
if(NOT EXISTS "${BEETLE_ROOT}/mednafen/cdrom/CDAccess_PBP.c")
  message(FATAL_ERROR "missing ${BEETLE_ROOT}/mednafen/cdrom/CDAccess_PBP.c -- git submodule update --init external/beetle-psx-libretro")
endif()
set(B "${BEETLE_ROOT}")
set(L "${BEETLE_ROOT}/libretro-common")

add_library(psxpbp SHARED
  "${CMAKE_CURRENT_SOURCE_DIR}/psxpbp.c"
  # The reader, and what it uses to rebuild sectors and their subchannel.
  "${B}/mednafen/cdrom/CDAccess_PBP.c"
  "${B}/mednafen/cdrom/CDUtility.c"
  "${B}/mednafen/cdrom/cdaccess_track.c"
  "${B}/mednafen/cdrom/edc_crc32.c"
  "${B}/mednafen/cdrom/recover-raw.c"
  "${B}/mednafen/cdrom/lec.c"
  "${B}/mednafen/cdrom/galois.c"
  "${B}/mednafen/cdrom/l-ec.c"
  "${B}/mednafen/cdstream.c"
  "${B}/mednafen/general.c"
  # libretro-common: file streams, raw inflate, paths.
  "${L}/streams/file_stream.c"
  "${L}/vfs/vfs_implementation.c"
  "${L}/formats/data_transfer.c"
  "${L}/encodings/encoding_deflate.c"
  "${L}/encodings/encoding_crc32.c"
  "${L}/encodings/encoding_utf.c"
  "${L}/features/features_cpu.c"
  "${L}/memmap/memmap.c"
  "${L}/file/file_path.c"
  "${L}/file/file_path_io.c"
  "${L}/string/stdstring.c"
  "${L}/string/rstrtod.c"
  "${L}/compat/compat_strl.c"
  "${L}/compat/compat_strcasestr.c"
  "${L}/compat/fopen_utf8.c"
  "${L}/time/rtime.c")
set_target_properties(psxpbp PROPERTIES PREFIX "lib")
if(WIN32)
  set_target_properties(psxpbp PROPERTIES PREFIX "")
endif()
target_include_directories(psxpbp PRIVATE "${B}" "${B}/mednafen" "${L}/include" "${CMAKE_CURRENT_SOURCE_DIR}")
if(NOT MSVC)
  # Upstream's own warnings are not ours to fix.
  target_compile_options(psxpbp PRIVATE -w)
endif()
if(CMAKE_SYSTEM_NAME MATCHES "Linux|Android")
  target_link_options(psxpbp PRIVATE -Wl,--no-undefined)
  target_link_libraries(psxpbp PRIVATE pthread)
endif()
if(MINGW)
  target_link_options(psxpbp PRIVATE -static -static-libgcc)
endif()
if(APPLE)
  set_target_properties(psxpbp PROPERTIES INSTALL_NAME_DIR "@rpath")
endif()
