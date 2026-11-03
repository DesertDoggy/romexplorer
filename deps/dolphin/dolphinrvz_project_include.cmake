# Passed to CMake as -DCMAKE_PROJECT_INCLUDE=<this file> (see scripts/build_dolphin_rvz.sh).
# CMake includes this immediately after dolphin/CMakeLists.txt's own top-level project()
# call -- i.e. dolphin is configured as the real top-level project (CMAKE_SOURCE_DIR stays
# dolphin/, exactly as dolphin's own CMake code assumes throughout -- e.g. Source/Core/Core/
# CMakeLists.txt's AchievementApprovedHash.h rule references ${CMAKE_SOURCE_DIR}/Data/Sys
# directly). This never edits dolphin/CMakeLists.txt itself.
#
# Two things happen here:
#
# 1. Frontends/features orthogonal to DiscIO conversion are turned off. This must happen
#    now, before dolphin's own option() calls run later in the file (option() only sets a
#    value if the variable isn't already in the cache -- setting it here first wins).
#    Disabling these doesn't change what discio/core/uicommon themselves build with, it
#    just skips the Qt GUI, the test suite, and a few unrelated integrations dolphinrvz
#    never touches (Discord Rich Presence is skipped at runtime too -- see dolphinrvz.cpp's
#    EnsureMinimalInit -- turning it off here also drops the discord-rpc dependency itself).
#
# 2. Adding the dolphinrvz target is deferred to the END of dolphin/CMakeLists.txt's
#    processing (cmake_language(DEFER), CMake 3.25+, which dolphin already requires) --
#    discio/uicommon don't exist as targets yet at this point (dolphin's own
#    add_subdirectory(Source) hasn't run), so dolphinrvz_target.cmake, which links against
#    them, can't run until dolphin's whole top-level file has finished.
#
# CMAKE_PROJECT_INCLUDE is re-included after EVERY project() call anywhere in the whole
# build, not just dolphin's own top-level one -- several of the third-party Externals/
# dolphin pulls in via add_subdirectory (fmt, pugixml, enet, minizip-ng, cubeb, ...) have
# their own project() calls, so without this guard the block below would run once per
# nested library too, registering the deferred include() (and therefore add_library())
# multiple times. PROJECT_IS_TOP_LEVEL (CMake 3.21+) is only true for dolphin's own
# project() call.
if(PROJECT_IS_TOP_LEVEL)

set(ENABLE_QT OFF CACHE BOOL "" FORCE)
set(ENABLE_NOGUI OFF CACHE BOOL "" FORCE)
set(ENABLE_TESTS OFF CACHE BOOL "" FORCE)
set(USE_DISCORD_PRESENCE OFF CACHE BOOL "" FORCE)
set(ENABLE_AUTOUPDATE OFF CACHE BOOL "" FORCE)
set(ENABLE_ANALYTICS OFF CACHE BOOL "" FORCE)
set(USE_RETRO_ACHIEVEMENTS OFF CACHE BOOL "" FORCE)
set(USE_MGBA OFF CACHE BOOL "" FORCE)
set(DSPTOOL OFF CACHE BOOL "" FORCE)
# Controller input (evdev) and the hardware database are the only users of libudev, which
# CMake otherwise finds REQUIRED on Linux; dolphinrvz reads discs and touches neither. Off,
# so a Linux build (and an arm64 cross build, which would need libudev-dev:arm64) does not
# need udev development headers at all.
set(ENABLE_EVDEV OFF CACHE BOOL "" FORCE)
set(ENABLE_HWDB OFF CACHE BOOL "" FORCE)

# NOTE: CMAKE_CURRENT_LIST_DIR is NOT used here -- CMAKE_PROJECT_INCLUDE is spliced in by
# a mechanism that leaves it pointing at dolphin/ (the top-level project's directory)
# rather than updating to this file's own directory the way a normal include() would.
# DOLPHINRVZ_USER_DIR is passed in explicitly via -D by scripts/build_dolphin_rvz.sh.
if(NOT DEFINED DOLPHINRVZ_USER_DIR)
  message(FATAL_ERROR "DOLPHINRVZ_USER_DIR not set -- run via scripts/build_dolphin_rvz.sh")
endif()

cmake_language(DEFER DIRECTORY "${CMAKE_SOURCE_DIR}" CALL include "${DOLPHINRVZ_USER_DIR}/dolphinrvz_target.cmake")

endif()
