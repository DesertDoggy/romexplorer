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
#   rpsx_*  PS1 and PS2 music from PSF/PSF2 rips through Highly Experimental
#           (external/highly_experimental, GPL-3.0). It needs a PS2 BIOS, which the caller
#           hands over from the user's own files; none is bundled.
#   rsega_* Saturn and Dreamcast music from SSF/DSF rips through Highly Theoretical
#           (external/highly_theoretical, GPL-3.0), with its C68K core (GPL-2.0+); the
#           old Musashi copy it also carries is under MAME's licence and is not built.
#   rgsf_*  GBA music from GSF rips through mGBA (external/mgba, MPL-2.0), its GBA core
#           alone as a static library (LIBMGBA_ONLY: no frontends, no optional deps).
#   rwsr_*  WonderSwan music from WSR rips through Beetle WonderSwan
#           (external/beetle-wswan-libretro, Mednafen's core, GPL-2.0+), without its
#           libretro frontend.
#   rvgm_   VGM logs for the chips gme lacks (Neo Geo Pocket T6W28, Virtual Boy VSU)
#           through libvgm (external/libvgm): its VGM player with only the SN76496 (MAME,
#           BSD-3-Clause) and VSU (Mednafen, GPL-2.0+) cores, no zlib or audio output.
#
# Built by deps/rommusic/scripts/build.sh.
cmake_minimum_required(VERSION 3.24)
project(rommusic C CXX)

foreach(root GME_ROOT GME_FORK_ROOT AGBPLAY_ROOT FMT_ROOT SSEQ_ROOT USF_ROOT HE_ROOT HT_ROOT MGBA_ROOT WSWAN_ROOT VGM_ROOT)
  if(NOT ${root})
    message(FATAL_ERROR "set GME_ROOT, GME_FORK_ROOT, AGBPLAY_ROOT, FMT_ROOT, SSEQ_ROOT, USF_ROOT, HE_ROOT, HT_ROOT, MGBA_ROOT, WSWAN_ROOT and VGM_ROOT (see scripts/build.sh)")
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
add_library(rommusic_usf OBJECT usf/rusf.c usf/adler32.c ${USF_SOURCES})
target_include_directories(rommusic_usf PRIVATE ${USF_ROOT})
set_target_properties(rommusic_usf PROPERTIES C_STANDARD 99 C_VISIBILITY_PRESET hidden)
# Upstream's own build is this lax: old mupen64plus code. GCC 14 and later make a `return 0`
# in a void function (rsp_hle/plugin.c) an error, which -fpermissive turns back into a warning.
target_compile_options(rommusic_usf PRIVATE -w -fno-strict-aliasing -fpermissive)

# --- Highly Experimental: the PS1/PS2 sound CPU and SPU -----------------------------
set(HE_SOURCES psx ioptimer iop bios r3000dis r3000asm r3000 vfs spucore spu mkhebios)
list(TRANSFORM HE_SOURCES PREPEND ${HE_ROOT}/Core/)
list(TRANSFORM HE_SOURCES APPEND .c)
add_library(rommusic_psx OBJECT psx/rpsx.c ${HE_SOURCES})
target_include_directories(rommusic_psx PRIVATE ${HE_ROOT}/Core)
target_compile_definitions(rommusic_psx PRIVATE EMU_COMPILE EMU_LITTLE_ENDIAN HAVE_STDINT_H)
set_target_properties(rommusic_psx PROPERTIES C_STANDARD 99 C_VISIBILITY_PRESET hidden)
# Old C that GCC 14 and later reject by default; -fpermissive keeps it a warning.
target_compile_options(rommusic_psx PRIVATE -w -fno-strict-aliasing -fpermissive)

# --- Highly Theoretical: the Saturn SCSP and Dreamcast AICA, with their sound CPUs -----
set(HT ${HT_ROOT}/Core)
add_library(rommusic_sega OBJECT sega/rsega.c
  ${HT}/sega.c ${HT}/dcsound.c ${HT}/satsound.c ${HT}/yam.c ${HT}/arm.c ${HT}/c68k/c68k.c ${HT}/c68k/c68kexec.c)
target_include_directories(rommusic_sega PRIVATE ${HT})
# Its op files are generated as switch cases, so the switch build it is.
target_compile_definitions(rommusic_sega PRIVATE EMU_COMPILE EMU_LITTLE_ENDIAN HAVE_STDINT_H C68K_NO_JUMP_TABLE)
set_target_properties(rommusic_sega PROPERTIES C_STANDARD 99 C_VISIBILITY_PRESET hidden)
target_compile_options(rommusic_sega PRIVATE -w -fno-strict-aliasing -fpermissive)

# --- mGBA's GBA core, for GSF rips ----------------------------------------------------
set(LIBMGBA_ONLY ON CACHE BOOL "" FORCE)
set(M_CORE_GB OFF CACHE BOOL "" FORCE)
set(M_CORE_GBA ON CACHE BOOL "" FORCE)
add_subdirectory(${MGBA_ROOT} mgba EXCLUDE_FROM_ALL)
add_library(rommusic_gsf OBJECT gsf/rgsf.c)
target_include_directories(rommusic_gsf PRIVATE ${MGBA_ROOT}/include ${CMAKE_CURRENT_BINARY_DIR}/mgba/include)
# Compiled with the library's own feature defines: they decide the layout of its structs, and
# its generated flags.h does not carry all of them.
get_directory_property(MGBA_DIR_DEFINES DIRECTORY ${MGBA_ROOT} COMPILE_DEFINITIONS)
target_compile_definitions(rommusic_gsf PRIVATE ${MGBA_DIR_DEFINES} $<TARGET_PROPERTY:mgba,COMPILE_DEFINITIONS>)
set_target_properties(rommusic_gsf PROPERTIES C_STANDARD 11 C_VISIBILITY_PRESET hidden)

