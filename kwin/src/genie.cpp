/*
    SPDX-FileCopyrightText: 2026 the makshell contributors

    SPDX-License-Identifier: GPL-3.0-or-later
*/

#include "genie.h"

#include "effect/effecthandler.h"
#include "effect/effectwindow.h"
#include "scene/itemgeometry.h"

#include <QDBusConnection>

#include <algorithm>
#include <chrono>

using namespace KWin;
using namespace std::chrono_literals;

namespace Makshell
{

KWIN_EFFECT_FACTORY_SUPPORTED(GenieEffect, "genie.json", return GenieEffect::supported();)

static const QString s_path = QStringLiteral("/org/makshell/Genie");
static constexpr auto s_duration = 450ms;
/// How small the squares of the grid are, in logical pixels: smaller is a smoother curve.
static constexpr int s_gridSize = 24;

GenieEffect::GenieEffect()
{
    connect(effects, &EffectsHandler::windowAdded, this, &GenieEffect::windowAdded);
    connect(effects, &EffectsHandler::windowDeleted, this, [this](EffectWindow *w) {
        m_animations.remove(w);
    });
    for (EffectWindow *w : effects->stackingOrder()) {
        windowAdded(w);
    }
    setVertexSnappingMode(RenderGeometry::VertexSnappingMode::None);
    QDBusConnection::sessionBus().registerObject(s_path, this, QDBusConnection::ExportScriptableSlots);
}

GenieEffect::~GenieEffect()
{
    QDBusConnection::sessionBus().unregisterObject(s_path);
}

bool GenieEffect::supported()
{
    return OffscreenEffect::supported() && effects->animationsSupported();
}

void GenieEffect::setTarget(const QString &id, int x, int y, int width, int height)
{
    const QRect rect(x, y, width, height);
    if (id.isEmpty()) {
        m_nextTarget = rect;
    } else {
        m_targets.insert(QUuid(id), rect);
    }
}

void GenieEffect::skip(const QString &id)
{
    m_skipped.insert(QUuid(id));
}

void GenieEffect::windowAdded(EffectWindow *w)
{
    connect(w, &EffectWindow::minimizedChanged, this, [this, w]() {
        animate(w, w->isMinimized());
    });
}

QRectF GenieEffect::targetFor(EffectWindow *w) const
{
    if (const auto it = m_targets.constFind(w->internalId()); it != m_targets.constEnd()) {
        return QRectF(*it);
    }
    if (m_nextTarget.isValid()) {
        return QRectF(m_nextTarget);
    }
    // Without a shell to say, the middle of the bottom of the screen.
    const RectF area = effects->clientArea(ScreenArea, w);
    return QRectF(area.center().x() - 24, area.bottom() - 48, 48, 48);
}

void GenieEffect::animate(EffectWindow *w, bool minimizing)
{
    if (minimizing ? m_skipped.contains(w->internalId()) : m_skipped.remove(w->internalId())) {
        return;
    }
    if (effects->activeFullScreenEffect()) {
        return;
    }
    Animation &animation = m_animations[w];
    const TimeLine::Direction direction = minimizing ? TimeLine::Forward : TimeLine::Backward;
    if (animation.timeLine.running()) {
        animation.timeLine.setDirection(direction);
    } else {
        animation.visibleRef = EffectWindowVisibleRef(w, EffectWindow::PAINT_DISABLED_BY_MINIMIZE);
        animation.target = targetFor(w);
        // Once it's back, its tile is gone. Next time, it goes where the next one goes.
        if (!minimizing) {
            m_targets.remove(w->internalId());
        }
        animation.timeLine.setDirection(direction);
        animation.timeLine.setDuration(animationTime(s_duration));
        animation.timeLine.setEasingCurve(QEasingCurve::Linear);
    }
    // Which way it's going, for other effects: the glass leaves a window alone on its way.
    const QVariant grabbed = QVariant::fromValue(static_cast<void *>(this));
    w->setData(WindowMinimizedGrabRole, minimizing ? grabbed : QVariant());
    w->setData(WindowUnminimizedGrabRole, minimizing ? QVariant() : grabbed);
    redirect(w);
    effects->addRepaintFull();
}

void GenieEffect::prePaintScreen(ScreenPrePaintData &data)
{
    // Otherwise, only part of the screen would repaint, and it would leave a trail.
    data.mask |= PAINT_SCREEN_WITH_TRANSFORMED_WINDOWS;
    effects->prePaintScreen(data);
}

void GenieEffect::prePaintWindow(RenderView *view, EffectWindow *w, WindowPrePaintData &data)
{
    if (auto it = m_animations.find(w); it != m_animations.end()) {
        it->timeLine.advance(view);
        data.setTransformed();
    }
    effects->prePaintWindow(view, w, data);
}

static float smoothstep(float from, float to, float value)
{
    const float t = std::clamp((value - from) / (to - from), 0.0f, 1.0f);
    return t * t * (3 - 2 * t);
}

void GenieEffect::apply(EffectWindow *w, int mask, WindowPaintData &data, WindowQuadList &quads)
{
    Q_UNUSED(mask)
    const auto it = m_animations.constFind(w);
    if (it == m_animations.constEnd()) {
        return;
    }
    const float progress = it->timeLine.value();
    const RectF window = w->frameGeometry();
    const QRectF target = it->target;

    // The way the curve goes is adapted from MacShell's genie (https://github.com/0-ss/MacShell, MIT;
    // see LICENSES/MacShell-MIT.txt). First
    // the sides bend toward the dock, most near the bottom. Then every row falls into it, the bottom
    // rows leading, and how narrow a row is depends on how close to the dock it's got.
    const float bend = smoothstep(0.0f, 0.45f, progress);
    const float fall = smoothstep(0.25f, 1.0f, progress);
    const float span = std::max(1.0f, float(target.bottom() - window.y()));

    const auto place = [&](WindowVertex &vertex) {
        const float across = vertex.x() / window.width();
        const float down = vertex.y() / window.height();

        float row = std::clamp(fall * 1.35f - (1.0f - down) * 0.35f, 0.0f, 1.0f);
        row = row * row * (3 - 2 * row);
        const float fromY = window.y() + vertex.y();
        const float toY = target.y() + down * target.height();
        const float y = fromY + (toY - fromY) * row;

        const float near = std::clamp((y - float(window.y())) / span, 0.0f, 1.0f);
        const float squeeze = std::clamp(bend * smoothstep(0.0f, 1.0f, near) + row, 0.0f, 1.0f);
        const float left = window.x() + (target.x() - window.x()) * squeeze;
        const float right = window.right() + (target.right() - window.right()) * squeeze;

        vertex.setX(left + (right - left) * across - window.x());
        vertex.setY(y - window.y());
    };

    quads = quads.makeGrid(s_gridSize);
    for (WindowQuad &quad : quads) {
        for (int i = 0; i < 4; ++i) {
            place(quad[i]);
        }
    }
    // The tip fades on its way in, so it never ends in a hard sliver.
    data.multiplyOpacity(1.0 - smoothstep(0.82f, 1.0f, progress) * 0.85);
}

void GenieEffect::postPaintScreen()
{
    for (auto it = m_animations.begin(); it != m_animations.end();) {
        if (it->timeLine.done()) {
            it.key()->setData(WindowMinimizedGrabRole, QVariant());
            it.key()->setData(WindowUnminimizedGrabRole, QVariant());
            unredirect(it.key());
            it = m_animations.erase(it);
        } else {
            ++it;
        }
    }
    effects->addRepaintFull();
    effects->postPaintScreen();
}

bool GenieEffect::isActive() const
{
    return !m_animations.isEmpty() && !effects->isScreenLocked();
}

}

#include "genie.moc"
