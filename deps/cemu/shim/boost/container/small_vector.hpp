// cemuwua stand-in for boost::container::small_vector (Cemu's FST path parser). A plain
// std::vector behaves the same; it only gives up the inline-storage optimisation.
#pragma once
#include <vector>
#include <cstddef>
namespace boost { namespace container {
template<class T, std::size_t N>
using small_vector = std::vector<T>;
} }
