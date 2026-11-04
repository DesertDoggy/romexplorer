# Builds rvgmstream: libvgmstream plus the flat shim in rvgm.c, as one shared library.
#
# Its own project rather than vgmstream's top-level CMakeLists.txt, so nothing upstream is
# edited and none of its plugins, players or external codec downloads are configured: this
# file globs the same source directories upstream's src/CMakeLists.txt does and adds one
# target. vgmstream's built-in decoders are compiled in (ADX, HCA, DSP, PS-ADPCM, CD-XA,
# ATRAC9, MP3 via minimp3, Xbox/IMA/MS ADPCM, PCM and the rest of its own coding/). When
# FFMPEG_ROOT names a deps/ffmpeg release tree, FFmpeg's static audio-only libraries are linked
# in too (WMA, XMA, ATRAC3/3plus, AAC, AC-3, Opus, Vorbis), still as this one library.
# libvorbis, mpg123, CELT and Speex are not used. G.719 and G.722.1 are left out on purpose:
# they come from ITU/Polycom reference code with no clear licence.
cmake_minimum_required(VERSION 3.10)
project(rvgmstream C)

# The upstream tree: external/vgmstream, or VGM_ROOT when given (scripts/build.sh passes it).
if(NOT VGM_ROOT)
  get_filename_component(VGM_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../../external/vgmstream" ABSOLUTE)
endif()
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
# Only rvgm_* (marked RVGM_API) is exported; vgmstream's own few thousand symbols stay
# hidden so none of them can stand in for a host symbol of the same name.
set(CMAKE_C_VISIBILITY_PRESET hidden)
if(POLICY CMP0063)
  cmake_policy(SET CMP0063 NEW)
endif()
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

set(VGM_SRC "${VGM_ROOT}/src")
file(GLOB VGM_SOURCES
  "${VGM_SRC}/*.c"
  "${VGM_SRC}/base/*.c"
  "${VGM_SRC}/coding/*.c"
  "${VGM_SRC}/coding/libs/*.c"
  "${VGM_SRC}/layout/*.c"
  "${VGM_SRC}/meta/*.c"
  "${VGM_SRC}/util/*.c")

add_library(rvgmstream SHARED ${VGM_SOURCES} "${CMAKE_CURRENT_SOURCE_DIR}/rvgm.c")
target_include_directories(rvgmstream PRIVATE
  "${VGM_SRC}" "${VGM_SRC}/base" "${VGM_SRC}/coding" "${VGM_SRC}/layout" "${VGM_SRC}/meta" "${VGM_SRC}/util"
  "${VGM_ROOT}/ext_includes")
target_compile_definitions(rvgmstream PRIVATE VGM_USE_ATRAC9)
if(FFMPEG_ROOT)
  target_compile_definitions(rvgmstream PRIVATE VGM_USE_FFMPEG)
  target_include_directories(rvgmstream PRIVATE "${FFMPEG_ROOT}/include")
  # Link order matters for static archives: users before what they use.
  foreach(lib avformat avcodec swresample avutil)
    target_link_libraries(rvgmstream PRIVATE "${FFMPEG_ROOT}/lib/lib${lib}.a")
  endforeach()
  if(WIN32)
    target_link_libraries(rvgmstream PRIVATE bcrypt)
  elseif(APPLE)
    target_link_libraries(rvgmstream PRIVATE "-framework CoreFoundation" "-framework Security")
  else()
    target_link_libraries(rvgmstream PRIVATE pthread)
    # FFmpeg's hand-written assembly has no hidden-visibility markers; keep its symbols out
    # of the export table all the same.
    target_link_options(rvgmstream PRIVATE "-Wl,--exclude-libs,ALL")
  endif()
endif()
if(CMAKE_C_COMPILER_ID MATCHES "GNU|Clang")
  # Upstream's own warning set is noisy on its own sources; they are not ours to fix.
  target_compile_options(rvgmstream PRIVATE -w)
endif()
if(NOT WIN32)
  target_link_libraries(rvgmstream PRIVATE m)
endif()
if(WIN32)
  target_compile_definitions(rvgmstream PRIVATE _CRT_SECURE_NO_WARNINGS)
  # rvgmstream.dll, not librvgmstream.dll: the name src/rom/native_lib.rs looks for on Windows.
  set_target_properties(rvgmstream PROPERTIES PREFIX "")
  if(MINGW)
    target_link_options(rvgmstream PRIVATE -static-libgcc -static)
  endif()
endif()
