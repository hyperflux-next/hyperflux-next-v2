# OpenRGB plugin

This plugin brings the Razer devices on a HyperFlux V2 receiver into
OpenRGB, where OpenRGB's own Razer code drives them. It also adds a
HyperFlux Next V2 page with their batteries to OpenRGB's Information
list.

## Building

    make openrgb-plugin                 # OpenRGB 1.1 with Qt 6
    make openrgb-plugin OPENRGB_QT=5    # OpenRGB 1.0 with Qt 5
    make openrgb-test

A plugin loads only in the OpenRGB release whose headers it was compiled
with. Build for the one your distribution ships. On Ubuntu and Fedora,
build for OpenRGB 1.1 from openrgb.org instead, whose packages use the
system's own Qt 6. The Flatpak and the AppImage bring an older Qt than the
distributions do. They can't load a plugin built there.

The make targets first run `fetch.sh`, which downloads the release pinned
in `PIN`, checks its SHA-256, applies `patches/` and leaves the tree in
`.upstreams/`. Apart from that tree, the build needs CMake, a C++20
compiler, and Qt Core, Gui and Widgets. It doesn't need hidapi.

`make openrgb-load-test` runs `tests/headless-load.sh PLUGIN [OPENRGB]`.
The script checks that OpenRGB loads the plugin, with no display, no
hardware detection and a throwaway configuration. For the run, OpenRGB's
system plugin directory is hidden behind an empty one. An installed copy
can't stand in for the build that way.

## How it fits together

- `discovery.cpp` follows the bridge on one connection: the route list
  and the snapshot, refreshed with `wait`. For each available route of a
  device in `device_table.hpp`, it builds OpenRGB's `RazerController` and
  `RGBController_Razer` off the GUI thread, because building them talks
  to the device.
- `route_session.cpp` gives each controller its own connection. The
  controller reaches it through `hidapi.h`, three functions that stand in
  for hidapi. The session holds the device's lease for a burst of reports.
- `mirror.cpp` shows each controller in OpenRGB as a plugin API 5 virtual
  controller and forwards changes back to it.
- `plugin.cpp` keeps all of this on the GUI thread and feeds the page
  (`page_model.cpp`, `page_widget.cpp`, `apca.cpp`). It also hides a
  device that OpenRGB already drives itself under the same serial.

There are two patches. The first makes OpenRGB ask keyboards for their
layout the way OpenRazer does, since that's the form the bridge's command
lists allow. The second stops the first lighting update after a controller
is created from getting lost.

## The page

A header line comes first, with the bridge's state and a quiet link to
the project. Below it there's a card per device, in as many columns as
fit. Each card has:

- the device's icon, name and connection
- its battery figure, with the state in words beside it
- a thin level
- small print such as `Not yet tested through the receiver`

When the bridge is away, a note above the cards says what to run. With
nothing to list, the page says why.

The mouse or keyboard icon comes from the desktop's icon theme (symbolic
icons only). Where the theme has none, the page uses a line drawing. The
small icons beside the state, the note and the link are the page's own
drawings, as tall as a capital letter of the text next to them, because
themes draw these too differently (some as filled blocks).

The page sets no style sheet of its own. How its labels look is up to the
style, the palette and a Skin plugin skin, if there is one. Some parts the
page paints itself: the card backgrounds and borders, muted text, icons,
battery figures and levels. Their colours are derived from the theme's or
skin's own text and background after the style has polished its widgets, and
each one is then checked with APCA-W3 (`apca.cpp`) against what is actually
painted behind it. The targets are:

- Lc 60 for muted text, the link and large figures
- Lc 70 for icons, whose lines are thin
- Lc 45 for level fills
- Lc 15 for borders

A low battery (20 % or less, neither charging nor rising) is the one
thing that gets a colour of its own. It goes on that card's figure and
level.

A skin can select the page's widgets by object name:

- `HyperFluxPage`: the whole page
- `HyperFluxTitle`, `HyperFluxProjectLink`, `HyperFluxProjectIcon`,
  `HyperFluxNotice`: the header and the note
- `HyperFluxDevices`: the grid of cards
- `HyperFluxDevice`: a card, with `low="true"` when its battery is low
- `HyperFluxDeviceName`, `HyperFluxDeviceDetails`, `HyperFluxDeviceState`,
  `HyperFluxDeviceNote`: a card's labels
- `HyperFluxDevicePercent`, `HyperFluxDeviceLevel`: the figure and level,
  which the page paints
- `HyperFluxDeviceIcon`, `HyperFluxStateIcon`, `HyperFluxNoticeIcon`,
  `HyperFluxEmptyIcon`: the icons
- `HyperFluxEmpty`, `HyperFluxEmptyHeadline`, `HyperFluxEmptyHint`: what
  shows in place of the cards

Muted labels also carry `muted="true"`. When a skin gives a label a
colour, the page starts from that colour. It fades muted labels no further
than Lc 60, and a card's figure, level and icons follow the colour of the
card's name. For example:

    #HyperFluxDevice[low="true"] #HyperFluxDeviceState { font-weight: bold; }
    #HyperFluxPage QLabel[muted="true"] { color: #2a5d8f; }

`tests/test_page_widget.cpp` builds the page in a light theme and a dark
one, and under two style sheets written the way the Skin plugin's skins
are. It checks the columns at three widths, that no text is cut off, and
that the painted colours reach their targets. Set `HFX_SKIN=file.qss` to
add a real skin to the run.

## Licences

The plugin is GPL-2.0-only. It compiles in OpenRGB sources and
nlohmann/json from the OpenRGB tree (MIT, `LICENSE-nlohmann-json`).
