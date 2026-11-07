/*
    SPDX-FileCopyrightText: 2026 the makshell contributors

    SPDX-License-Identifier: GPL-3.0-or-later
*/

#pragma once

#include "effect/effect.h"
#include "effect/effectwindow.h"
#include "glassmanager.h"
#include "opengl/glutils.h"

#include <QElapsedTimer>
#include <QStringList>

#include <memory>
#include <optional>
#include <unordered_map>
#include <vector>

namespace KWin
{
class BackgroundEffectItem;
class RenderView;
class SurfaceInterface;
}

namespace Makshell
{

// From KWin 6.8, painting says whether it worked (it doesn't after a GPU reset); before, it doesn't
// say. A void function can return a void call, so `return effects->drawWindow(...)` suits both.
#if MAKSHELL_KWIN_MINOR >= 8
using PaintResult = bool;
#define MAKSHELL_PAINTED true
#else
using PaintResult = void;
#define MAKSHELL_PAINTED
#endif

class Arranger;
class GlassManager;

/// Look of the glass, from the `Effect-makshell` group of kwinrc. The defaults are measured from
/// Tahoe's menus and dock.
struct GlassSettings {
    /// The blur under liquid glass, cards and backdrops: dual Kawase passes and their sample offset.
    int blurPasses = 2;
    float blurOffset = 2.0;
    /// The much wider blur under frosted glass, where text behind has to disappear completely.
    int frostPasses = 4;
    float frostOffset = 3.0;

    /// Frosted glass, for menus, popovers and alerts: flat and tinted, with a light line that's
    /// mostly the surroundings, inside a dark one.
    float frostSaturation = 1.5;
    float frostTint = 0.58;
    float frostEdgeWhite = 0.12;
    float frostOuterEdge = 0.8;

    /// Liquid glass, for controls like the dock: darkened a little, its edge fading into the
    /// surroundings, a touch brighter.
    float liquidBrightness = 0.86;
    float liquidSaturation = 1.1;
    float liquidEdgeFade = 2.0;
    float liquidEdgeGain = 1.06;

    /// Control modules, like Control Center's capsules, sit on a backdrop that's blurred already.
    /// Measured from Tahoe: a grey veil keeps them legible on anything, taking white to 181 and
    /// black to 31.
    float controlTint = 0.35;
    float controlTintGrey = 0.35;
    float controlSaturation = 1.1;
    float controlRim = 0.88;

    /// Grain against banding, and how much a rim takes after what's behind it.
    float grain = 0.006;
    float rimTint = 0.4;

    /// Glass as a physical slab: its edge curves down, and the background is refracted through that
    /// curve. These values reproduce the bend measured from Tahoe's capsules to within a pixel and a
    /// half, mirrored edge included.
    float glassIor = 1.5;
    float glassIorSpread = 0.02;
    float glassProfile = 2.5;
    float glassShoulder = 1.5;
    float glassGloss = 0.25;
    float glassThickness = 47.5;
    float glassBezel = 5.5;

    /// Backdrops, which set things apart from a busy background, dim it a little as well.
    float backdropBrightness = 0.9;

    /// A dialog that isn't active has a lighter tint than frostTint.
    float inactiveTint = 0.38;

    /// Chips, for small buttons like a notification's ✕: a very wide blur under a light grey tint,
    /// inside a rim that's even all the way around.
    float chipTint = 0.6;
    float chipRim = 0.15;

    /// Bubbles, for small labels like the dock's: tinted like menus, with liquid glass's edge,
    /// dimmed to stay legible.
    float bubbleTint = 0.5;
    float bubbleEdgeGain = 0.85;

    /// How opaque apps' windows are drawn, so that what's behind shows, blurred.
    float windowOpacity = 1.0;
    /// Whether that's only their background, rather than all of them. 1 or 0.
    float windowBackgroundsOnly = 1.0;
};

/// An app's window drawn with only its background see-through. It's drawn by itself first, which is
/// where its background colors are found, and then from that.
struct KeyedWindow {
    ~KeyedWindow();
    std::unique_ptr<KWin::GLTexture> texture;
    std::unique_ptr<KWin::GLFramebuffer> framebuffer;
    /// The window has changed since it was last drawn by itself.
    bool dirty = true;
    /// Its background colors, as they're stored in the texture.
    QList<QVector3D> keys;
    /// It's see-through by itself, like a terminal set up that way, and keeps its own opacity.
    bool clearByItself = false;
    /// When they were last looked for, since they're looked for again now and then.
    QElapsedTimer looked;
    QMetaObject::Connection damaged;
};

/// What one glass shape keeps between frames, per render view.
struct ShapeRender {
    /// Level 0 is the background behind the shape at device resolution, cached: only the
    /// repainted part is copied each frame. Further levels are the blur chain: level 1 ends up
    /// with the blur, and the first level after the blur's own with the very wide one.
    std::vector<std::unique_ptr<KWin::GLTexture>> textures;
    std::vector<std::unique_ptr<KWin::GLFramebuffer>> framebuffers;
};

struct WindowGlass {
    std::unique_ptr<KWin::BackgroundEffectItem> item;
    std::unordered_map<KWin::RenderView *, std::vector<ShapeRender>> render;
    /// A repaint is scheduled for when the next brightness measurement is due.
    bool probeScheduled = false;
    /// Where the glass last was, in global logical coordinates.
    KWin::Rect lastBounds;
    /// The animation for its coming and going.
    Appear appear;
    QElapsedTimer appearing;
    QElapsedTimer disappearing;
    /// Keeps a closed window around for as long as it's disappearing.
    std::optional<KWin::EffectWindowDeletedRef> deletedRef;
    /// The shapes, which outlive the surface, so that glass fades out with a closing window.
    QList<Shape> shapes;
    /// The window's closing. Its glass is fading out with it, and needs repainting all the way.
    bool closing = false;
    /// It's an app's window, whose glass the effect put there, not the app.
    bool automatic = false;
    /// It's drawn less than opaque, so that what's behind shows.
    bool dim = false;
    /// Only its background is, which takes drawing it by itself first.
    std::unique_ptr<KeyedWindow> keyed;
};

/// Glass behind the shapes clients ask for with makshell-glass-v1, the way Tahoe draws it: a flat
/// interior seen through a lens, with an edge that shows the surroundings.
class GlassEffect : public KWin::Effect
{
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.makshell.Glass")

public:
    GlassEffect();
    ~GlassEffect() override;