# --- Beetle WonderSwan, for WSR rips -----------------------------------------------------
set(WSW ${WSWAN_ROOT}/mednafen)
add_library(rommusic_wsr OBJECT wsr/rwsr.c
  ${WSW}/wswan/sound.c ${WSW}/wswan/interrupt.c ${WSW}/wswan/comm.c ${WSW}/wswan/rtc.c ${WSW}/wswan/tcache.c
  ${WSW}/wswan/gfx.c ${WSW}/wswan/wswan-memory.c ${WSW}/wswan/v30mz.c ${WSW}/wswan/eeprom.c
  ${WSW}/mempatcher.c ${WSW}/settings.c ${WSW}/state.c ${WSW}/sound/Blip_Buffer.c
  ${WSWAN_ROOT}/libretro-common/compat/compat_strl.c)
target_include_directories(rommusic_wsr PRIVATE ${WSWAN_ROOT} ${WSW} ${WSW}/include ${WSWAN_ROOT}/libretro-common/include)
set_target_properties(rommusic_wsr PROPERTIES C_STANDARD 99 C_VISIBILITY_PRESET hidden)
# The defines its own Makefile passes.
target_compile_definitions(rommusic_wsr PRIVATE SIZEOF_DOUBLE=8 MEDNAFEN_VERSION_NUMERIC=931 MPC_FIXED_POINT STDC_HEADERS __STDC_LIMIT_MACROS __LIBRETRO__ _LOW_ACCURACY_)
target_compile_options(rommusic_wsr PRIVATE -w -fno-strict-aliasing -fpermissive)

# --- libvgm, for VGM logs gme cannot play -------------------------------------------------
# Its own CMake is not used: it would want zlib for loaders and a player list rommusic does
# not need. SNDDEV_SELECT builds the device table from only the SNDDEV_ named here.
set(LV ${VGM_ROOT})
set(RVGM_SOURCES vgm/rvgm.cpp
  ${LV}/emu/SoundEmu.c ${LV}/emu/Resampler.c ${LV}/emu/logging.c ${LV}/emu/panning.c ${LV}/emu/dac_control.c
  ${LV}/emu/cores/sn764intf.c ${LV}/emu/cores/sn76496.c ${LV}/emu/cores/vsu.c
  ${LV}/player/playerbase.cpp ${LV}/player/vgmplayer.cpp ${LV}/player/vgmplayer_cmdhandler.cpp ${LV}/player/playera.cpp
  ${LV}/player/dblk_compr.c ${LV}/player/helper.c ${LV}/utils/DataLoader.c)
if(WIN32)
  list(APPEND RVGM_SOURCES ${LV}/utils/StrUtils-CPConv_Win.c)
else()
  list(APPEND RVGM_SOURCES ${LV}/utils/StrUtils-CPConv_IConv.c)
endif()
add_library(rommusic_vgm OBJECT ${RVGM_SOURCES})
target_include_directories(rommusic_vgm PRIVATE ${LV} ${LV}/libs/include)
target_compile_definitions(rommusic_vgm PRIVATE SNDDEV_SELECT SNDDEV_SN76496 EC_SN76496_MAME SNDDEV_VBOY_VSU VGM_LITTLE_ENDIAN)
set_target_properties(rommusic_vgm PROPERTIES C_STANDARD 99 CXX_STANDARD 11 C_VISIBILITY_PRESET hidden CXX_VISIBILITY_PRESET hidden)
target_compile_options(rommusic_vgm PRIVATE -w)

# --- the library ----------------------------------------------------------------------
add_library(rommusic SHARED $<TARGET_OBJECTS:rommusic_sgc> $<TARGET_OBJECTS:rommusic_spc> $<TARGET_OBJECTS:rommusic_agb> $<TARGET_OBJECTS:rommusic_sseq>
  $<TARGET_OBJECTS:rommusic_usf> $<TARGET_OBJECTS:rommusic_psx> $<TARGET_OBJECTS:rommusic_sega> $<TARGET_OBJECTS:rommusic_gsf> $<TARGET_OBJECTS:rommusic_wsr> $<TARGET_OBJECTS:rommusic_vgm>)
target_link_libraries(rommusic PRIVATE "$<LINK_LIBRARY:WHOLE_ARCHIVE,gme_static>" mgba)
if(APPLE)
  target_link_libraries(rommusic PRIVATE iconv)
endif()
set_target_properties(rommusic PROPERTIES CXX_VISIBILITY_PRESET hidden VISIBILITY_INLINES_HIDDEN ON LINKER_LANGUAGE CXX)
if(NOT WIN32 AND NOT APPLE)
  # A symbol nothing defines fails the link here, as it does on Windows, not at load time.
  target_link_options(rommusic PRIVATE -Wl,--no-undefined)
endif()
if(NOT APPLE)
  # mGBA's functions are its own business: only the rgsf_ calls are exported.
  target_link_options(rommusic PRIVATE -Wl,--exclude-libs,libmgba.a)
endif()
if(WIN32)
  # The MinGW runtime (libstdc++, libgcc, winpthread) linked in, so the DLL imports only
  # system DLLs.
  target_link_options(rommusic PRIVATE -static -static-libgcc -static-libstdc++)
endif()
