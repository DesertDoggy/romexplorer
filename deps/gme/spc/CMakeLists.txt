# gmespc: upstream libgme's SNES sound chip emulator (Snes_Spc) behind the rspc_ functions in
# rspc.cpp, for driving it through its ports in time (libgme's own API only plays .spc files).
# Built by deps/gme/scripts/build.sh next to libgme.
cmake_minimum_required(VERSION 3.16)
project(gmespc CXX)

if(NOT GME_ROOT)
  message(FATAL_ERROR "set GME_ROOT to external/game-music-emu")
endif()
set(SRC ${GME_ROOT}/gme)
add_library(gmespc SHARED rspc.cpp ${SRC}/Snes_Spc.cpp ${SRC}/Spc_Cpu.cpp ${SRC}/Spc_Dsp.cpp)
target_include_directories(gmespc PRIVATE ${SRC})
target_compile_definitions(gmespc PRIVATE HAVE_STDINT_H BLARGG_LITTLE_ENDIAN=1 SPC_MORE_ACCURACY=1)
set_target_properties(gmespc PROPERTIES CXX_VISIBILITY_PRESET hidden VISIBILITY_INLINES_HIDDEN ON)
if(WIN32)
  target_link_options(gmespc PRIVATE -static -static-libgcc -static-libstdc++)
endif()
