// SPDX-License-Identifier: GPL-2.0-only

#include "page_widget.hpp"

#include "apca.hpp"

#include <QApplication>
#include <QDesktopServices>
#include <QEvent>
#include <QGridLayout>
#include <QIcon>
#include <QImage>
#include <QLabel>
#include <QPainter>
#include <QPainterPath>
#include <QRegularExpression>
#include <QScrollArea>
#include <QStyle>
#include <QTimer>
#include <QUrl>
#include <QVBoxLayout>

#include <algorithm>
#include <cmath>
#include <vector>

namespace hyperflux::openrgb
{
namespace
{

// The low battery colour starts from a red-orange and is lightened or
// darkened from there until it stands out from the card. Orange stays a
// warm colour when it has to be lightened for a dark theme, where a red
// would turn pink.
const QColor low_colour(0xe0, 0x50, 0x1c);

Rgb rgb(const QColor& colour)
{
    return {static_cast<double>(colour.red()), static_cast<double>(colour.green()),
            static_cast<double>(colour.blue())};
}

QColor qcolor(Rgb colour)
{
    return QColor(static_cast<int>(std::lround(colour.r)), static_cast<int>(std::lround(colour.g)),
                  static_cast<int>(std::lround(colour.b)));
}

double contrast(const QColor& colour, const QColor& background)
{
    return std::abs(apca_contrast(rgb(colour), rgb(background)));
}

// White or black, whichever stands out more on `background`.
QColor extreme(const QColor& background)
{
    return contrast(Qt::white, background) > contrast(Qt::black, background) ? QColor(Qt::white) : QColor(Qt::black);
}

// `colour` lightened or darkened, whichever the background leaves more room
// for, until it reaches `target`.
QColor fit(const QColor& colour, const QColor& background, double target)
{
    return qcolor(reach(rgb(colour), rgb(extreme(background)), rgb(background), target));
}

// `text` faded toward the background as far as `target` allows. Text that
// can't stand out there at all (a skin's gradient running dark behind dark
// text) gives way to black or white.
QColor faded(QColor text, const QColor& background, double target)
{
    if(contrast(text, background) < target)
        text = extreme(background);
    return qcolor(reach(rgb(background), rgb(text), rgb(background), target));
}

// CIE lightness, 0 to 100.
double lightness(const QColor& colour)
{
    const auto linear = [](double v) { return v <= 0.04045 ? v / 12.92 : std::pow((v + 0.055) / 1.055, 2.4); };
    const double y = 0.2126 * linear(colour.redF()) + 0.7152 * linear(colour.greenF()) + 0.0722 * linear(colour.blueF());
    return y > 0.008856 ? 116 * std::cbrt(y) - 16 : 903.3 * y;
}

// A card's surface: four steps of lightness above the background, or below
// it where the background is too close to white for that.
QColor step(const QColor& background)
{
    const double from = lightness(background);
    const Rgb toward = from <= 96 ? Rgb {255, 255, 255} : Rgb {};
    double amount = 0;
    while(amount < 1 && std::abs(lightness(qcolor(mix(rgb(background), toward, amount))) - from) < 4)
        amount += 0.01;
    return qcolor(mix(rgb(background), toward, amount));
}

QColor over(const QColor& top, const QColor& bottom)
{
    const double a = top.alphaF();
    const auto channel = [a](double t, double b) { return static_cast<float>(t * a + b * (1 - a)); };
    return QColor::fromRgbF(channel(top.redF(), bottom.redF()), channel(top.greenF(), bottom.greenF()),
                            channel(top.blueF(), bottom.blueF()));
}

// A skin's gradient counts as the average of its stops.
QColor flat(const QBrush& brush)
{
    const QGradient* gradient = brush.gradient();
    if(gradient == nullptr || gradient->stops().isEmpty())
        return brush.style() == Qt::NoBrush ? QColor(0, 0, 0, 0) : brush.color();
    double r = 0, g = 0, b = 0, a = 0;
    for(const auto& stop : gradient->stops())
    {
        r += stop.second.redF();
        g += stop.second.greenF();
        b += stop.second.blueF();
        a += stop.second.alphaF();
    }
    const auto n = static_cast<double>(gradient->stops().size());
    return QColor::fromRgbF(static_cast<float>(r / n), static_cast<float>(g / n), static_cast<float>(b / n),
                            static_cast<float>(a / n));
}

// The background a widget paints itself, if it hides what is behind.
QColor own_fill(const QWidget* widget)
{
    const QColor colour = flat(widget->palette().brush(widget->backgroundRole()));
    const bool fills = widget->autoFillBackground() || widget->testAttribute(Qt::WA_StyledBackground);
    return fills && colour.alpha() == 255 ? colour : QColor();
}

QString from(const std::string& value)
{
    return QString::fromStdString(value);
}

// The page's own widget classes have no meta-object. findChildren() can't
// tell them apart.
template<typename T>
std::vector<T*> all(QWidget* root)
{
    std::vector<T*> result;
    for(QWidget* widget : root->findChildren<QWidget*>())
        if(auto* found = dynamic_cast<T*>(widget))
            result.push_back(found);
    return result;
}

void scale(QWidget* widget, double factor, QFont::Weight weight = QFont::Normal)
{
    QFont font = widget->font();
    if(font.pixelSize() > 0)
        font.setPixelSize(std::max(1, static_cast<int>(std::lround(font.pixelSize() * factor))));
    else
        font.setPointSizeF(font.pointSizeF() * factor);
    font.setWeight(weight);
    widget->setFont(font);
}

// ------------------------------------------------------------------ icons

enum class Glyph { Mouse, Keyboard, Device, Charging, Full, Asleep, Unreachable, Waiting, External, Info };

// The device's own icon comes from the desktop's theme, and it has to be a
// symbolic one: every icon is tinted to one colour, which leaves a coloured
// icon a solid blob. State icons vary too much between themes (YAMIS draws
// charging as a filled block). We draw those ourselves.
QIcon theme_icon(Glyph glyph)
{
    const char* name = glyph == Glyph::Mouse ? "input-mouse-symbolic"
                       : glyph == Glyph::Keyboard ? "input-keyboard-symbolic"
                                                  : nullptr;
    return name != nullptr && QIcon::hasThemeIcon(name) ? QIcon::fromTheme(name) : QIcon();
}

// Line drawings on a 24 unit grid. Strokes stay at least 1.25 pixels wide
// however small the icon. A state icon the size of a capital letter still
// has the weight of the text beside it.
void draw_glyph(QPainter& painter, Glyph glyph, const QRectF& area, const QColor& colour)
{
    painter.save();
    painter.translate(area.topLeft());
    const double scale = area.height() / 24;
    painter.scale(area.width() / 24, scale);
    painter.setRenderHint(QPainter::Antialiasing);
    painter.setPen(QPen(colour, std::max(1.8, 1.25 / scale), Qt::SolidLine, Qt::RoundCap, Qt::RoundJoin));
    switch(glyph)
    {
        case Glyph::Mouse:
            painter.drawRoundedRect(QRectF(6.5, 2.5, 11, 19), 5.5, 5.5);
            painter.drawLine(QPointF(12, 6), QPointF(12, 9.5));
            break;
        case Glyph::Keyboard:
            painter.drawRoundedRect(QRectF(2.5, 6, 19, 12), 2, 2);
            for(const double x : {6.5, 10.0, 13.5, 17.0})
                painter.drawPoint(QPointF(x, 10));
            painter.drawLine(QPointF(8, 14), QPointF(16, 14));
            break;
        case Glyph::Device: painter.drawRoundedRect(QRectF(4, 4, 16, 16), 3, 3); break;
        case Glyph::Charging: painter.drawPolyline(QPolygonF({{15, 1.5}, {6.5, 13}, {17.5, 11}, {9, 22.5}})); break;
        case Glyph::Full:
            painter.drawRoundedRect(QRectF(1.5, 5.5, 18, 13), 2.5, 2.5);
            painter.drawLine(QPointF(22.5, 9.5), QPointF(22.5, 14.5));
            painter.setBrush(colour);
            painter.setPen(Qt::NoPen);
            painter.drawRect(QRectF(5, 9, 11, 6));
            break;
        case Glyph::Asleep:
        {
            QPainterPath moon;
            moon.addEllipse(QPointF(12, 12), 9, 9);
            QPainterPath bite;
            bite.addEllipse(QPointF(17.5, 7.5), 8, 8);
            painter.drawPath(moon.subtracted(bite));
            break;
        }
        case Glyph::Unreachable:
            // Radio waves with a slash don't survive the size of a capital
            // letter, but a crossed circle does.
            painter.drawEllipse(QPointF(12, 12), 9.5, 9.5);
            painter.drawLine(QPointF(5.5, 5.5), QPointF(18.5, 18.5));
            break;
        case Glyph::Waiting:
            painter.drawEllipse(QPointF(12, 12), 9.5, 9.5);
            painter.drawPolyline(QPolygonF({{12, 6.5}, {12, 12}, {15.5, 14.5}}));
            break;
        case Glyph::External:
            painter.drawPolyline(QPolygonF({{10, 4}, {4, 4}, {4, 20}, {20, 20}, {20, 14}}));
            painter.drawPolyline(QPolygonF({{14, 4}, {20, 4}, {20, 10}}));
            painter.drawLine(QPointF(11, 13), QPointF(20, 4));
            break;
        case Glyph::Info:
            painter.drawEllipse(QPointF(12, 12), 10.5, 10.5);
            painter.drawLine(QPointF(12, 10.5), QPointF(12, 17.5));
            painter.drawPoint(QPointF(12, 6.5));
            break;
    }
    painter.restore();
}

// An icon in one colour, which recolour() works out like muted text. It
// follows the theme and the skin. One that goes with a line of text takes
// that text's cap height. It opens `url` when there is one.
class Icon final : public QWidget
{
public:
    Icon(Glyph glyph, int size, const char* name) : glyph_(glyph), icon_(theme_icon(glyph))
    {
        setObjectName(name);
        setFixedSize(size, size);
    }

