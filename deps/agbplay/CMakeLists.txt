# ragb: GBA music from the ROM through agbplay (external/agbplay, LGPL-3.0), behind the ragb_
# functions in shim/ragb.cpp. Only the playback core is compiled: no GUI, settings, profiles on
# disk or export, so no libzip, zlib, libsndfile or JSON. fmt is header-only from external/fmt,
# and the one Boost function used comes from compat/. Built by deps/agbplay/scripts/build.sh.
cmake_minimum_required(VERSION 3.16)
project(ragb CXX)

if(NOT AGBPLAY_ROOT OR NOT FMT_ROOT)
  message(FATAL_ERROR "set AGBPLAY_ROOT to external/agbplay and FMT_ROOT to external/fmt")
endif()
set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(SRC ${AGBPLAY_ROOT}/src/agbplay)

set(CORE CGBPatterns Debug LoudnessCalculator MP2KChn MP2KChnPCM MP2KChnPSG MP2KContext MP2KPlayer
  MP2KScanner MP2KTrack Profile ReverbEffect Resampler SequenceReader SoundMixer Types Xcept)
set(SOURCES shim/ragb.cpp shim/rom_buffer.cpp)
foreach(f ${CORE})
  list(APPEND SOURCES ${SRC}/${f}.cpp)
endforeach()
# The AVX2 resamplers, on x86-64 targets. Asked of the compiler itself: MSYS2's CMake leaves
# CMAKE_SYSTEM_PROCESSOR empty, and Resampler.cpp links them whenever immintrin.h exists.
include(CheckCXXSourceCompiles)
check_cxx_source_compiles("#if !defined(__x86_64__)\n#error not x86-64\n#endif\nint main() { return 0; }" RAGB_X86_64)
if(RAGB_X86_64)
  list(APPEND SOURCES ${SRC}/ResamplerAVX2.cpp)
  set_source_files_properties(${SRC}/ResamplerAVX2.cpp PROPERTIES COMPILE_OPTIONS "-mavx2")
endif()

add_library(ragb SHARED ${SOURCES})
target_include_directories(ragb PRIVATE ${SRC} ${CMAKE_CURRENT_SOURCE_DIR}/compat ${FMT_ROOT}/include)
target_compile_definitions(ragb PRIVATE FMT_HEADER_ONLY)
set_target_properties(ragb PROPERTIES CXX_VISIBILITY_PRESET hidden VISIBILITY_INLINES_HIDDEN ON)
if(WIN32)
  target_link_options(ragb PRIVATE -static -static-libgcc -static-libstdc++)
endif()
