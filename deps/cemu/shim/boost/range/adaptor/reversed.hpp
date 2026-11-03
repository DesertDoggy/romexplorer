// cemuwua stand-in for boost::adaptors::reversed (`range | boost::adaptors::reversed`).
#pragma once
#include <ranges>
namespace boost { namespace adaptors {
inline constexpr auto reversed = std::views::reverse;
} }