    Icon(Glyph glyph, QLabel* text, const char* name) : Icon(glyph, 1, name)
    {
        text_ = text;
        fit_text();
    }

    QColor colour;
    QUrl url;

    // As tall as a line of the text, with the drawing standing on its
    // baseline. Again after polishing, once the text has the skin's font.
    void fit_text()
    {
        if(text_ == nullptr)
            return;
        const QFontMetrics metrics = text_->fontMetrics();
        cap_ = std::max(8, metrics.capHeight());
        ascent_ = metrics.ascent();
        setFixedSize(cap_, metrics.height());
    }

protected:
    void paintEvent(QPaintEvent*) override
    {
        QPainter painter(this);
        if(text_ != nullptr)
            return draw_glyph(painter, glyph_, QRectF(0, ascent_ - cap_, cap_, cap_), colour);
        const qreal ratio = devicePixelRatioF();
        const QPixmap pixmap = icon_.isNull() ? QPixmap() : icon_.pixmap(size() * ratio);
        if(pixmap.isNull())
            return draw_glyph(painter, glyph_, rect(), colour);
        QImage image = pixmap.toImage().convertToFormat(QImage::Format_ARGB32_Premultiplied);
        QPainter tint(&image);
        tint.setCompositionMode(QPainter::CompositionMode_SourceIn);
        tint.fillRect(image.rect(), colour);
        tint.end();
        const QSizeF shown = QSizeF(image.size()) / ratio;
        painter.drawImage(QRectF(QPointF((width() - shown.width()) / 2, (height() - shown.height()) / 2), shown), image);
    }

