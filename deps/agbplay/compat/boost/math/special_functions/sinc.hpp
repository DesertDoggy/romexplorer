// Stand-in for the one Boost function agbplay's resampler uses, so the library builds without
// Boost: sinc(x) = sin(x) / x, with its limit 1 near zero.
#pragma once
#include <cmath>

namespace boost::math {
template<typename T> T sinc_pi(T x)
{
    if (std::abs(x) < T(1e-8))
        return T(1) - x * x / T(6);
    return std::sin(x) / x;
}
}    // namespace boost::math