    static bool supported();

    void reconfigure(ReconfigureFlags flags) override;
    void prePaintScreen(KWin::ScreenPrePaintData &data) override;
    PaintResult paintScreen(const KWin::RenderTarget &renderTarget, const KWin::RenderViewport &viewport, int mask, const KWin::Region &deviceRegion, KWin::LogicalOutput *screen) override;
    void postPaintScreen() override;
    void prePaintWindow(KWin::RenderView *view, KWin::EffectWindow *w, KWin::WindowPrePaintData &data) override;
    PaintResult paintWindow(const KWin::RenderTarget &renderTarget, const KWin::RenderViewport &viewport, KWin::EffectWindow *w, int mask, const KWin::Region &deviceRegion, KWin::WindowPaintData &data) override;
    PaintResult drawWindow(const KWin::RenderTarget &renderTarget, const KWin::RenderViewport &viewport, KWin::EffectWindow *w, int mask, const KWin::Region &deviceRegion, KWin::WindowPaintData &data) override;

    bool isActive() const override;
    bool blocksDirectScanout() const override;

    int requestedEffectChainPosition() const override
    {
        // Right after blur.
        return 21;
    }

public Q_SLOTS:
    /// The current value of a setting the Settings app can change, or 0 if it's not one of those.
    Q_SCRIPTABLE double setting(const QString &key) const;
    /// Changes one, right away. It's saved a moment later, so a slider can be dragged.
    Q_SCRIPTABLE void setSetting(const QString &key, double value);
    /// Puts all of them back to their defaults.
    Q_SCRIPTABLE void resetSettings();
    /// The apps left as they are, with no blur behind and no translucency, by desktop entry id or
    /// window class.
    Q_SCRIPTABLE QStringList exceptions() const;
    Q_SCRIPTABLE void setException(const QString &app, bool excepted);

private:
    void updateWindow(KWin::EffectWindow *w);
    void updateSwitcherGlass(KWin::EffectWindow *w);
    void updateAppGlass(KWin::EffectWindow *w);
    /// Follows what changes whether an app's window gets glass, and how much.
    void watchAppWindow(KWin::EffectWindow *w);
    bool takesAppGlass(KWin::EffectWindow *w) const;
    bool isException(KWin::EffectWindow *w) const;
    /// Draws a window with only its background see-through, or returns false if it can't yet.
    bool drawKeyed(const KWin::RenderTarget &renderTarget, const KWin::RenderViewport &viewport, KWin::EffectWindow *w, const KWin::Region &deviceRegion, const KWin::WindowPaintData &data, KeyedWindow &keyed);
    void findBackgrounds(KeyedWindow &keyed, const KWin::Rect &frame);
    void windowMoved(KWin::EffectWindow *w, const KWin::RectF &oldGeometry);
    void placeDialog(KWin::EffectWindow *w);
    void paintGlass(const KWin::RenderTarget &renderTarget, const KWin::RenderViewport &viewport, KWin::EffectWindow *w, const KWin::Region &deviceRegion, const KWin::WindowPaintData &data);
    bool ensureTextures(ShapeRender &render, const QSize &size, GLenum format, int levels);
    void frost(ShapeRender &render, int passes, float offset);
    float measureLuminance(ShapeRender &render, bool linear);
    QTimer *m_saveSoon = nullptr;
    /// What the screen is being painted to. A window painted to anything else, like the genie's copy
    /// of it, has no background there, so its glass shows what was last seen behind it.
    const KWin::RenderTarget *m_screenTarget = nullptr;

    std::unique_ptr<GlassManager> m_manager;
    std::unique_ptr<Arranger> m_arranger;
    GlassSettings m_settings;
    QStringList m_exceptions;

    std::unique_ptr<KWin::GLShader> m_glassShader;
    std::unique_ptr<KWin::GLShader> m_downsampleShader;
    std::unique_ptr<KWin::GLShader> m_upsampleShader;
    std::unique_ptr<KWin::GLShader> m_keyedShader;
    /// A window's background colors are found in a small copy of it.
    std::unique_ptr<KWin::GLTexture> m_keySampleTexture;
    std::unique_ptr<KWin::GLFramebuffer> m_keySampleFramebuffer;

    /// Brightness probes downsample into this.
    std::unique_ptr<KWin::GLTexture> m_probeTexture;
    std::unique_ptr<KWin::GLFramebuffer> m_probeFramebuffer;

    KWin::RenderView *m_currentView = nullptr;
    std::unordered_map<KWin::EffectWindow *, WindowGlass> m_windows;
    /// New surfaces of clients that use glass, kept out of sight until theirs comes, a moment later,
    /// so that nothing shows without its glass or before its animation has started.
    std::unordered_map<KWin::EffectWindow *, QElapsedTimer> m_awaitingGlass;
    bool m_valid = false;
};

}