    void mousePressEvent(QMouseEvent*) override
    {
        if(url.isValid())
            QDesktopServices::openUrl(url);
    }

private:
    Glyph glyph_;
    QIcon icon_;
    QLabel* text_ = nullptr;
    int cap_ = 0;
    int ascent_ = 0;
};

// Muted text in the colour recolour() worked out. A plain QLabel would
// paint in the style sheet's text colour instead, since it applies the
// sheet's palette again as it paints, and under most skins nothing would
// look muted.
class QuietLabel final : public QLabel
{
public:
    using QLabel::QLabel;

protected:
    void paintEvent(QPaintEvent*) override
    {
        QPainter painter(this);
        drawFrame(&painter);
        const int flags = static_cast<int>(alignment()) | (wordWrap() ? Qt::TextWordWrap : 0);
        style()->drawItemText(&painter, contentsRect(), flags, palette(), isEnabled(), text(), foregroundRole());
    }
};

QLabel* label(const QString& text, const char* name, bool muted = false)
{
    QLabel* result = muted ? new QuietLabel(text) : new QLabel(text);
    result->setObjectName(name);
    result->setTextFormat(Qt::PlainText);
    result->setWordWrap(true);
    if(muted)
        result->setProperty("muted", true);
    return result;
}

// Text with a command in it, set as code, kept on one line and selectable.
QLabel* with_command(const Notice& notice, const char* name)
{
    QString html = from(notice.message).toHtmlEscaped();
    if(!notice.command.empty())
    {
        const QString command = from(notice.command).toHtmlEscaped();
        html.replace(command, "<code style=\"white-space: nowrap\">" + command + "</code>");
    }
    auto* result = new QLabel(html);
    result->setObjectName(name);
    result->setTextFormat(Qt::RichText);
    result->setWordWrap(true);
    result->setTextInteractionFlags(Qt::TextSelectableByMouse);
    return result;
}

// The name, and the state in a muted colour of its own, as one label that
// can wrap on a narrow page.
QString title_html(const QString& status, const QColor& muted)
{
    return QString("<b>HyperFlux Next V2</b>&nbsp;&nbsp;<span style=\"color: %1\">%2</span>")
        .arg(muted.name(), status.toHtmlEscaped());
}

// The anchor carries its colour: a label's rich text takes link colours
// from the application palette when the text is set. Neither the widget's
// palette nor a skin reaches them.
QString link_html(const QColor& colour)
{
    return QString("<a href=\"%1\" style=\"color: %2\">Project page</a>")
        .arg(QStringLiteral(HFX_PROJECT_URL).toHtmlEscaped(), colour.name());
}

// ------------------------------------------------------------------ cards

// A device name that drops "Razer " and then elides when the card is
// narrow. The tooltip keeps the whole name.
class Name final : public QLabel
{
public:
    explicit Name(const QString& full) : QLabel(full), full_(full)
    {
        setObjectName("HyperFluxDeviceName");
        setTextFormat(Qt::PlainText);
        setToolTip(full);
        setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
        scale(this, 1, QFont::Medium);
    }

