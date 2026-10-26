// SPDX-License-Identifier: GPL-2.0-only
//
// APCA-W3 0.0.98G-4g by Andrew Somers (Myndex), written here from the
// formula and constants published in
// https://github.com/Myndex/SAPC-APCA/blob/master/documentation/APCA-W3-LaTeX.md
// rather than taken from the reference code, whose licence doesn't fit
// GPL-2.0-only. tests/test_apca.cpp checks it against the values listed
// under "Testing your implementation" in the same repository.

#include "apca.hpp"

#include <cmath>

namespace hyperflux::openrgb
{
namespace
{

// Screen luminance: a plain 2.4 power per channel, not the piecewise sRGB
// curve, with the sRGB coefficients.
double luminance(Rgb c)
{
    const auto channel = [](double v) { return std::pow(v / 255.0, 2.4); };
    const double y = 0.2126729 * channel(c.r) + 0.7151522 * channel(c.g) + 0.0721750 * channel(c.b);
    // Soft clamp near black, where the plain power law overstates contrast.
    constexpr double threshold = 0.022;
    return y < threshold ? y + std::pow(threshold - y, 1.414) : y;
}

} // namespace

double apca_contrast(Rgb text, Rgb background)
{
    const double t = luminance(text);
    const double b = luminance(background);
    constexpr double scale = 1.14;
    constexpr double offset = 0.027;
    constexpr double clip = 0.1;
    if(b > t)
    {
        const double s = (std::pow(b, 0.56) - std::pow(t, 0.57)) * scale;
        return s < clip ? 0 : (s - offset) * 100;
    }
    const double s = (std::pow(b, 0.65) - std::pow(t, 0.62)) * scale;
    return s > -clip ? 0 : (s + offset) * 100;
}

Rgb mix(Rgb from, Rgb to, double amount)
{
    return {from.r + (to.r - from.r) * amount, from.g + (to.g - from.g) * amount, from.b + (to.b - from.b) * amount};
}

Rgb reach(Rgb colour, Rgb toward, Rgb background, double target)
{
    // Whole channel values, as the colour will be drawn: rounding can't
    // take it back under the target.
    const auto at = [&](double amount)
    {
        const Rgb c = mix(colour, toward, amount);
        return Rgb {std::round(c.r), std::round(c.g), std::round(c.b)};
    };
    const auto enough = [&](double amount) { return std::abs(apca_contrast(at(amount), background)) >= target; };
    if(enough(0))
        return colour;
    if(!enough(1))
        return toward;
    // Once the colour has passed the background's lightness, contrast only
    // grows along the way, so halving finds the least change. The 16 steps
    // are finer than one 8-bit level.
    double low = 0;
    double high = 1;
    for(int i = 0; i < 16; ++i)
    {
        const double middle = (low + high) / 2;
        (enough(middle) ? high : low) = middle;
    }
    return at(high);
}

} // namespace hyperflux::openrgb
