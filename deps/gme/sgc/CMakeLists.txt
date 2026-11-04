# gmesgc: SGC playback from kode54's Game_Music_Emu fork (external/gme-kode54), behind the
# rsgc_ functions in rsgc.cpp. Only what SGC needs is compiled; the YM2413 is the silent
# stand-in in ym2413_off.cpp. Built by deps/gme/scripts/build.sh next to upstream libgme.
cmake_minimum_required(VERSION 3.16)
project(gmesgc CXX)

if(NOT GME_FORK_ROOT)
  message(FATAL_ERROR "set GME_FORK_ROOT to external/gme-kode54")
endif()
set(SRC ${GME_FORK_ROOT}/gme)
# The fork moved Data_Reader to File_Extractor; upstream libgme's copy is the same class.
if(NOT DATA_READER_DIR)
  message(FATAL_ERROR "set DATA_READER_DIR to external/game-music-emu/gme")
endif()

add_library(gmesgc SHARED
  rsgc.cpp ym2413_off.cpp
  ${SRC}/gme.cpp ${SRC}/Sgc_Core.cpp ${SRC}/Sgc_Cpu.cpp ${SRC}/Sgc_Emu.cpp ${SRC}/Sgc_Impl.cpp
  ${SRC}/Sms_Apu.cpp ${SRC}/Sms_Fm_Apu.cpp ${SRC}/Z80_Cpu.cpp
  ${SRC}/Classic_Emu.cpp ${SRC}/Music_Emu.cpp ${SRC}/Gme_File.cpp ${SRC}/Gme_Loader.cpp ${SRC}/Rom_Data.cpp
  ${SRC}/M3u_Playlist.cpp ${SRC}/Track_Filter.cpp ${SRC}/Blip_Buffer.cpp ${SRC}/Multi_Buffer.cpp
  ${SRC}/Effects_Buffer.cpp ${SRC}/blargg_common.cpp ${SRC}/blargg_errors.cpp
  ${DATA_READER_DIR}/Data_Reader.cpp)
target_include_directories(gmesgc PRIVATE ${SRC} ${DATA_READER_DIR})
target_compile_definitions(gmesgc PRIVATE "GME_TYPE_LIST=gme_sgc_type" HAVE_STDINT_H BLARGG_LITTLE_ENDIAN=1)
set_target_properties(gmesgc PROPERTIES CXX_VISIBILITY_PRESET hidden VISIBILITY_INLINES_HIDDEN ON)
# gme_load_custom passes a long-sized reader callback where upstream's Data_Reader takes an
# int-sized one. rsgc never calls it, so the conversion is allowed rather than patched.
set_source_files_properties(${SRC}/gme.cpp PROPERTIES COMPILE_OPTIONS "-fpermissive;-w")
if(WIN32)
  target_link_options(gmesgc PRIVATE -static -static-libgcc -static-libstdc++)
endif()