    QSize minimumSizeHint() const override
    {
        return {fontMetrics().averageCharWidth() * 8, QLabel::minimumSizeHint().height()};
    }

protected:
    void resizeEvent(QResizeEvent* event) override
    {
        QLabel::resizeEvent(event);
        const QFontMetrics metrics = fontMetrics();
        QString shown = full_;
        if(metrics.horizontalAdvance(shown) > width())
            shown = metrics.elidedText(full_.startsWith("Razer ") ? full_.mid(6) : full_, Qt::ElideRight, width());
        if(shown != text())
            setText(shown);
    }

private:
    QString full_;
};

// The battery figure, painted rather than a label so that a skin's label
// colour can't take the low colour away, and sized from the skin's font.
class Figure final : public QWidget
{
public:
    explicit Figure(QString text) : text_(std::move(text))
    {
        setObjectName("HyperFluxDevicePercent");
        setAccessibleName(text_);
    }

    QSize sizeHint() const override
    {
        const QFontMetrics metrics(big());
        return {metrics.horizontalAdvance(text_), metrics.height()};
    }

    QSize minimumSizeHint() const override { return sizeHint(); }

    QColor colour;

protected:
    void paintEvent(QPaintEvent*) override
    {
        QPainter painter(this);
        painter.setFont(big());
        painter.setPen(colour);
        painter.drawText(rect(), Qt::AlignLeft | Qt::AlignVCenter, text_);
    }

private:
    // From the font the widget ends up with. A skin that sets every font's
    // size still gets a figure that stands out.
    QFont big() const
    {
        QFont result = font();
        if(result.pixelSize() > 0)
            result.setPixelSize(static_cast<int>(std::lround(result.pixelSize() * 1.8)));
        else
            result.setPointSizeF(result.pointSizeF() * 1.8);
        result.setWeight(QFont::Medium);
        return result;
    }

    QString text_;
};

// The battery level: a thin rounded track and a fill. QProgressBar would
// hand the fill to the style, and Breeze, Kvantum and the skins paint it in
// their own accent whatever the palette says. A low battery wouldn't show.
class Level final : public QWidget
{
public:
    explicit Level(int percent) : percent_(percent)
    {
        setObjectName("HyperFluxDeviceLevel");
        setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Fixed);
    }

    QSize sizeHint() const override
    {
        const int thickness = std::max(4, fontMetrics().height() / 4);
        return {thickness * 10, thickness};
    }

    QSize minimumSizeHint() const override { return sizeHint(); }

