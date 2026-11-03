# Builds exiso: extract-xiso's XISO rewrite as one shared library (see exiso.h). Its own
# project; extract-xiso.c is compiled as it is, nothing upstream is edited.
cmake_minimum_required(VERSION 3.16)
project(exiso C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(CMAKE_C_VISIBILITY_PRESET hidden)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

get_filename_component(_ext "${CMAKE_CURRENT_SOURCE_DIR}/../../external" ABSOLUTE)
set(EXISO_ROOT "${_ext}/extract-xiso" CACHE PATH "extract-xiso source tree")
if(NOT EXISTS "${EXISO_ROOT}/extract-xiso.c")
  message(FATAL_ERROR "missing ${EXISO_ROOT}/extract-xiso.c -- git submodule update --init external/extract-xiso")
endif()

add_library(exiso SHARED "${CMAKE_CURRENT_SOURCE_DIR}/exiso.c")
set_target_properties(exiso PROPERTIES PREFIX "lib")
if(WIN32)
  set_target_properties(exiso PROPERTIES PREFIX "")
endif()
target_include_directories(exiso PRIVATE "${EXISO_ROOT}" "${CMAKE_CURRENT_SOURCE_DIR}")
# extract-xiso picks its platform from these, as its own CMakeLists does (__LINUX__, ...).
string(TOUPPER "__${CMAKE_SYSTEM_NAME}__" _target_os)
# Android is Linux to it: bionic has the off64_t / lseek64 / stat64 its Linux branch uses.
if(ANDROID)
  set(_target_os "__LINUX__")
endif()
target_compile_definitions(exiso PRIVATE ${_target_os})
if(NOT MSVC)
  # Its K&R-era style: unused results, sign mismatches. Not ours to fix.
  target_compile_options(exiso PRIVATE -w)
endif()
if(CMAKE_SYSTEM_NAME MATCHES "Linux|Android")
  target_link_options(exiso PRIVATE -Wl,--no-undefined)
  # Android's libc has pthreads built in; there is no libpthread to link.
  if(NOT ANDROID)
    target_link_libraries(exiso PRIVATE pthread)
  endif()
endif()
if(MINGW)
  target_link_options(exiso PRIVATE -static -static-libgcc)
endif()
if(APPLE)
  set_target_properties(exiso PROPERTIES INSTALL_NAME_DIR "@rpath")
endif()
