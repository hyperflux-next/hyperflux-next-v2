// SPDX-License-Identifier: GPL-2.0-only

#pragma once

#include "page_model.hpp"

#include <QColor>
#include <QWidget>

#include <functional>

class QVBoxLayout;

namespace hyperflux::openrgb
{

// The HyperFlux Next V2 page in OpenRGB's Information list.
//
// A card per device, in Qt widgets that take their look from the style,
// palette and style sheet OpenRGB runs with, the Skin plugin's included.
// The page sets no style sheet of its own. What it paints itself (the
// cards' surfaces and edges, muted text, icons, figures and levels) gets
// colours worked out from the theme's or skin's own, after the style has
// had its say, and checked against what is painted behind them
// (apca.hpp). The README lists the object names a skin can select.
class PageWidget final : public QWidget
{
public:
    using Source = std::function<Page()>;

    explicit PageWidget(Source source, QWidget* parent = nullptr);

    // Rebuilds the page when what it shows has changed.
    void refresh();

    // Often enough that "Last update N minutes ago" stays true.
    static constexpr int refresh_interval_ms = 30'000;

    // The colour behind `widget` as it is painted: its own fill or what its
    // ancestors paint there, translucent layers laid over what is below.
    [[nodiscard]] static QColor backdrop(QWidget* widget);

protected:
    bool event(QEvent* event) override;

private:
    void rebuild();
    void recolour();
    void later(bool& queued, void (PageWidget::*what)());
    [[nodiscard]] QColor text_near(QWidget* widget) const;

    Source source_;
    Page shown_;
    bool has_shown_ = false;
    bool rebuilding_ = false;
    bool rebuild_queued_ = false;
    bool recolour_queued_ = false;
    QVBoxLayout* holder_ = nullptr;
    QWidget* content_ = nullptr;
};

} // namespace hyperflux::openrgb