    QColor track;
    QColor fill;

protected:
    void paintEvent(QPaintEvent*) override
    {
        QPainter painter(this);
        painter.setRenderHint(QPainter::Antialiasing);
        painter.setPen(Qt::NoPen);
        const QRectF area = rect();
        const qreal radius = area.height() / 2;
        painter.setBrush(track);
        painter.drawRoundedRect(area, radius, radius);
        if(percent_ <= 0)
            return;
        QRectF filled = area;
        filled.setWidth(std::max(area.height(), area.width() * percent_ / 100));
        painter.setBrush(fill);
        painter.drawRoundedRect(filled, radius, radius);
    }

private:
    int percent_;
};

// A device's card: a surface one step off what is behind it, with a
// hairline edge, both worked out by recolour().
class Card final : public QWidget
{
public:
    QColor surface;
    QColor edge;

protected:
    void paintEvent(QPaintEvent*) override
    {
        if(!surface.isValid())
            return;
        QPainter painter(this);
        painter.setRenderHint(QPainter::Antialiasing);
        painter.setPen(QPen(edge, 1));
        painter.setBrush(surface);
        const qreal radius = fontMetrics().height() / 3.0;
        painter.drawRoundedRect(QRectF(rect()).adjusted(0.5, 0.5, -0.5, -0.5), radius, radius);
    }
};

Glyph glyph_of(const std::string& type)
{
    return type == "Mouse" ? Glyph::Mouse : type == "Keyboard" ? Glyph::Keyboard : Glyph::Device;
}

QWidget* card_for(const DeviceRow& device)
{
    auto* card = new Card();
    card->setObjectName("HyperFluxDevice");
    card->setProperty("key", from(device.key));
    card->setProperty("low", device.low);
    card->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
    const int line = card->fontMetrics().height();
    auto* grid = new QGridLayout(card);
    grid->setContentsMargins(line * 3 / 4, line * 2 / 3, line * 3 / 4, line * 2 / 3);
    grid->setHorizontalSpacing(line / 2);
    grid->setVerticalSpacing(line / 6);
    grid->setColumnStretch(1, 1);

    // Who: the type's icon, the name and how it is connected.
    // The size themes draw device icons for, or a size up for large fonts.
    // In between, theme icons come out blurred.
    grid->addWidget(new Icon(glyph_of(device.type), line >= 28 ? 32 : 22, "HyperFluxDeviceIcon"), 0, 0, 2, 1,
                    Qt::AlignVCenter);
    grid->addWidget(new Name(from(device.name)), 0, 1);
    grid->addWidget(label(from(device.details), "HyperFluxDeviceDetails", true), 1, 1);
    grid->setRowMinimumHeight(2, line / 2);

    // How much: the figure people come for, and the state beside it.
    auto* reading = new QHBoxLayout();
    reading->setSpacing(line / 3);
    auto* figure = new Figure(device.percent ? QString("%1%").arg(*device.percent) : QString("–"));
    figure->setProperty("low", device.low);
    reading->addWidget(figure, 0, Qt::AlignVCenter);
    reading->addSpacing(line / 6);
    if(!device.state.empty())
    {
        QLabel* state = label(from(device.state), "HyperFluxDeviceState");
        // In the order of Sign, which starts with None.
        const Glyph glyphs[] = {{}, Glyph::Charging, Glyph::Full, Glyph::Asleep, Glyph::Unreachable, Glyph::Waiting};
        if(device.sign != Sign::None)
            reading->addWidget(new Icon(glyphs[static_cast<int>(device.sign)], state, "HyperFluxStateIcon"), 0,
                               Qt::AlignVCenter);
        reading->addWidget(state, 1, Qt::AlignVCenter);
    }
    else
    {
        reading->addStretch(1);
    }
    grid->addLayout(reading, 3, 0, 1, 2);
    int row = 4;
    if(device.percent)
    {
        auto* level = new Level(*device.percent);
        level->setProperty("low", device.low);
        level->setAccessibleName(QString("%1 battery, %2%").arg(from(device.name)).arg(*device.percent));
        grid->addWidget(level, row++, 0, 1, 2);
    }
    if(!device.notes.empty())
        grid->setRowMinimumHeight(row++, line / 3);
    for(const auto& note : device.notes)
    {
        QLabel* small = label(from(note), "HyperFluxDeviceNote", true);
        scale(small, 0.9);
        grid->addWidget(small, row++, 0, 1, 2);
    }
    grid->setRowStretch(row, 1); // a taller neighbour mustn't spread this card out
    return card;
}

// The cards in as many columns as fit: one in a narrow window, two at
// OpenRGB's usual width, more when there is a lot of room.
class Cards final : public QWidget
{
public:
    explicit Cards(const std::vector<DeviceRow>& devices)
    {
        setObjectName("HyperFluxDevices");
        for(const DeviceRow& device : devices)
        {
            cards_.push_back(card_for(device));
            cards_.back()->setParent(this);
            cards_.back()->setMinimumWidth(narrow());
        }
        arrange(1);
    }

protected:
    void resizeEvent(QResizeEvent* event) override
    {
        QWidget::resizeEvent(event);
        int columns = 1;
        // A third column only where the text has room: DejaVu Sans's lines
        // are a fifth shorter than Noto Sans's, but its text is wider.
        const int wide = fontMetrics().horizontalAdvance(QStringLiteral("abcdefghijklmnopqrstuvwxyz")) * 2;
        while(columns < 4 && width() >= (columns + 1) * (columns == 1 ? narrow() : wide) + columns * gap())
            ++columns;
        arrange(columns);
    }

private:
    int narrow() const { return fontMetrics().height() * 14; }
    int gap() const { return fontMetrics().height() * 2 / 3; }

