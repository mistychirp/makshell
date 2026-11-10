/*
    SPDX-FileCopyrightText: 2026 the makshell contributors

    SPDX-License-Identifier: GPL-3.0-or-later
*/

#include "glassmanager.h"

#include "wayland/clientconnection.h"

#include "wayland/display.h"
#include "wayland/surface.h"

#include <algorithm>
#include <numbers>
#include <algorithm>

namespace Makshell
{

static constexpr int s_version = 12;

/// Reading pixels back stalls the GPU, so brightness is measured at most this often.
static constexpr int s_probeIntervalMs = 500;
/// How much the luminance (out of 1000) has to change before the client hears about it.
static constexpr uint32_t s_brightnessStep = 20;

GlassManager::GlassManager(KWin::Display *display, QObject *parent)
    : QObject(parent)
    , QtWaylandServer::makshell_glass_manager_v1(*display, s_version)
{
}

GlassSurface *GlassManager::glassFor(KWin::SurfaceInterface *surface) const
{
    return m_surfaces.value(surface);
}

bool GlassManager::usesGlass(KWin::SurfaceInterface *surface) const
{
    return resourceMap().contains(surface->client()->client());
}

void GlassManager::makshell_glass_manager_v1_destroy(Resource *resource)
{
    wl_resource_destroy(resource->handle);
}

void GlassManager::makshell_glass_manager_v1_get_glass(Resource *resource, uint32_t id, struct ::wl_resource *surface)
{
    KWin::SurfaceInterface *surf = KWin::SurfaceInterface::get(surface);
    if (m_surfaces.contains(surf)) {
        wl_resource_post_error(resource->handle, error_glass_exists, "the surface already has a glass object");
        return;
    }
    m_surfaces.insert(surf, new GlassSurface(this, surf, resource->client(), id, resource->version()));
}

GlassSurface::GlassSurface(GlassManager *manager, KWin::SurfaceInterface *surface, wl_client *client, uint32_t id, int version)
    : QtWaylandServer::makshell_glass_v1(client, id, version)
    , m_manager(manager)
    , m_surface(surface)
{
    // Shapes are double-buffered: they take effect on the surface's next commit.
    connect(surface, &KWin::SurfaceInterface::committed, this, [this]() {
        if (!m_dirty) {
            return;
        }
        m_dirty = false;
        m_current = m_pending;
        m_currentBlurs = m_pendingBlurs;
        if (m_manager && m_surface) {
            Q_EMIT m_manager->glassChanged(m_surface);
        }
    });
    // Inert once the surface is gone. (The QPointer is already null when this fires.)
    connect(surface, &QObject::destroyed, this, [this, surface]() {
        if (m_manager) {
            m_manager->m_surfaces.remove(surface);
        }
    });
}

GlassSurface::~GlassSurface()
{
    detach();
}

void GlassSurface::detach()
{
    if (m_manager && m_surface) {
        m_manager->m_surfaces.remove(m_surface);
        m_current.clear();
        Q_EMIT m_manager->glassChanged(m_surface);
    }
    m_surface = nullptr;
}

const QList<Shape> &GlassSurface::shapes() const
{
    return m_current;
}

bool GlassSurface::tracksBrightness() const
{
    return m_tracking;
}

int GlassSurface::msUntilProbe() const
{
    if (!m_lastProbe.isValid()) {
        return 0;
    }
    return std::max<qint64>(0, s_probeIntervalMs - m_lastProbe.elapsed());
}

void GlassSurface::reportBrightness(float luminance)
{
    m_lastProbe.start();
    const uint32_t value = std::clamp(luminance, 0.0f, 1.0f) * 1000.0f + 0.5f;
    if (m_sent && std::abs(int(*m_sent) - int(value)) < int(s_brightnessStep)) {
        return;
    }
    m_sent = value;
    send_brightness(value);
}

void GlassSurface::makshell_glass_v1_destroy_resource(Resource *resource)
{
    Q_UNUSED(resource)
    delete this;
}

void GlassSurface::makshell_glass_v1_destroy(Resource *resource)
{
    detach();
    wl_resource_destroy(resource->handle);
}

void GlassSurface::makshell_glass_v1_clear(Resource *resource)
{
    Q_UNUSED(resource)
    m_pending.clear();
    m_pendingBlurs.clear();
    m_dirty = true;
}

void GlassSurface::makshell_glass_v1_add_content_blur(Resource *resource, int32_t x, int32_t y, int32_t width, int32_t height, wl_fixed_t radius)
{
    Q_UNUSED(resource)
    const float logical = wl_fixed_to_double(radius);
    // A handful is plenty, and all that's drawn.
    if (width <= 0 || height <= 0 || logical <= 0 || m_pendingBlurs.size() >= s_mostContentBlurs) {
        return;
    }
    m_pendingBlurs.append(ContentBlur{.rect = QRectF(x, y, width, height), .radius = std::min(logical, 64.0f)});
    m_dirty = true;
}

void GlassSurface::makshell_glass_v1_add_rounded_rect(Resource *resource, int32_t x, int32_t y, int32_t width, int32_t height, uint32_t radius, uint32_t style)
{
    if (width <= 0 || height <= 0) {
        wl_resource_post_error(resource->handle, error_invalid_shape, "glass shapes need a positive width and height");
        return;
    }
    Style shapeStyle = Style::Clear;
    if (style == style_frosted) {
        shapeStyle = Style::Frosted;
    } else if (style == style_transparent) {
        shapeStyle = Style::Transparent;
    } else if (style == style_liquid) {
        shapeStyle = Style::Liquid;
    } else if (style == style_bubble) {
        shapeStyle = Style::Bubble;
    } else if (style == style_control) {
        shapeStyle = Style::Control;
    } else if (style == style_card) {
        shapeStyle = Style::Card;
    } else if (style == style_chip) {
        shapeStyle = Style::Chip;
    } else if (style == style_window) {
        shapeStyle = Style::Window;
    } else if (style == style_window_inactive) {
        shapeStyle = Style::WindowInactive;
    }
    m_pending.append(Shape{
        .rect = QRect(x, y, width, height),
        .radius = int(std::min<uint32_t>(radius, std::min(width, height) / 2)),
        .style = shapeStyle,
    });
    m_dirty = true;
}

void GlassSurface::makshell_glass_v1_add_rounded_rect_with_tail(Resource *resource, int32_t x, int32_t y, int32_t width, int32_t height, uint32_t radius, uint32_t style, int32_t tail_center, uint32_t tail_width, uint32_t tail_height)
{
    makshell_glass_v1_add_rounded_rect(resource, x, y, width, height, radius, style);
    if (!m_pending.isEmpty() && tail_height < uint32_t(height)) {
        m_pending.last().tail = QVector3D(tail_center, tail_width, tail_height);
        // The radius is clamped against what's left above the tail.
        const int body = std::min(width, height - int(tail_height));
        m_pending.last().radius = std::min<int>(radius, body / 2);
    }
}

void GlassSurface::makshell_glass_v1_add_rounded_rect_with_top_tail(Resource *resource, int32_t x, int32_t y, int32_t width, int32_t height, uint32_t radius, uint32_t style, int32_t tail_center, uint32_t tail_width, uint32_t tail_height)
{
    makshell_glass_v1_add_rounded_rect_with_tail(resource, x, y, width, height, radius, style, tail_center, tail_width, tail_height);
    if (!m_pending.isEmpty() && tail_height < uint32_t(height)) {
        // Negative means on top.
        m_pending.last().tail.setZ(-float(tail_height));
    }
}

void GlassSurface::makshell_glass_v1_add_rotated_rect(Resource *resource, int32_t x, int32_t y, int32_t width, int32_t height, uint32_t radius, uint32_t style, uint32_t box_width, uint32_t box_height, int32_t millidegrees)
{
    const qsizetype before = m_pending.size();
    makshell_glass_v1_add_rounded_rect(resource, x, y, width, height, radius, style);
    if (m_pending.size() > before) {
        Shape &shape = m_pending.last();
        shape.box = QSizeF(box_width, box_height);
        shape.rotation = float(millidegrees) / 1000.0f * std::numbers::pi_v<float> / 180.0f;
        // The rectangle given is only what holds it.
        shape.radius = std::min<int>(radius, std::min(box_width, box_height) / 2);
    }
}

void GlassSurface::makshell_glass_v1_add_box(Resource *resource, int32_t x, int32_t y, int32_t width, int32_t height, uint32_t style, wl_fixed_t box_x, wl_fixed_t box_y, wl_fixed_t box_width, wl_fixed_t box_height, wl_fixed_t box_radius)
{
    const qsizetype before = m_pending.size();
    makshell_glass_v1_add_rounded_rect(resource, x, y, width, height, 0, style);
    if (m_pending.size() > before) {
        Shape &shape = m_pending.last();
        shape.box = QSizeF(wl_fixed_to_double(box_width), wl_fixed_to_double(box_height));
        shape.boxOffset = QPointF(wl_fixed_to_double(box_x) - width / 2.0, wl_fixed_to_double(box_y) - height / 2.0);
        shape.boxRadius = std::min(wl_fixed_to_double(box_radius), std::min(shape.box.width(), shape.box.height()) / 2);
    }
}

void GlassSurface::makshell_glass_v1_add_droplet(Resource *resource, wl_fixed_t x, wl_fixed_t y, wl_fixed_t radius, wl_fixed_t blend)
{
    Q_UNUSED(resource)
    if (m_pending.isEmpty() || m_pending.last().droplets.size() >= 4) {
        return;
    }
    Shape &shape = m_pending.last();
    shape.droplets.append(QVector4D(wl_fixed_to_double(x) - shape.rect.width() / 2.0,
                                    wl_fixed_to_double(y) - shape.rect.height() / 2.0,
                                    wl_fixed_to_double(radius),
                                    wl_fixed_to_double(blend)));
}

void GlassSurface::makshell_glass_v1_set_last_opacity(Resource *resource, wl_fixed_t opacity)
{
    Q_UNUSED(resource)
    if (!m_pending.isEmpty()) {
        m_pending.last().opacity = std::clamp(wl_fixed_to_double(opacity), 0.0, 1.0);
    }
}

void GlassSurface::makshell_glass_v1_set_appear(Resource *resource, uint32_t kind, int32_t x, int32_t y)
{
    Q_UNUSED(resource)
    m_appear = Appear{.kind = int(kind), .origin = QPointF(x, y)};
}

void GlassSurface::makshell_glass_v1_add_backdrop(Resource *resource, int32_t x, int32_t y, int32_t width, int32_t height, uint32_t fade_left, uint32_t fade_top, uint32_t fade_right, uint32_t fade_bottom)
{
    if (width <= 0 || height <= 0) {
        wl_resource_post_error(resource->handle, error_invalid_shape, "glass shapes need a positive width and height");
        return;
    }
    m_pending.append(Shape{
        .rect = QRect(x, y, width, height),
        .radius = 0,
        .style = Style::Backdrop,
        .softEdges = QVector4D(fade_left, fade_top, fade_right, fade_bottom),
    });
    m_dirty = true;
}

void GlassSurface::makshell_glass_v1_track_brightness(Resource *resource)
{
    Q_UNUSED(resource)
    m_tracking = true;
}

}
