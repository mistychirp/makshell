/*
    SPDX-FileCopyrightText: 2026 the makshell contributors

    SPDX-License-Identifier: GPL-3.0-or-later
*/

#include "decoration.h"

#include <KDecoration3/DecoratedWindow>
#include <KDecoration3/DecorationSettings>
#include <KDecoration3/DecorationShadow>
#include <KDecoration3/ScaleHelpers>
#include <KPluginFactory>

#include <KConfigGroup>
#include <KSharedConfig>

#include <QDBusConnection>
#include <QHoverEvent>
#include <QDBusMessage>
#include <QDBusPendingCall>
#include <QFontDatabase>
#include <QPainter>
#include <QPainterPath>

#include <algorithm>
#include <cmath>
#include <vector>

K_PLUGIN_FACTORY_WITH_JSON(MakshellDecorationFactory, "makshell.json", registerPlugin<Makshell::Decoration>();)

namespace Makshell
{

// Measured from Tahoe's windows, in logical pixels.
static constexpr qreal s_titleHeight = 32;
static constexpr qreal s_cornerRadius = 16;
static constexpr qreal s_dotSize = 14;
static constexpr qreal s_dotSpacing = 23;
/// From the left edge to the middle of the first light.
static constexpr qreal s_dotInset = 16;
/// How far past the window's edges it can be grabbed to resize it.
static constexpr qreal s_resizeBorder = 6;
static constexpr qreal s_titleSize = 13;
/// How much white the rim adds all the way around, measured from Tahoe's windows. KWin blends an
/// outline's color as premultiplied, so that's how it has to be given.
static constexpr int s_rimLevel = 51;
/// How much more it adds along the top, fading out down the corners, for active windows and others.
static constexpr qreal s_topRim = 0.24;
static constexpr qreal s_topRimInactive = 0.12;

static constexpr QColor s_closeColor(255, 92, 93);
static constexpr QColor s_minimizeColor(255, 195, 11);
static constexpr QColor s_zoomColor(55, 200, 81);
static constexpr QColor s_closeInk(110, 14, 16);
static constexpr QColor s_minimizeInk(120, 72, 0);
static constexpr QColor s_zoomInk(8, 82, 24);

// How the shadow falls, measured from Tahoe's alert, the same as the shell's own windows.
static constexpr int s_shadowLeft = 56;
static constexpr int s_shadowTop = 40;
static constexpr int s_shadowRight = 56;
static constexpr int s_shadowBottom = 72;
/// How far the shadow's corners reach into the window, past which its edges are straight.
static constexpr int s_shadowInner = 64;
static constexpr int s_shadowOffsetY = 12;
/// How much fades away at the outermost pixels, where it would otherwise stop abruptly.
static constexpr float s_shadowFade = 8;

static QString titleFontFamily()
{
    static const QString family = [] {
        const int id = QFontDatabase::addApplicationFont(QStringLiteral(":/makshell/decoration/Inter-SemiBold.ttf"));
        const QStringList families = id >= 0 ? QFontDatabase::applicationFontFamilies(id) : QStringList();
        return families.isEmpty() ? QString() : families.first();
    }();
    return family;
}

/// One pass of a box blur along each row, or each column.
static std::vector<float> boxBlur(const std::vector<float> &pixels, int width, int height, int radius, bool horizontal)
{
    const int lines = horizontal ? height : width;
    const int length = horizontal ? width : height;
    const auto at = [&](int line, int along) {
        return horizontal ? line * width + along : along * width + line;
    };
    const float window = 2 * radius + 1;
    std::vector<float> blurred(pixels.size());
    for (int line = 0; line < lines; ++line) {
        float sum = 0;
        for (int along = 0; along <= std::min(radius, length - 1); ++along) {
            sum += pixels[at(line, along)];
        }
        for (int along = 0; along < length; ++along) {
            blurred[at(line, along)] = sum / window;
            if (along + radius + 1 < length) {
                sum += pixels[at(line, along + radius + 1)];
            }
            if (along >= radius) {
                sum -= pixels[at(line, along - radius)];
            }
        }
    }
    return blurred;
}

static float roundedBoxDistance(float x, float y, float left, float top, float width, float height, float radius)
{
    const float qx = std::abs(x - left - width / 2) - width / 2 + radius;
    const float qy = std::abs(y - top - height / 2) - height / 2 + radius;
    return std::min(std::max(qx, qy), 0.0f) + std::hypot(std::max(qx, 0.0f), std::max(qy, 0.0f)) - radius;
}

static std::shared_ptr<KDecoration3::DecorationShadow> makeShadow(bool active)
{
    const float sigma = active ? 20 : 12;
    const float strength = active ? 0.62 : 0.3;

    const int innerWidth = 2 * s_shadowInner + 1;
    const int innerHeight = 2 * s_shadowInner + 1;
    const int width = s_shadowLeft + innerWidth + s_shadowRight;
    const int height = s_shadowTop + innerHeight + s_shadowBottom;

    std::vector<float> pixels(width * height);
    for (int y = 0; y < height; ++y) {
        for (int x = 0; x < width; ++x) {
            const float distance = roundedBoxDistance(x + 0.5f, y + 0.5f - s_shadowOffsetY, s_shadowLeft, s_shadowTop, innerWidth, innerHeight, s_cornerRadius);
            pixels[y * width + x] = std::clamp(0.5f - distance, 0.0f, 1.0f);
        }
    }
    // Three box blurs are as good as a Gaussian.
    const int radius = int(std::sqrt(sigma * sigma * 4 + 1)) / 2;
    for (int pass = 0; pass < 3; ++pass) {
        pixels = boxBlur(pixels, width, height, radius, true);
        pixels = boxBlur(pixels, width, height, radius, false);
    }

    QImage image(width, height, QImage::Format_ARGB32_Premultiplied);
    for (int y = 0; y < height; ++y) {
        auto *line = reinterpret_cast<QRgb *>(image.scanLine(y));
        for (int x = 0; x < width; ++x) {
            const float px = x + 0.5f;
            const float py = y + 0.5f;
            // Nothing where the window is: it's not all opaque.
            const float outside = std::clamp(0.5f + roundedBoxDistance(px, py, s_shadowLeft, s_shadowTop, innerWidth, innerHeight, s_cornerRadius), 0.0f, 1.0f);
            const float border = std::min({px, py, width - px, height - py});
            const float alpha = pixels[y * width + x] * strength * outside * std::min(border / s_shadowFade, 1.0f);
            line[x] = qRgba(0, 0, 0, std::lround(alpha * 255));
        }
    }

    auto shadow = std::make_shared<KDecoration3::DecorationShadow>();
    shadow->setPadding(QMarginsF(s_shadowLeft, s_shadowTop, s_shadowRight, s_shadowBottom));
    shadow->setInnerShadowRect(QRectF(s_shadowLeft + s_shadowInner, s_shadowTop + s_shadowInner, 1, 1));
    shadow->setShadow(image);
    return shadow;
}

Decoration::Decoration(QObject *parent, const QVariantList &args)
    : KDecoration3::Decoration(parent, args)
{
}

bool Decoration::init()
{
    auto window = this->window();
    using DecoratedWindow = KDecoration3::DecoratedWindow;
    const auto redraw = [this] {
        update();
    };
    connect(window, &DecoratedWindow::activeChanged, this, redraw);
    connect(window, &DecoratedWindow::captionChanged, this, redraw);
    connect(window, &DecoratedWindow::paletteChanged, this, redraw);
    connect(window, &DecoratedWindow::activeChanged, this, &Decoration::updateShadow);
    connect(window, &DecoratedWindow::widthChanged, this, &Decoration::updateGeometry);
    connect(window, &DecoratedWindow::maximizedChanged, this, &Decoration::updateGeometry);
    connect(window, &DecoratedWindow::maximizedChanged, this, &Decoration::updateShadow);
    connect(window, &DecoratedWindow::nextScaleChanged, this, &Decoration::updateGeometry);

    m_buttons = new KDecoration3::DecorationButtonGroup(this);
    m_buttons->setSpacing(s_dotSpacing - s_dotSize);
    for (const auto type : {KDecoration3::DecorationButtonType::Close,
                            KDecoration3::DecorationButtonType::Minimize,
                            KDecoration3::DecorationButtonType::Maximize}) {
        auto *button = new Button(type, this, this);
        m_buttons->addButton(button);
        // These don't have tooltips on macOS, and one would get in the way of the green one's menu.
        connect(button, &KDecoration3::DecorationButton::pointerEntered, this, [this] {
            QTimer::singleShot(0, this, [this] {
                requestHideToolTip();
            });
        });
        if (type == KDecoration3::DecorationButtonType::Maximize) {
            m_zoom = button;
            connect(button, &KDecoration3::DecorationButton::hoveredChanged, this, &Decoration::zoomHoverChanged);
            connect(button, &KDecoration3::DecorationButton::pressed, this, [this] {
                zoomHoverChanged(false);
            });
        }
    }

    m_zoomHover.setSingleShot(true);
    m_zoomHover.setInterval(450);
    connect(&m_zoomHover, &QTimer::timeout, this, [this] {
        if (!m_zoom) {
            return;
        }
        // The shell shows the green light's menu. It's not an error if it isn't there.
        const QRectF zoom = m_zoom->geometry();
        auto message = QDBusMessage::createMethodCall(QStringLiteral("org.makshell.Shell"),
                                                      QStringLiteral("/org/makshell/Windows"),
                                                      QStringLiteral("org.makshell.Windows"),
                                                      QStringLiteral("ZoomHovered"));
        message << int(zoom.center().x()) << int(zoom.bottom());
        QDBusConnection::sessionBus().asyncCall(message);
        m_zoomMenuOpen = true;
    });

    updateGeometry();
    updateShadow();
    return true;
}

bool Decoration::buttonsHovered() const
{
    return m_overButtons;
}

void Decoration::setOverButtons(bool over)
{
    if (m_overButtons != over) {
        m_overButtons = over;
        updateButtons();
    }
}

void Decoration::hoverEnterEvent(QHoverEvent *event)
{
    KDecoration3::Decoration::hoverEnterEvent(event);
    setOverButtons(m_buttons && m_buttons->geometry().contains(event->position()));
}

void Decoration::hoverMoveEvent(QHoverEvent *event)
{
    KDecoration3::Decoration::hoverMoveEvent(event);
    setOverButtons(m_buttons && m_buttons->geometry().contains(event->position()));
}

void Decoration::hoverLeaveEvent(QHoverEvent *event)
{
    KDecoration3::Decoration::hoverLeaveEvent(event);
    setOverButtons(false);
}

void Decoration::zoomHoverChanged(bool hovered)
{
    if (hovered) {
        m_zoomHover.start();
        return;
    }
    m_zoomHover.stop();
    if (m_zoomMenuOpen) {
        m_zoomMenuOpen = false;
        const auto message = QDBusMessage::createMethodCall(QStringLiteral("org.makshell.Shell"),
                                                            QStringLiteral("/org/makshell/Windows"),
                                                            QStringLiteral("org.makshell.Windows"),
                                                            QStringLiteral("ZoomLeft"));
        QDBusConnection::sessionBus().asyncCall(message);
    }
}

void Decoration::updateButtons()
{
    if (m_buttons) {
        update(m_buttons->geometry());
    }
}

void Decoration::updateGeometry()
{
    auto window = this->window();
    const qreal scale = window->nextScale();
    const qreal titleHeight = KDecoration3::snapToPixelGrid(s_titleHeight, scale);
    setBorders(QMarginsF(0, titleHeight, 0, 0));
    setTitleBar(QRectF(0, 0, window->width(), titleHeight));

    const bool maximized = window->isMaximized();
    // Just outside the window, all the way around, since there's no border to grab.
    setResizeOnlyBorders(maximized ? QMarginsF() : QMarginsF(s_resizeBorder, s_resizeBorder, s_resizeBorder, s_resizeBorder));
    const KDecoration3::BorderRadius radius(maximized ? 0 : s_cornerRadius);
    setBorderRadius(radius);
    setBorderOutline(maximized ? KDecoration3::BorderOutline()
                               : KDecoration3::BorderOutline(KDecoration3::pixelSize(scale), QColor(s_rimLevel, s_rimLevel, s_rimLevel, s_rimLevel), radius));
    setOpaque(maximized);

    if (m_buttons) {
        const qreal first = s_dotInset - s_dotSize / 2;
        m_buttons->setPos(QPointF(first, (titleHeight - s_dotSize) / 2));
        for (auto *button : m_buttons->buttons()) {
            button->setGeometry(QRectF(button->geometry().topLeft(), QSizeF(s_dotSize, s_dotSize)));
        }
        // The group lays out by each button's size, which only just got set.
        m_buttons->setSpacing(s_dotSpacing - s_dotSize);
    }
    update();
}

void Decoration::updateShadow()
{
    if (window()->isMaximized()) {
        setShadow(nullptr);
        return;
    }
    static const auto activeShadow = makeShadow(true);
    static const auto inactiveShadow = makeShadow(false);
    setShadow(window()->isActive() ? activeShadow : inactiveShadow);
}

void Decoration::paint(QPainter *painter, const QRectF &repaintArea)
{
    auto window = this->window();
    const QRectF bar = titleBar();
    const QColor background = window->palette().color(QPalette::Window);
    const bool light = background.lightnessF() > 0.5;

    painter->save();
    painter->setRenderHint(QPainter::Antialiasing);
    painter->setPen(Qt::NoPen);
    painter->setBrush(background);
    // Square bottom corners; only the window as a whole is rounded, by its border radius.
    QPainterPath path;
    const qreal radius = window->isMaximized() ? 0 : s_cornerRadius;
    path.moveTo(bar.left(), bar.bottom());
    path.lineTo(bar.left(), bar.top() + radius);
    path.quadTo(bar.topLeft(), QPointF(bar.left() + radius, bar.top()));
    path.lineTo(bar.right() - radius, bar.top());
    path.quadTo(bar.topRight(), QPointF(bar.right(), bar.top() + radius));
    path.lineTo(bar.bottomRight());
    path.closeSubpath();
    painter->drawPath(path);

    // The top catches more light than the rest of the rim, fading out as it curves away.
    if (!window->isMaximized()) {
        const qreal top = window->isActive() ? s_topRim : s_topRimInactive;
        QLinearGradient light(0, bar.top(), 0, bar.top() + s_cornerRadius);
        light.setColorAt(0, QColor::fromRgbF(1, 1, 1, top));
        light.setColorAt(1, QColor::fromRgbF(1, 1, 1, 0));
        const qreal inset = KDecoration3::pixelSize(window->nextScale()) / 2;
        QPainterPath edge;
        const qreal left = bar.left() + inset;
        const qreal right = bar.right() - inset;
        const qreal y = bar.top() + inset;
        edge.moveTo(left, y + s_cornerRadius);
        edge.quadTo(left, y, left + s_cornerRadius, y);
        edge.lineTo(right - s_cornerRadius, y);
        edge.quadTo(right, y, right, y + s_cornerRadius);
        painter->setBrush(Qt::NoBrush);
        painter->setPen(QPen(QBrush(light), KDecoration3::pixelSize(window->nextScale())));
        painter->drawPath(edge);
        painter->setPen(Qt::NoPen);
    }

    // The line between the bar and what's under it.
    painter->setBrush(QColor(0, 0, 0, light ? 30 : 90));
    const qreal line = KDecoration3::pixelSize(window->nextScale());
    painter->drawRect(QRectF(bar.left(), bar.bottom() - line, bar.width(), line));

    // The title, in the middle of the window unless the lights are in the way.
    QFont font(titleFontFamily());
    font.setPixelSize(s_titleSize);
    font.setWeight(QFont::DemiBold);
    painter->setFont(font);
    QColor ink = light ? QColor(0, 0, 0) : QColor(255, 255, 255);
    ink.setAlphaF(window->isActive() ? 0.85 : 0.45);
    painter->setPen(ink);
    const qreal lightsEnd = s_dotInset + 2 * s_dotSpacing + s_dotSize;
    const qreal room = bar.width() - 2 * lightsEnd;
    if (room > 0) {
        const QRectF titleRect(lightsEnd, bar.top(), room, bar.height());
        const QString title = painter->fontMetrics().elidedText(window->caption(), Qt::ElideMiddle, int(room));
        painter->drawText(titleRect, Qt::AlignCenter | Qt::TextSingleLine, title);
    }
    painter->restore();

    if (m_buttons) {
        m_buttons->paint(painter, repaintArea);
    }
}

Button::Button(KDecoration3::DecorationButtonType type, Decoration *decoration, QObject *parent)
    : KDecoration3::DecorationButton(type, decoration, parent)
{
    setGeometry(QRectF(0, 0, s_dotSize, s_dotSize));
    if (type == KDecoration3::DecorationButtonType::Maximize) {
        setAcceptedButtons(Qt::LeftButton);
        // Its click either maximizes, as KDecoration has it, or goes full screen, as the Settings
        // app chooses. The check is here since that can change at any time.
        disconnect(this, &KDecoration3::DecorationButton::clicked, nullptr, nullptr);
        connect(this, &KDecoration3::DecorationButton::clicked, decoration, [decoration](Qt::MouseButton button) {
            const KConfigGroup group = KSharedConfig::openConfig(QStringLiteral("kwinrc"))->group(QStringLiteral("Effect-makshell"));
            if (group.readEntry("GreenFullScreen", 0.0) == 0.0) {
                decoration->requestToggleMaximization(button);
                return;
            }
            const auto message = QDBusMessage::createMethodCall(QStringLiteral("org.kde.KWin"),
                                                                QStringLiteral("/org/makshell/Arranger"),
                                                                QStringLiteral("org.makshell.Arranger"),
                                                                QStringLiteral("fullScreenUnderCursor"));
            QDBusConnection::sessionBus().asyncCall(message);
        }, Qt::QueuedConnection);
    }
}

void Button::paint(QPainter *painter, const QRectF &repaintArea)
{
    Q_UNUSED(repaintArea)
    auto *owner = static_cast<Decoration *>(decoration());
    const bool hovered = owner->buttonsHovered();
    const bool lit = isEnabled() && (hovered || owner->window()->isActive());

    QColor color;
    QColor ink;
    switch (type()) {
    case KDecoration3::DecorationButtonType::Close:
        color = s_closeColor;
        ink = s_closeInk;
        break;
    case KDecoration3::DecorationButtonType::Minimize:
        color = s_minimizeColor;
        ink = s_minimizeInk;
        break;
    default:
        color = s_zoomColor;
        ink = s_zoomInk;
        break;
    }
    if (isPressed()) {
        color = color.darker(120);
    }

    const QRectF rect = geometry();
    painter->save();
    painter->setRenderHint(QPainter::Antialiasing);
    painter->setPen(Qt::NoPen);
    if (lit) {
        painter->setBrush(color);
    } else {
        // Measured from Tahoe: a see-through grey, whatever's behind it.
        const bool light = owner->window()->palette().color(QPalette::Window).lightnessF() > 0.5;
        painter->setBrush(light ? QColor(0, 0, 0, 40) : QColor(255, 255, 255, 44));
    }
    painter->drawEllipse(rect);

    if (lit && hovered) {
        const QPointF center = rect.center();
        const qreal size = rect.width();
        QPen pen(ink, size * 0.11, Qt::SolidLine, Qt::RoundCap);
        switch (type()) {
        case KDecoration3::DecorationButtonType::Close: {
            const qreal reach = size * 0.2;
            painter->setPen(pen);
            painter->drawLine(center + QPointF(-reach, -reach), center + QPointF(reach, reach));
            painter->drawLine(center + QPointF(-reach, reach), center + QPointF(reach, -reach));
            break;
        }
        case KDecoration3::DecorationButtonType::Minimize: {
            const qreal reach = size * 0.27;
            painter->setPen(pen);
            painter->drawLine(center + QPointF(-reach, 0), center + QPointF(reach, 0));
            break;
        }
        default: {
            // Two triangles on the diagonal, pointing away from each other.
            painter->setPen(Qt::NoPen);
            painter->setBrush(ink);
            const qreal far = size * 0.27;
            const qreal near = size * 0.03;
            const QPointF up[3] = {center + QPointF(-far, -far), center + QPointF(far - near * 2, -far), center + QPointF(-far, far - near * 2)};
            const QPointF down[3] = {center + QPointF(far, far), center + QPointF(-far + near * 2, far), center + QPointF(far, -far + near * 2)};
            painter->drawConvexPolygon(up, 3);
            painter->drawConvexPolygon(down, 3);
            break;
        }
        }
    }
    painter->restore();
}

}

#include "decoration.moc"