    void arrange(int columns)
    {
        if(columns == columns_)
            return;
        columns_ = columns;
        delete layout();
        auto* grid = new QGridLayout(this);
        grid->setContentsMargins(0, 0, 0, 0);
        grid->setSpacing(gap());
        for(int column = 0; column < columns; ++column)
            grid->setColumnStretch(column, 1);
        for(std::size_t i = 0; i < cards_.size(); ++i)
            grid->addWidget(cards_[i], static_cast<int>(i) / columns, static_cast<int>(i) % columns);
    }

    std::vector<QWidget*> cards_;
    int columns_ = 0;
};

// ------------------------------------------------------------------ page

QWidget* empty_state(const Empty& empty)
{
    auto* box = new QWidget();
    box->setObjectName("HyperFluxEmpty");
    auto* layout = new QVBoxLayout(box);
    const int line = box->fontMetrics().height();
    layout->setContentsMargins(line, line * 3, line, line);
    layout->setSpacing(line / 2);
    layout->addWidget(new Icon(Glyph::Mouse, line >= 28 ? 64 : 48, "HyperFluxEmptyIcon"), 0, Qt::AlignHCenter);
    QLabel* headline = label(from(empty.headline), "HyperFluxEmptyHeadline");
    scale(headline, 1.25, QFont::Medium);
    QLabel* hint = with_command(empty.hint, "HyperFluxEmptyHint");
    for(QLabel* text : {headline, hint})
    {
        text->setAlignment(Qt::AlignHCenter);
        layout->addWidget(text);
    }
    return box;
}

QWidget* build(const Page& page)
{
    auto* root = new QWidget();
    root->setObjectName("HyperFluxPage");
    auto* layout = new QVBoxLayout(root);
    const int line = root->fontMetrics().height();
    layout->setSpacing(line * 2 / 3);

    // Whose page and how things stand on one line, the project quietly on
    // the right.
    auto* head = new QHBoxLayout();
    QString status = "· " + from(page.status);
    if(const auto count = page.devices.size(); count > 0)
        status += QString(" · %1 %2").arg(count).arg(count == 1 ? "device" : "devices");
    auto* title = new QLabel(title_html(status, QApplication::palette().color(QPalette::WindowText)));
    title->setObjectName("HyperFluxTitle");
    title->setTextFormat(Qt::RichText);
    title->setWordWrap(true);
    title->setProperty("status", status);
    title->setToolTip(from(page.detail));
    head->addWidget(title, 1);
    const QString place = QStringLiteral(HFX_PROJECT_URL).remove(QRegularExpression("^https?://"));
    auto* link = new QLabel(link_html(QApplication::palette().color(QPalette::Link)));
    link->setObjectName("HyperFluxProjectLink");
    link->setTextFormat(Qt::RichText);
    link->setOpenExternalLinks(true);
    link->setTextInteractionFlags(Qt::LinksAccessibleByMouse | Qt::LinksAccessibleByKeyboard);
    link->setToolTip(place);
    head->addWidget(link, 0, Qt::AlignVCenter);
    auto* arrow = new Icon(Glyph::External, link, "HyperFluxProjectIcon");
    arrow->url = QUrl(QStringLiteral(HFX_PROJECT_URL));
    arrow->setToolTip(place);
    arrow->setCursor(Qt::PointingHandCursor);
    head->addWidget(arrow, 0, Qt::AlignVCenter);
    layout->addLayout(head);

    // A calm note when the bridge is away or the readings are old.
    if(page.notice)
    {
        auto* note = new QHBoxLayout();
        note->setSpacing(line / 2);
        QLabel* text = with_command(*page.notice, "HyperFluxNotice");
        auto* icon = new Icon(Glyph::Info, text, "HyperFluxNoticeIcon");
        note->addWidget(icon, 0, Qt::AlignTop);
        note->addWidget(text, 1);
        layout->addLayout(note);
    }

    if(page.empty)
        layout->addWidget(empty_state(*page.empty));
    else if(!page.devices.empty())
        layout->addWidget(new Cards(page.devices));
    layout->addStretch(1);
    return root;
}

} // namespace

