# rommusic: the players for music made by a console's own sound chips, in one library.
#
#   gme_*   upstream libgme (external/game-music-emu, LGPL-2.1+): rip sets (NSF, SPC, GBS, VGM,
#           HES, KSS, AY, SAP, GYM). Nuked YM2612, no zlib.
#   rsgc_*  SGC rips (Game Gear, Master System) from kode54's fork (external/gme-kode54, LGPL),
#           which upstream does not play. The fork shares classes with upstream (Blip_Buffer,
#           Sms_Apu, Gme_File...), so its sources are compiled with sgc/rename.h force-included:
#           every name it defines gets an rsgc_ prefix and both copies live side by side.
#   rspc_*  upstream's SNES sound chip (Snes_Spc) driven through its ports, in time.
#   ragb_*  GBA music from the ROM through agbplay (external/agbplay, LGPL-3.0): the playback
#           core only, fmt header-only from external/fmt.
#   rsseq_* DS sequenced music (SSEQ in a sound archive) through kode54's SSEQ Player
#           (external/SSEQPlayer, WTFPL), with deps/rommusic/patches/sseq/ applied.
#   rusf_*  N64 music from USF rips through lazyusf2 (external/lazyusf2, GPL-2.0+): the
#           cached interpreter, no recompiler, so it builds the same everywhere.
#
# Built by deps/rommusic/scripts/build.sh.
cmake_minimum_required(VERSION 3.24)
project(rommusic C CXX)

foreach(root GME_ROOT GME_FORK_ROOT AGBPLAY_ROOT FMT_ROOT SSEQ_ROOT USF_ROOT)
  if(NOT ${root})
    message(FATAL_ERROR "set GME_ROOT, GME_FORK_ROOT, AGBPLAY_ROOT, FMT_ROOT, SSEQ_ROOT and USF_ROOT (see scripts/build.sh)")
  endif()
endforeach()
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

