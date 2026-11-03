// Stand-in for Cemu's src/Common/precompiled.h, for the few Cemu sources cemuwua compiles
// (the WUD/WUX reader, the FST/partition decryption and the ticket/TMD parser).
//
// Cemu's own header pulls in boost, glm, the emulator's memory and CPU state and its whole
// logging system. These files need only the basic types, the big-endian wrappers
// (Common/betype.h, used as is), fmt, and the assert/log hooks, which are no-ops here: the
// library reports failure through return values, never by logging or aborting.
//
// Force-included into every Cemu source (-include / /FI), as Cemu itself does with its
// precompiled header: some of them use these types before including anything.
#pragma once

#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <climits>
#include <cassert>
#include <string>
#include <string_view>
#include <vector>
#include <array>
#include <memory>
#include <optional>
#include <algorithm>
#include <functional>
#include <unordered_map>
#include <map>
#include <mutex>
#include <atomic>
#include <filesystem>
#include <span>
#include <ranges>
#include <type_traits>
#include <utility>
#include <fstream>
#include <sstream>
#include <iostream>
#include <chrono>
#include <thread>
#include <bit>
#if defined(ARCH_X86_64)
#include <immintrin.h> // aes128.cpp's AES-NI path, as in Cemu's own header
#endif

#include <fmt/core.h>
#include <fmt/format.h>

namespace fs = std::filesystem;

// boost/predef's OS macros, which Cemu's sources test (cpu_features.cpp, FileStream).
#if defined(__APPLE__)
#define BOOST_OS_MACOS 1
#else
#define BOOST_OS_MACOS 0
#endif
#if defined(_WIN32)
#define BOOST_OS_WINDOWS 1
#else
#define BOOST_OS_WINDOWS 0
#endif
#if defined(__linux__)
#define BOOST_OS_LINUX 1
#else
#define BOOST_OS_LINUX 0
#endif
#define BOOST_OS_BSD 0
#if !defined(_WIN32)
#define BOOST_OS_UNIX 1
#else
#define BOOST_OS_UNIX 0
#endif

using uint64 = uint64_t;
using uint32 = uint32_t;
using uint16 = uint16_t;
using uint8 = uint8_t;
using sint64 = int64_t;
using sint32 = int32_t;
using sint16 = int16_t;
using sint8 = int8_t;
using float32 = float;
using float64 = double;

using uint64le = uint64_t;
using uint32le = uint32_t;
using uint16le = uint16_t;
using uint8le = uint8_t;

inline uint64 _swapEndianU64(uint64 v) { return __builtin_bswap64(v); }
inline uint32 _swapEndianU32(uint32 v) { return __builtin_bswap32(v); }
inline sint32 _swapEndianS32(sint32 v) { return (sint32)__builtin_bswap32((uint32)v); }
inline uint16 _swapEndianU16(uint16 v) { return (uint16)((v >> 8) | (v << 8)); }
inline sint16 _swapEndianS16(sint16 v) { return (sint16)(((uint16)v >> 8) | ((uint16)v << 8)); }

#include "Common/betype.h"

template <typename T1, typename T2>
constexpr bool HAS_FLAG(T1 flags, T2 test_flag) { return (flags & (T1)test_flag) == (T1)test_flag; }

#define DEFINE_ENUM_FLAG_OPERATORS(T)                                                                                    \
    inline T operator~(T a) { return static_cast<T>(~static_cast<std::underlying_type_t<T>>(a)); }                       \
    inline T operator|(T a, T b) { return static_cast<T>(static_cast<std::underlying_type_t<T>>(a) | static_cast<std::underlying_type_t<T>>(b)); } \
    inline T operator&(T a, T b) { return static_cast<T>(static_cast<std::underlying_type_t<T>>(a) & static_cast<std::underlying_type_t<T>>(b)); } \
    inline T operator^(T a, T b) { return static_cast<T>(static_cast<std::underlying_type_t<T>>(a) ^ static_cast<std::underlying_type_t<T>>(b)); } \
    inline T& operator|=(T& a, T b) { return a = a | b; }                                                                \
    inline T& operator&=(T& a, T b) { return a = a & b; }                                                                \
    inline T& operator^=(T& a, T b) { return a = a ^ b; }

inline void cemu_assert(bool) {}
inline void cemu_assert_debug(bool) {}
inline void cemu_assert_unimplemented() {}
inline void cemu_assert_suspicious() {}
inline void cemu_assert_error() {}

#define UNREACHABLE __builtin_unreachable()
#define assert_dbg() ((void)0)

inline std::string _pathToUtf8(const fs::path& path)
{
	std::u8string strU8 = path.generic_u8string();
	return std::string((const char*)strU8.data(), strU8.size());
}

inline fs::path _utf8ToPath(std::string_view input)
{
	std::basic_string_view<char8_t> v((const char8_t*)input.data(), input.size());
	return fs::path(v);
}

#include <boost/algorithm/string.hpp>
#ifdef _WIN32
#include <boost/nowide/convert.hpp>
#endif

// Logging: Cemu's call sites format their message with fmt; here it goes nowhere.
enum class LogType : sint32 { Force = 0, Placeholder = 1 };
template<typename... TArgs>
inline bool cemuLog_log(LogType, TArgs&&...) { return false; }
template<typename... TArgs>
inline bool cemuLog_logDebug(LogType, TArgs&&...) { return false; }
#define cemuLog_logOnce(...) ((void)0)
#define cemuLog_logDebugOnce(...) ((void)0)

inline std::string _tr(std::string_view text) { return std::string{text}; }
#define TR_NOOP(str) str