PageWidget::PageWidget(Source source, QWidget* parent) : QWidget(parent), source_(std::move(source))
{
    auto* outer = new QVBoxLayout(this);
    outer->setContentsMargins(0, 0, 0, 0);
    auto* scroll = new QScrollArea(this);
    scroll->setFrameShape(QFrame::NoFrame);
    scroll->setWidgetResizable(true);
    outer->addWidget(scroll);
    auto* holder = new QWidget();
    scroll->setWidget(holder);
    // setWidget() turns the fill on. Without it the tab pane shows through,
    // as it does around OpenRGB's own pages.
    holder->setAutoFillBackground(false);
    scroll->viewport()->setAutoFillBackground(false);
    holder_ = new QVBoxLayout(holder);
    holder_->setContentsMargins(0, 0, 0, 0);

    auto* timer = new QTimer(this);
    timer->setInterval(refresh_interval_ms);
    connect(timer, &QTimer::timeout, this, [this] { refresh(); });
    timer->start();
    refresh();
}

void PageWidget::refresh()
{
    Page page = source_();
    if(has_shown_ && page == shown_)
        return;
    shown_ = std::move(page);
    has_shown_ = true;
    rebuild();
}

bool PageWidget::event(QEvent* event)
{
    switch(event->type())
    {
        // Qt is still walking the old children; rebuild once it's done.
        case QEvent::Polish:
        case QEvent::ParentChange:
        case QEvent::PaletteChange:
        case QEvent::StyleChange:
        case QEvent::FontChange:
        case QEvent::ApplicationFontChange:
            if(!rebuilding_)
                later(rebuild_queued_, &PageWidget::rebuild);
            break;
        // What is behind each widget depends on where it ends up.
        case QEvent::Show:
        case QEvent::Resize: later(recolour_queued_, &PageWidget::recolour); break;
        default: break;
    }
    return QWidget::event(event);
}

void PageWidget::later(bool& queued, void (PageWidget::*what)())
{
    if(!has_shown_ || queued)
        return;
    queued = true;
    QMetaObject::invokeMethod(
        this,
        [this, &queued, what]
        {
            queued = false;
            (this->*what)();
        },
        Qt::QueuedConnection);
}

void PageWidget::rebuild()
{
    rebuilding_ = true;
    QWidget* next = build(shown_);
    if(content_ != nullptr)
    {
        holder_->removeWidget(content_);
        delete content_;
    }
    content_ = next;
    holder_->addWidget(content_);
    content_->ensurePolished();
    // What the theme or skin made of each label, for every recolour to
    // start from.
    for(QLabel* label : content_->findChildren<QLabel*>())
        label->setProperty("start", label->palette().color(label->foregroundRole()));
    rebuilding_ = false;
    // The new widgets are shown and laid out from the event loop.
    later(recolour_queued_, &PageWidget::recolour);
}

QColor PageWidget::text_near(QWidget* widget) const
{
    // Skins often colour only labels. The page's own widgets take the text
    // colour of the labels around them: the card's name, or the title.
    const QLabel* source = nullptr;
    for(QWidget* w = widget; w != nullptr && w != content_ && source == nullptr; w = w->parentWidget())
        if(w->objectName() == "HyperFluxDevice")
            source = w->findChild<QLabel*>("HyperFluxDeviceName");
    if(source == nullptr)
        source = content_->findChild<QLabel*>("HyperFluxTitle");
    return source != nullptr ? source->property("start").value<QColor>() : palette().color(QPalette::WindowText);
}