# --- upstream libgme, as a static library linked in whole ------------------------------
set(GME_BUILD_SHARED OFF CACHE BOOL "" FORCE)
set(GME_BUILD_STATIC ON CACHE BOOL "" FORCE)
set(GME_BUILD_FRAMEWORK OFF CACHE BOOL "" FORCE)
set(GME_BUILD_TESTING OFF CACHE BOOL "" FORCE)
set(GME_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
set(GME_ZLIB OFF CACHE BOOL "" FORCE)
set(GME_YM2612_EMU "Nuked" CACHE STRING "" FORCE)
set(BUILD_TESTING OFF CACHE BOOL "" FORCE)
add_subdirectory(${GME_ROOT} gme EXCLUDE_FROM_ALL)
# Export gme_* from the library this is linked into; the more accurate sound CPU timing is
# what the port-driven SNES chip needs (it changes no class layout).
target_compile_definitions(gme_static PRIVATE BLARGG_BUILD_DLL LIBGME_VISIBILITY SPC_MORE_ACCURACY=1)

# --- SGC from the fork, renamed -------------------------------------------------------
set(FORK ${GME_FORK_ROOT}/gme)
add_library(rommusic_sgc OBJECT
  sgc/rsgc.cpp sgc/ym2413_off.cpp
  ${FORK}/gme.cpp ${FORK}/Sgc_Core.cpp ${FORK}/Sgc_Cpu.cpp ${FORK}/Sgc_Emu.cpp ${FORK}/Sgc_Impl.cpp
  ${FORK}/Sms_Apu.cpp ${FORK}/Sms_Fm_Apu.cpp ${FORK}/Z80_Cpu.cpp
  ${FORK}/Classic_Emu.cpp ${FORK}/Music_Emu.cpp ${FORK}/Gme_File.cpp ${FORK}/Gme_Loader.cpp ${FORK}/Rom_Data.cpp
  ${FORK}/M3u_Playlist.cpp ${FORK}/Track_Filter.cpp ${FORK}/Blip_Buffer.cpp ${FORK}/Multi_Buffer.cpp
  ${FORK}/Effects_Buffer.cpp ${FORK}/blargg_common.cpp ${FORK}/blargg_errors.cpp
  # The fork moved Data_Reader to File_Extractor; upstream's copy is the same class.
  ${GME_ROOT}/gme/Data_Reader.cpp)
target_include_directories(rommusic_sgc PRIVATE ${FORK} ${GME_ROOT}/gme)
target_compile_definitions(rommusic_sgc PRIVATE "GME_TYPE_LIST=gme_sgc_type" HAVE_STDINT_H BLARGG_LITTLE_ENDIAN=1)
target_compile_options(rommusic_sgc PRIVATE -include ${CMAKE_CURRENT_SOURCE_DIR}/sgc/rename.h)
set_target_properties(rommusic_sgc PROPERTIES CXX_STANDARD 11 CXX_VISIBILITY_PRESET hidden VISIBILITY_INLINES_HIDDEN ON)
# gme_load_custom passes a long-sized reader callback where upstream's Data_Reader takes an
# int-sized one. rsgc never calls it, so the conversion is allowed rather than patched.
set_source_files_properties(${FORK}/gme.cpp PROPERTIES COMPILE_OPTIONS "-fpermissive;-w")

# --- the SNES sound chip through its ports (Snes_Spc comes from gme_static) -----------
add_library(rommusic_spc OBJECT spc/rspc.cpp)
target_include_directories(rommusic_spc PRIVATE ${GME_ROOT}/gme)
target_compile_definitions(rommusic_spc PRIVATE HAVE_STDINT_H BLARGG_LITTLE_ENDIAN=1 SPC_MORE_ACCURACY=1)
set_target_properties(rommusic_spc PROPERTIES CXX_STANDARD 11 CXX_VISIBILITY_PRESET hidden VISIBILITY_INLINES_HIDDEN ON)

# --- agbplay's playback core ----------------------------------------------------------
set(AGB ${AGBPLAY_ROOT}/src/agbplay)
set(AGB_CORE CGBPatterns Debug LoudnessCalculator MP2KChn MP2KChnPCM MP2KChnPSG MP2KContext MP2KPlayer
  MP2KScanner MP2KTrack Profile ReverbEffect Resampler SequenceReader SoundMixer Types Xcept)
set(AGB_SOURCES agbplay/ragb.cpp agbplay/rom_buffer.cpp)
foreach(f ${AGB_CORE})
  list(APPEND AGB_SOURCES ${AGB}/${f}.cpp)
endforeach()
# The AVX2 resamplers, on x86-64 targets. Asked of the compiler itself: MSYS2's CMake leaves
# CMAKE_SYSTEM_PROCESSOR empty, and Resampler.cpp links them whenever immintrin.h exists.
include(CheckCXXSourceCompiles)
check_cxx_source_compiles("#if !defined(__x86_64__)\n#error not x86-64\n#endif\nint main() { return 0; }" ROMMUSIC_X86_64)
if(ROMMUSIC_X86_64)
  list(APPEND AGB_SOURCES ${AGB}/ResamplerAVX2.cpp)
  set_source_files_properties(${AGB}/ResamplerAVX2.cpp PROPERTIES COMPILE_OPTIONS "-mavx2")
endif()
add_library(rommusic_agb OBJECT ${AGB_SOURCES})
target_include_directories(rommusic_agb PRIVATE ${AGB} ${CMAKE_CURRENT_SOURCE_DIR}/agbplay/compat ${FMT_ROOT}/include)
target_compile_definitions(rommusic_agb PRIVATE FMT_HEADER_ONLY)
set_target_properties(rommusic_agb PROPERTIES CXX_STANDARD 23 CXX_STANDARD_REQUIRED ON CXX_VISIBILITY_PRESET hidden VISIBILITY_INLINES_HIDDEN ON)

# --- the DS sequence player ---------------------------------------------------------
file(GLOB SSEQ_SOURCES ${SSEQ_ROOT}/*.cpp)
add_library(rommusic_sseq OBJECT sseq/rsseq.cpp ${SSEQ_SOURCES})
target_include_directories(rommusic_sseq PRIVATE ${SSEQ_ROOT})
# Its string helpers use std::wstring_convert, deprecated since C++17 but still there.
target_compile_options(rommusic_sseq PRIVATE -Wno-deprecated-declarations)
set_target_properties(rommusic_sseq PROPERTIES CXX_STANDARD 17 CXX_STANDARD_REQUIRED ON CXX_VISIBILITY_PRESET hidden VISIBILITY_INLINES_HIDDEN ON)

# --- the N64 player ------------------------------------------------------------------
set(USF_SOURCES ai/ai_controller api/callbacks debugger/dbg_decoder main/main main/rom main/savestates
  main/util memory/memory pi/cart_rom pi/pi_controller r4300/cached_interp r4300/cp0 r4300/cp1
  r4300/exception r4300/interupt r4300/mi_controller r4300/pure_interp r4300/r4300 r4300/r4300_core
  r4300/recomp r4300/reset r4300/tlb r4300/empty_dynarec rdp/rdp_core ri/rdram ri/rdram_detection_hack
  ri/ri_controller rsp/rsp_core rsp_hle/alist rsp_hle/alist_audio rsp_hle/alist_naudio rsp_hle/alist_nead
  rsp_hle/audio rsp_hle/cicx105 rsp_hle/hle rsp_hle/hvqm rsp_hle/jpeg rsp_hle/memory rsp_hle/mp3
  rsp_hle/musyx rsp_hle/plugin rsp_hle/re2 rsp_lle/rsp si/cic si/game_controller si/n64_cic_nus_6105
  si/pif si/si_controller usf/usf usf/barray usf/resampler vi/vi_controller)
list(TRANSFORM USF_SOURCES PREPEND ${USF_ROOT}/)
list(TRANSFORM USF_SOURCES APPEND .c)
add_library(rommusic_usf OBJECT usf/rusf.c ${USF_SOURCES})
target_include_directories(rommusic_usf PRIVATE ${USF_ROOT})
set_target_properties(rommusic_usf PROPERTIES C_STANDARD 99 C_VISIBILITY_PRESET hidden)
# Upstream's own build is this lax: old mupen64plus code.
target_compile_options(rommusic_usf PRIVATE -w -fno-strict-aliasing)

# --- the library ----------------------------------------------------------------------
add_library(rommusic SHARED $<TARGET_OBJECTS:rommusic_sgc> $<TARGET_OBJECTS:rommusic_spc> $<TARGET_OBJECTS:rommusic_agb> $<TARGET_OBJECTS:rommusic_sseq>
  $<TARGET_OBJECTS:rommusic_usf>)
target_link_libraries(rommusic PRIVATE "$<LINK_LIBRARY:WHOLE_ARCHIVE,gme_static>")
set_target_properties(rommusic PROPERTIES CXX_VISIBILITY_PRESET hidden VISIBILITY_INLINES_HIDDEN ON LINKER_LANGUAGE CXX)
if(WIN32)
  # The MinGW runtime (libstdc++, libgcc, winpthread) linked in, so the DLL imports only
  # system DLLs.
  target_link_options(rommusic PRIVATE -static -static-libgcc -static-libstdc++)
endif()
