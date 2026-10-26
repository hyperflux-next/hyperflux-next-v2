// SPDX-License-Identifier: GPL-2.0-only
//
// Contrast as APCA-W3 measures it, for the page to check and pick its
// colours against whatever background the theme or skin paints.

#pragma once

namespace hyperflux::openrgb
{

// sRGB channels, 0 to 255.
struct Rgb
{
    double r = 0;
    double g = 0;
    double b = 0;
};

// Lc of `text` on `background`: positive for dark text on a light
// background, negative the other way round, about -108 to 106.
[[nodiscard]] double apca_contrast(Rgb text, Rgb background);

// What the page asks of each kind of colour, as |Lc| (APCA bronze level).
namespace lc
{
constexpr double body = 75;      // text people read
constexpr double secondary = 60; // metadata, links, short notes
constexpr double indicator = 45; // the fill of a battery level
constexpr double minimum = 30;   // nothing that means something goes below this
constexpr double edge = 15;      // a card's edge, a level's empty track
} // namespace lc

[[nodiscard]] Rgb mix(Rgb from, Rgb to, double amount);

// `colour` moved toward `toward` (black or white, usually) just far enough
// to reach `target` |Lc| on `background`. If even `toward` itself falls
// short, that is the best there is and it is returned.
[[nodiscard]] Rgb reach(Rgb colour, Rgb toward, Rgb background, double target);

} // namespace hyperflux::openrgb
