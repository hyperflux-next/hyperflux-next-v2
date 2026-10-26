// SPDX-License-Identifier: GPL-2.0-only
//
// The page offscreen in a tab widget, as OpenRGB shows it, under a light and
// a dark theme and two skins: what the cards say, their columns, no text cut
// off, and the colours the page paints against APCA targets. Run with
// QT_QPA_PLATFORM=offscreen; HFX_SKIN=file.qss adds a skin, HFX_SCREENS=dir
// saves each theme's look.

#include "check.hpp"

#include "apca.hpp"
#include "page_widget.hpp"

#include <QApplication>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QFontInfo>
#include <QIcon>
#include <QImage>
#include <QLabel>
#include <QMainWindow>
#include <QRegularExpression>
#include <QScrollArea>
#include <QScrollBar>
#include <QStyle>
#include <QStyleFactory>
#include <QTabWidget>
#include <QTemporaryDir>

#include <cmath>
#include <cstdio>
#include <set>

using namespace hyperflux::openrgb;

namespace
{

Page sample()
{
    DeviceRow charging {"r1/pointer-00cd", "Razer Basilisk V3 Pro 35K", "Mouse", "Mouse · Wireless", 79, false,
                        "Charging", Sign::Charging, {}};
    DeviceRow low {"r1/keyboard-0296", "Razer DeathStalker V2 Pro TKL", "Keyboard", "Keyboard · Wireless", 12, true,
                   "Low battery", Sign::None, {}};
    DeviceRow asleep {"r2/pointer-00ab", "Razer Basilisk V3 Pro", "Mouse", "Mouse · Wireless", 64, false, "Asleep",
                      Sign::Asleep, {"Not yet tested through the receiver", "Last update 42 minutes ago"}};
    DeviceRow wired {"r2/keyboard-025c", "Razer BlackWidow V3 Pro", "Keyboard", "Keyboard · Wired", std::nullopt,
                     false, "", Sign::None,
                     {"Battery level not reported yet", "OpenRGB controls it over the cable until it is unplugged."}};
    return {"Not running", "the bridge closed the connection",
            Notice {"These readings are from 3 minutes ago. The bridge isn't answering. Run hyperfluxctl doctor "
                    "--explain in a terminal to see the fix.",
                    "hyperfluxctl doctor --explain"},
            {charging, low, asleep, wired}, std::nullopt};
}

// Breeze Dark's colours, from KDE's BreezeDark.colors.
QPalette dark()
{
    QPalette palette(QColor(41, 44, 48), QColor(32, 35, 38));
    for(const auto& [role, colour] : std::initializer_list<std::pair<QPalette::ColorRole, QColor>> {
            {QPalette::WindowText, QColor(252, 252, 252)},
            {QPalette::Text, QColor(252, 252, 252)},
            {QPalette::Base, QColor(20, 22, 24)},
            {QPalette::Link, QColor(29, 153, 243)}})
        palette.setColor(role, colour);
    return palette;
}

// The rules the Skin plugin's Numix skins set for the widgets around and
// in the page: every widget transparent with light text, and the dark
// colour on the window, the scroll area and the tab pane.
constexpr const char* numix_like = R"(
QWidget { background-color: transparent; color: #F2F2F2; font: normal; }
QMainWindow, QAbstractScrollArea { background-color: #1E1E1E; }
QTabWidget::pane { border: 1px solid #4D4D4D; background-color: #1E1E1E; }
QLabel { color: #F2F2F2; }
)";

// And the Light Blue Gradient skin's way: one gradient on the window,
// light at the top and dark blue at the bottom, behind everything.
constexpr const char* gradient = R"(
QMainWindow { background: qlineargradient(x1: 0, y1: 0, x2: 0, y2: 1, stop: 0 rgb(191, 230, 255),
              stop: 0.4 rgb(194, 216, 242), stop: 1.0 rgb(0, 78, 130)); color: rgb(0, 0, 0); }
QTabWidget::pane { border: 1px solid rgb(255, 255, 255); }
)";

struct Theme
{
    QString name;
    QPalette palette;
    QString sheet;
    bool ours = true; // a theme the test defines, so its text has to pass too
};

double lc(const QColor& text, const QColor& background)
{
    return std::abs(apca_contrast({double(text.red()), double(text.green()), double(text.blue())},
                                  {double(background.red()), double(background.green()), double(background.blue())}));
}

// The strongest pixel inside a widget: its text or icon colour, give or
// take antialiasing, whatever palette the widget claims to have.
double drawn(const QImage& shot, const QWidget& window, const QWidget* widget, const QColor& back)
{
    const QRect area = QRect(widget->mapTo(&window, QPoint(0, 0)), widget->size()) & shot.rect();
    double strongest = 0;
    for(int y = area.top(); y <= area.bottom(); ++y)
        for(int x = area.left(); x <= area.right(); ++x)
            strongest = std::max(strongest, lc(shot.pixelColor(x, y), back));
    return strongest;
}

// A bare container has no fonts, and Qt's stand-in is so wide and so
// unlike any real font that text sizes there say nothing.
bool fonts()
{
    return !QFontInfo(QApplication::font()).family().isEmpty();
}

// Lets the page's queued rebuilds and recolours run.
void settle()
{
    for(int i = 0; i < 10; ++i)
        QApplication::processEvents();
}

bool has_label(const QWidget& page, const QString& text)
{
    for(const QLabel* label : page.findChildren<QLabel*>())
        if(label->text().contains(text) || label->toolTip() == text)
            return true;
    return false;
}

void shows_the_model(const QWidget& page)
{
    CHECK(page.findChildren<QWidget*>("HyperFluxDevice").size() == 4);
    for(const char* text : {"Razer DeathStalker V2 Pro TKL", "Low battery", "Asleep", "Charging",
                            "Not yet tested through the receiver", "Last update 42 minutes ago",
                            "Battery level not reported yet",
                            "OpenRGB controls it over the cable", "Mouse · Wireless", "· Not running · 4 devices"})
        CHECK(has_label(page, text));
    QStringList figures;
    for(const QWidget* figure : page.findChildren<QWidget*>("HyperFluxDevicePercent"))
        figures << figure->accessibleName();
    CHECK(figures.contains("79%") && figures.contains("12%") && figures.contains("–"));
    auto* title = page.findChild<QLabel*>("HyperFluxTitle");
    CHECK(title != nullptr && title->toolTip() == "the bridge closed the connection");
    auto* link = page.findChild<QLabel*>("HyperFluxProjectLink");
    CHECK(link != nullptr && link->text().contains(QString("href=\"%1\"").arg(HFX_PROJECT_URL))
          && link->text().contains(">Project page</a>") && !link->toolTip().startsWith("http")
          && QString(HFX_PROJECT_URL).endsWith(link->toolTip()));
    auto* notice = page.findChild<QLabel*>("HyperFluxNotice");
    CHECK(notice != nullptr && notice->text().contains(">hyperfluxctl doctor --explain</code>"));
    const auto levels = page.findChildren<QWidget*>("HyperFluxDeviceLevel");
    int low = 0;
    for(const QWidget* level : levels)
        low += level->property("low").toBool();
    CHECK(levels.size() == 3 && low == 1);
}

int columns(const QWidget& page)
{
    std::set<int> lefts;
    for(const QWidget* card : page.findChildren<QWidget*>("HyperFluxDevice"))
        lefts.insert(card->x());
    return static_cast<int>(lefts.size());
}

// Every label shows all its text (a wrapping one is tall enough for its
// lines, any other wide enough for its line, a name for what it shows),
// and the page fits across.
int cut_off(const QWidget& page)
{
    int count = 0;
    for(const QLabel* label : page.findChildren<QLabel*>())
    {
        if(!label->isVisibleTo(&page))
            continue;
        const bool fits = label->objectName() == "HyperFluxDeviceName"
                              ? label->fontMetrics().horizontalAdvance(label->text()) <= label->width()
                          : label->wordWrap() ? label->height() >= label->heightForWidth(label->width())
                                              : label->width() >= label->minimumSizeHint().width()
                                                    && label->height() >= label->minimumSizeHint().height();
        if(!fits)
        {
            std::cerr << "cut off: " << label->objectName().toStdString() << " \"" << label->text().toStdString()
                      << "\" in " << label->width() << "x" << label->height() << "\n";
            ++count;
        }
    }
    for(const QWidget* figure : page.findChildren<QWidget*>("HyperFluxDevicePercent"))
        count += figure->width() < figure->sizeHint().width() || figure->height() < figure->sizeHint().height();
    const auto* scroll = page.findChild<QScrollArea*>();
    if(scroll != nullptr && scroll->horizontalScrollBar()->maximum() > 0)
    {
        std::cerr << "the page scrolls sideways\n";
        ++count;
    }
    return count;
}

void check_theme(const Theme& theme, const QString& screens)
{
    QApplication::setPalette(theme.palette);
    qApp->setStyleSheet(theme.sheet);

    QMainWindow window;
    auto* tabs = new QTabWidget();
    tabs->setTabPosition(QTabWidget::West);
    tabs->addTab(new QWidget(), "Software");
    auto* page = new PageWidget([] { return sample(); });
    tabs->addTab(page, "HyperFlux Next V2");
    tabs->setCurrentWidget(page);
    window.setCentralWidget(tabs);
    std::printf("%s\n", theme.name.toUtf8().constData());

    // One column when narrow, two at OpenRGB's usual width, more when wide.
    // Going from tall to short also moves the lower cards from the light
    // end of a gradient to the dark one, and the page has to work their
    // colours out again.
    for(const auto& [size, expected] : std::initializer_list<std::pair<QSize, int>> {
            {QSize(420, 600), 1}, {QSize(1900, 900), 4}, {QSize(1000, 2000), 2}, {QSize(1000, 700), 2}})
    {
        window.resize(size);
        window.show();
        settle();
        shows_the_model(*page);
        if(fonts())
        {
            CHECK(columns(*page) == expected);
            CHECK(cut_off(*page) == 0);
        }
    }

    const QImage shot = window.grab().toImage();
    if(!screens.isEmpty())
        shot.save(QDir(screens).filePath(theme.name + ".png"));
    const auto at = [&](const QWidget* widget, QPoint point) { return shot.pixelColor(widget->mapTo(&window, point)); };

    // Each colour the page works out reaches its target, or as much as
    // black or white could on that background.
    const auto meets = [](double contrast, double target, const QColor& back)
    {
        const double most = std::max(lc(Qt::white, back), lc(Qt::black, back));
        const bool ok = contrast >= std::min(target, most) - 1e-6;
        if(!ok)
            std::cerr << "  Lc " << contrast << " under " << std::min(target, most) << "\n";
        return ok;
    };
    double body = 200, muted = 200, link = 200, figure = 200, fill = 200, icon = 200;
    for(QLabel* label : page->findChildren<QLabel*>())
    {
        const QColor back = PageWidget::backdrop(label);
        // The link and the state beside the name carry their colours in
        // their text.
        const auto colour = QRegularExpression("color: (#[0-9a-f]{6})").match(label->text()).captured(1);
        if(!colour.isEmpty())
        {
            const bool is_link = label->objectName() == "HyperFluxProjectLink";
            double& lowest = is_link ? link : muted;
            lowest = std::min(lowest, lc(QColor(colour), back));
            CHECK(meets(lc(QColor(colour), back), lc::secondary, back));
            if(is_link)
                continue;
        }
        const double contrast = lc(label->palette().color(label->foregroundRole()), back);
        if(!label->property("muted").toBool())
        {
            body = std::min(body, contrast);
            continue;
        }
        muted = std::min(muted, contrast);
        CHECK(meets(contrast, lc::secondary, back));
        // Painted in the colour worked out, not the skin's own text colour.
        // Antialiasing leaves thin strokes a little lighter.
        if(fonts())
        {
            const double strongest = drawn(shot, window, label, back);
            const bool same = strongest <= contrast + 3 && strongest >= contrast - 12;
            if(!same)
                std::cerr << "  " << label->objectName().toStdString() << " drawn at Lc " << strongest
                          << ", worked out " << contrast << "\n";
            CHECK(same);
        }
    }
    for(QWidget* widget : page->findChildren<QWidget*>())
    {
        const QString name = widget->objectName();
        const QColor back = PageWidget::backdrop(widget);
        if(name == "HyperFluxDevice")
        {
            // A surface one step off the window, unless the skin fills the
            // labels themselves, and an edge you can see.
            auto* label = widget->findChild<QLabel*>("HyperFluxDeviceName");
            const bool filled = label != nullptr && label->testAttribute(Qt::WA_StyledBackground)
                                && label->palette().color(QPalette::Window).alpha() == 255;
            const QColor surface = at(widget, QPoint(widget->width() / 2, 3));
            const QColor edge = at(widget, QPoint(widget->width() / 2, 0));
            CHECK((surface != back || filled) && meets(lc(edge, back), lc::edge, back));
            // The space right of a device's name is the card: what the page
            // thinks is behind the name is what is painted there.
            CHECK(label != nullptr && at(label, QPoint(label->width() - 1, label->height() / 2)) == surface);
        }
        else if(name == "HyperFluxDevicePercent" && fonts())
        {
            // Low ones in the low colour and a missing reading's dash muted.
            // The rest are text.
            const double strongest = drawn(shot, window, widget, back);
            if(widget->property("low").toBool() || widget->accessibleName() == QString("–"))
                CHECK(meets(strongest, lc::secondary, back));
            else
                body = std::min(body, strongest);
            figure = std::min(figure, strongest);
        }
        else if(name == "HyperFluxDeviceLevel")
        {
            // The fill as painted, at the middle of its rounded start.
            const int middle = widget->height() / 2;
            const QColor colour = at(widget, QPoint(middle, middle));
            fill = std::min(fill, lc(colour, back));
            CHECK(meets(lc(colour, back), lc::indicator, back));
            // The low one keeps its warm hue, whatever lightness it needed.
            if(widget->property("low").toBool())
                CHECK(colour.hslHue() <= 30);
        }
        else if(name.endsWith("Icon"))
        {
            icon = std::min(icon, drawn(shot, window, widget, back));
            CHECK(meets(drawn(shot, window, widget, back), lc::indicator, back));
        }
    }
    std::printf("  lowest Lc: body %.1f, muted %.1f, link %.1f, figures %.1f, level fill %.1f, icons %.1f\n", body,
                muted, link, figure, fill, icon);
    if(theme.ours)
        CHECK(body >= lc::body && muted >= lc::secondary && muted < body && link >= lc::secondary
              && fill >= lc::indicator);
}

// A skin applied while the page is up, as the Skin plugin does, changes
// the colours it works out.
void follows_a_new_skin()
{
    QApplication::setPalette(QApplication::style()->standardPalette());
    qApp->setStyleSheet({});
    PageWidget page([] { return sample(); });
    page.show();
    settle();
    const auto muted = [&page]
    {
        const QLabel* label = page.findChild<QLabel*>("HyperFluxDeviceDetails");
        return label == nullptr ? QColor() : label->palette().color(label->foregroundRole());
    };
    const QColor before = muted();
    qApp->setStyleSheet(numix_like);
    settle();
    CHECK(muted().isValid() && muted() != before && muted().lightness() > 128);
    qApp->setStyleSheet({});
}

// Inked share of a square the widget's width across.
double coverage(QWidget* widget)
{
    const QImage image = widget->grab().toImage();
    const QColor back = widget->palette().color(widget->backgroundRole());
    int inked = 0;
    for(int y = 0; y < image.height(); ++y)
        for(int x = 0; x < image.width(); ++x)
            inked += std::abs(image.pixelColor(x, y).lightness() - back.lightness()) > 40;
    return static_cast<double>(inked) / (image.width() * image.width());
}

// Under an icon theme whose icons are filled blocks, as YAMIS's charging
// one is, only the device's icon comes from the theme. The state, the note
// and the link keep the page's own line drawings.
void state_icons_are_drawn()
{
    QTemporaryDir themes;
    QDir(themes.path()).mkpath("blocks/22");
    QFile index(themes.filePath("blocks/index.theme"));
    CHECK(index.open(QIODevice::WriteOnly));
    index.write("[Icon Theme]\nName=blocks\nDirectories=22\n\n[22]\nSize=22\nType=Fixed\n");
    index.close();
    QImage block(22, 22, QImage::Format_ARGB32);
    block.fill(Qt::black);
    for(const char* name : {"input-mouse-symbolic", "battery-full-charging-symbolic", "dialog-information-symbolic",
                            "external-link-symbolic"})
        block.save(themes.filePath(QString("blocks/22/%1.png").arg(name)));
    const QStringList paths = QIcon::themeSearchPaths();
    const QString name = QIcon::themeName();
    QIcon::setThemeSearchPaths({themes.path()});
    QIcon::setThemeName("blocks");

    QApplication::setPalette(QApplication::style()->standardPalette());
    qApp->setStyleSheet({});
    PageWidget page([] { return sample(); });
    page.resize(1000, 700);
    page.show();
    settle();
    auto* device = page.findChild<QWidget*>("HyperFluxDeviceIcon");
    CHECK(device != nullptr && coverage(device) > 0.8);
    for(const char* drawn : {"HyperFluxStateIcon", "HyperFluxNoticeIcon", "HyperFluxProjectIcon"})
    {
        auto* icon = page.findChild<QWidget*>(drawn);
        CHECK(icon != nullptr && coverage(icon) < 0.6);
    }
    // At the height of a capital letter beside it.
    auto* state = page.findChild<QWidget*>("HyperFluxStateIcon");
    auto* words = page.findChild<QLabel*>("HyperFluxDeviceState");
    CHECK(state != nullptr && words != nullptr && state->width() == std::max(8, words->fontMetrics().capHeight()));

    QIcon::setThemeSearchPaths(paths);
    QIcon::setThemeName(name);
}

void an_empty_page_says_so()
{
    Page empty {"Connected", "", std::nullopt, {}, Empty {"No devices yet", {"Move or click a paired device.", {}}}};
    PageWidget page([empty] { return empty; });
    CHECK(page.findChildren<QWidget*>("HyperFluxDevice").isEmpty());
    CHECK(has_label(page, "No devices yet") && has_label(page, "Move or click a paired device."));
    auto* title = page.findChild<QLabel*>("HyperFluxTitle");
    CHECK(title != nullptr && title->text().endsWith(">· Connected</span>"));
}

} // namespace

int main(int argc, char** argv)
{
    QApplication application(argc, argv);
    QApplication::setStyle(QStyleFactory::create("Fusion"));
    // The window sizes are meant for a desktop's usual 10 pt. Without a
    // desktop Qt picks its own size, far larger in Debian's Qt 5.
    QFont font = QApplication::font();
    font.setPointSize(10);
    QApplication::setFont(font);
    const QString screens = qEnvironmentVariable("HFX_SCREENS");
    if(!fonts())
        std::printf("no fonts installed: text sizes and painted text not checked\n");

    std::vector<Theme> themes {{"light", QApplication::style()->standardPalette(), {}},
                               {"dark", dark(), {}},
                               {"skin", QApplication::style()->standardPalette(), numix_like},
                               {"gradient", QApplication::style()->standardPalette(), gradient, false}};
    if(const QString path = qEnvironmentVariable("HFX_SKIN"); !path.isEmpty())
    {
        QFile file(path);
        CHECK(file.open(QIODevice::ReadOnly | QIODevice::Text));
        themes.push_back({QFileInfo(path).completeBaseName(), QApplication::style()->standardPalette(),
                          QString::fromUtf8(file.readAll()), false});
    }
    for(const Theme& theme : themes)
        check_theme(theme, screens);
    follows_a_new_skin();
    state_icons_are_drawn();
    an_empty_page_says_so();
    return finish("page widget");
}
