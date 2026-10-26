// SPDX-License-Identifier: GPL-2.0-only
//
// The APCA implementation against the published reference values, and the
// colour search the page relies on.

#include "check.hpp"

#include "apca.hpp"

#include <cmath>

using namespace hyperflux::openrgb;

namespace
{

Rgb hex(unsigned value)
{
    return {static_cast<double>(value >> 16 & 0xff), static_cast<double>(value >> 8 & 0xff),
            static_cast<double>(value & 0xff)};
}

bool near(double actual, double expected, double tolerance)
{
    const bool ok = std::abs(actual - expected) <= tolerance;
    if(!ok)
        std::cerr << "got " << actual << ", expected " << expected << "\n";
    return ok;
}

void reference_values()
{
    // From "Testing your implementation" in Myndex/SAPC-APCA
    // (documentation/README.md), text first.
    CHECK(near(apca_contrast(hex(0x888888), hex(0xffffff)), 63.056469930209424, 1e-9));
    CHECK(near(apca_contrast(hex(0xffffff), hex(0x888888)), -68.54146436644962, 1e-9));
    CHECK(near(apca_contrast(hex(0x000000), hex(0xaaaaaa)), 58.146262578561334, 1e-9));
    CHECK(near(apca_contrast(hex(0xaaaaaa), hex(0x000000)), -56.24113336839742, 1e-9));
    CHECK(near(apca_contrast(hex(0x112233), hex(0xddeeff)), 91.66830811481631, 1e-9));
    CHECK(near(apca_contrast(hex(0xddeeff), hex(0x112233)), -93.06770049484275, 1e-9));
    CHECK(near(apca_contrast(hex(0x112233), hex(0x444444)), 8.32326136957393, 1e-9));
    CHECK(near(apca_contrast(hex(0x444444), hex(0x112233)), -7.526878460278154, 1e-9));
    // The calculator at apcacontrast.com shows these to two places.
    CHECK(near(apca_contrast(hex(0x000000), hex(0xffffff)), 106.04, 0.005));
    CHECK(near(apca_contrast(hex(0xffffff), hex(0x000000)), -107.88, 0.005));
    // Too close to call is exactly zero, either way round.
    CHECK(apca_contrast(hex(0x777777), hex(0x7a7a7a)) == 0);
    CHECK(apca_contrast(hex(0x7a7a7a), hex(0x777777)) == 0);
}

void reach_stops_at_the_target()
{
    const Rgb white = hex(0xffffff);
    const Rgb black = hex(0x000000);
    for(const Rgb background : {hex(0xefefef), hex(0xfcfcfc), hex(0x2a2e32), hex(0x1e1e1e)})
    {
        const Rgb toward = std::abs(apca_contrast(white, background)) > std::abs(apca_contrast(black, background))
                               ? white
                               : black;
        const Rgb red = reach(hex(0xd03030), toward, background, lc::indicator);
        CHECK(std::abs(apca_contrast(red, background)) >= lc::indicator);
        // Faded from the background toward the text: just past the target,
        // not all the way back to the text.
        const Rgb text = toward;
        const Rgb muted = reach(background, text, background, lc::secondary);
        const double contrast = std::abs(apca_contrast(muted, background));
        CHECK(contrast >= lc::secondary && contrast < lc::secondary + 1);
    }
    // A colour that already passes is left alone.
    const Rgb ink = hex(0x232629);
    const Rgb same = reach(ink, hex(0x000000), hex(0xeff0f1), lc::body);
    CHECK(same.r == ink.r && same.g == ink.g && same.b == ink.b);
    // Out of reach: the best there is.
    const Rgb best = reach(hex(0x808080), hex(0x000000), hex(0x777777), lc::body);
    CHECK(best.r == 0 && best.g == 0 && best.b == 0);
}

} // namespace

int main()
{
    reference_values();
    reach_stops_at_the_target();
    return finish("apca");
}
