/*
    SPDX-FileCopyrightText: 2026 the makshell contributors

    SPDX-License-Identifier: GPL-3.0-or-later
*/

#pragma once

#include "qwayland-server-makshell-glass-v1.h"

#include <QSizeF>
#include <QElapsedTimer>
#include <QHash>
#include <QList>
#include <QObject>
#include <QPointF>
#include <QPointer>
#include <QRect>
#include <QVector3D>
#include <QVector4D>

#include <optional>

namespace KWin
{
class Display;
class SurfaceInterface;
}

namespace Makshell
{

enum class Style {
    Clear,
    Frosted,
    /// Nothing is drawn; the shape only marks where brightness is measured.
    Transparent,
    Liquid,
    Bubble,
    Control,
    Backdrop,
    Card,
    Chip,
    /// Frosted, for dialogs, which have a lighter tint when they aren't active.
    Window,
    WindowInactive,
    /// Blur behind an app's own window, where it's see-through. It isn't one clients ask for: the
    /// effect puts it there.
    AppWindow,
};

/// How a surface wants to come and go.
struct Appear {
    /// One of the protocol's kinds. 0 means there's no animation.
    int kind = 0;
    /// What it grows out of, in surface-local logical coordinates.
    QPointF origin;
};

struct Shape {
    /// Surface-local logical rectangle.
    QRect rect;
    int radius = 0;
    Style style = Style::Clear;
    /// For a backdrop: how far it fades in from the left, top, right and bottom.
    QVector4D softEdges;
    /// For a shape with a tail: where its tip is from the left, how wide it is and how tall. It's
    /// part of `rect`, whose bottom it takes up.
    QVector3D tail;
    /// For a shape that isn't all of `rect`: its own size, where its middle is from the middle of
    /// `rect`, and how far it's turned clockwise about that, in radians. An empty size means `rect`
    /// is the shape.
    QSizeF box;
    QPointF boxOffset;
    float rotation = 0;
    /// Its corner radius, when it needs to be finer than `radius`. Negative means it doesn't.
    float boxRadius = -1;
    float opacity = 1;
    /// Circles melted into it: where each is from the middle of `rect`, its radius, and how far it
    /// reaches for the shape.
    QList<QVector4D> droplets;
};

/// How many parts of a surface can be blurred at once.
inline constexpr int s_mostContentBlurs = 16;

/// Part of a surface whose own contents are drawn blurred, as they come into view.
struct ContentBlur {
    /// Surface-local logical rectangle.
    QRectF rect;
    /// In logical pixels.
    float radius = 0;
};

class GlassSurface;

/// The makshell_glass_manager_v1 global: hands out a glass object per surface.
class GlassManager : public QObject, public QtWaylandServer::makshell_glass_manager_v1
{
    Q_OBJECT

public:
    explicit GlassManager(KWin::Display *display, QObject *parent = nullptr);

    GlassSurface *glassFor(KWin::SurfaceInterface *surface) const;
    /// Whether the surface's client uses glass, so that the surface is likely to get some.
    bool usesGlass(KWin::SurfaceInterface *surface) const;

Q_SIGNALS:
    /// The surface's glass shapes changed (applied on commit), or its glass went away.
    void glassChanged(KWin::SurfaceInterface *surface);

protected:
    void makshell_glass_manager_v1_destroy(Resource *resource) override;
    void makshell_glass_manager_v1_get_glass(Resource *resource, uint32_t id, struct ::wl_resource *surface) override;

private:
    friend class GlassSurface;
    QHash<KWin::SurfaceInterface *, GlassSurface *> m_surfaces;
};

/// A surface's makshell_glass_v1 object: its double-buffered shapes and brightness tracking.
class GlassSurface : public QObject, public QtWaylandServer::makshell_glass_v1
{
    Q_OBJECT

public:
    GlassSurface(GlassManager *manager, KWin::SurfaceInterface *surface, wl_client *client, uint32_t id, int version);
    ~GlassSurface() override;

    const QList<Shape> &shapes() const;
    const QList<ContentBlur> &contentBlurs() const
    {
        return m_currentBlurs;
    }
    bool tracksBrightness() const;
    Appear appear() const
    {
        return m_appear;
    }

    /// How long until the next brightness measurement is due; 0 when it's due now.
    int msUntilProbe() const;
    /// Takes a measurement, as relative luminance from 0 to 1, and sends it to the client if
    /// it changed noticeably.
    void reportBrightness(float luminance);

protected:
    void makshell_glass_v1_destroy_resource(Resource *resource) override;
    void makshell_glass_v1_destroy(Resource *resource) override;
    void makshell_glass_v1_clear(Resource *resource) override;
    void makshell_glass_v1_add_rounded_rect(Resource *resource, int32_t x, int32_t y, int32_t width, int32_t height, uint32_t radius, uint32_t style) override;
    void makshell_glass_v1_add_backdrop(Resource *resource, int32_t x, int32_t y, int32_t width, int32_t height, uint32_t fade_left, uint32_t fade_top, uint32_t fade_right, uint32_t fade_bottom) override;
    void makshell_glass_v1_add_rounded_rect_with_tail(Resource *resource, int32_t x, int32_t y, int32_t width, int32_t height, uint32_t radius, uint32_t style, int32_t tail_center, uint32_t tail_width, uint32_t tail_height) override;
    void makshell_glass_v1_set_appear(Resource *resource, uint32_t kind, int32_t x, int32_t y) override;
    void makshell_glass_v1_add_rounded_rect_with_top_tail(Resource *resource, int32_t x, int32_t y, int32_t width, int32_t height, uint32_t radius, uint32_t style, int32_t tail_center, uint32_t tail_width, uint32_t tail_height) override;
    void makshell_glass_v1_add_box(Resource *resource, int32_t x, int32_t y, int32_t width, int32_t height, uint32_t style, wl_fixed_t box_x, wl_fixed_t box_y, wl_fixed_t box_width, wl_fixed_t box_height, wl_fixed_t box_radius) override;
    void makshell_glass_v1_set_last_opacity(Resource *resource, wl_fixed_t opacity) override;
    void makshell_glass_v1_add_droplet(Resource *resource, wl_fixed_t x, wl_fixed_t y, wl_fixed_t radius, wl_fixed_t blend) override;
    void makshell_glass_v1_add_rotated_rect(Resource *resource, int32_t x, int32_t y, int32_t width, int32_t height, uint32_t radius, uint32_t style, uint32_t box_width, uint32_t box_height, int32_t millidegrees) override;
    void makshell_glass_v1_track_brightness(Resource *resource) override;
    void makshell_glass_v1_add_content_blur(Resource *resource, int32_t x, int32_t y, int32_t width, int32_t height, wl_fixed_t radius) override;

private:
    void detach();

    QPointer<GlassManager> m_manager;
    QPointer<KWin::SurfaceInterface> m_surface;
    QList<Shape> m_pending;
    QList<Shape> m_current;
    QList<ContentBlur> m_pendingBlurs;
    QList<ContentBlur> m_currentBlurs;
    Appear m_appear;
    bool m_dirty = false;

    bool m_tracking = false;
    QElapsedTimer m_lastProbe;
    /// The last luminance sent, out of 1000.
    std::optional<uint32_t> m_sent;
};

}