// Once the widgets are polished and in place, since both what the skin made
// of them and what is painted behind them count. Cards go first: the rest
// is measured against them.
void PageWidget::recolour()
{
    if(content_ == nullptr)
        return;
    // One surface for every card, from what is behind the first: over a
    // skin's gradient, cards shaded one by one would come out in bands.
    const auto cards = all<Card>(content_);
    if(!cards.empty())
    {
        const QColor back = backdrop(cards.front());
        // A skin that fills every label decides the surface itself. Another
        // shade under its labels would leave them as patches on the card.
        const auto* name = cards.front()->findChild<QLabel*>("HyperFluxDeviceName");
        const QColor own = name != nullptr ? own_fill(name) : QColor();
        const QColor surface = own.isValid() ? own : step(back);
        for(Card* card : cards)
        {
            card->surface = surface;
            // The edge sets each card off what is behind that one.
            card->edge = faded(text_near(card), backdrop(card), lc::edge);
            card->update();
        }
    }
    if(auto* title = content_->findChild<QLabel*>("HyperFluxTitle"))
        title->setText(title_html(title->property("status").toString(),
                                  faded(text_near(title), backdrop(title), lc::secondary)));
    for(QLabel* label : content_->findChildren<QLabel*>())
    {
        const bool link = label->objectName() == "HyperFluxProjectLink";
        if(!label->property("muted").toBool() && !link)
            continue;
        const QColor colour = faded(link ? text_near(label) : label->property("start").value<QColor>(),
                                    backdrop(label), lc::secondary);
        // A quiet link: the muted colour, told apart by its underline and
        // the arrow beside it.
        if(link)
        {
            label->setText(link_html(colour));
            continue;
        }
        QPalette palette = label->palette();
        palette.setColor(label->foregroundRole(), colour);
        label->setPalette(palette);
    }
    // Icons a shade stronger than muted text: their thin strokes lose more
    // to antialiasing than letters do.
    for(Icon* icon : all<Icon>(content_))
    {
        icon->fit_text();
        icon->colour = faded(text_near(icon), backdrop(icon), lc::secondary + 10);
        icon->update();
    }
    for(Figure* figure : all<Figure>(content_))
    {
        // Low in the low colour, a missing reading's dash as quiet as muted
        // text, and otherwise the text colour, as long as that stands out
        // the way large text has to.
        const QColor back = backdrop(figure);
        const QColor text = text_near(figure);
        figure->colour = figure->property("low").toBool()           ? fit(low_colour, back, lc::secondary)
                         : figure->accessibleName() == QString("–") ? faded(text, back, lc::secondary)
                         : contrast(text, back) >= lc::secondary   ? text
                                                                     : extreme(back);
        figure->update();
    }
    for(Level* level : all<Level>(content_))
    {
        const QColor back = backdrop(level);
        const QColor text = text_near(level);
        // Grey, the text faded as far as an indicator may be, so the one
        // colour on the page is a low battery's. The track marks where
        // 100 % is and nothing more.
        level->fill = level->property("low").toBool() ? fit(low_colour, back, lc::indicator)
                                                      : faded(text, back, lc::indicator);
        level->track = faded(text, back, lc::edge);
        level->update();
    }
}

QColor PageWidget::backdrop(QWidget* widget)
{
    std::vector<QColor> layers; // nearest first
    // The widget's own fill counts, but nothing else it paints: that's text.
    if(widget->autoFillBackground() || widget->testAttribute(Qt::WA_StyledBackground))
        layers.push_back(flat(widget->palette().brush(widget->backgroundRole())));
    // Each ancestor paints itself, without its children, at the spot under
    // the widget's middle. That takes in whatever its style does there: the
    // shade Fusion or Breeze give a tab pane, a skin's gradient, a
    // translucent Kvantum view, a card.
    const QPoint middle = widget->rect().center();
    for(QWidget* w = widget->parentWidget(); w != nullptr; w = w->parentWidget())
    {
        if(!layers.empty() && layers.back().alpha() == 255)
            break;
        const QPoint at = widget->mapTo(w, middle);
        QImage pixel(1, 1, QImage::Format_ARGB32_Premultiplied);
        pixel.fill(Qt::transparent);
        w->render(&pixel, QPoint(), QRegion(at.x(), at.y(), 1, 1),
                  w->isWindow() ? QWidget::DrawWindowBackground : QWidget::RenderFlags());
        if(const QColor colour = pixel.pixelColor(0, 0); colour.alpha() > 0)
            layers.push_back(colour);
    }
    QColor result = QApplication::palette().color(QPalette::Window);
    for(auto layer = layers.rbegin(); layer != layers.rend(); ++layer)
        result = over(*layer, result);
    return result;
}

} // namespace hyperflux::openrgb
