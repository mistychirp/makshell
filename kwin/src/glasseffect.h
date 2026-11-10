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
#include <QVector3D>

#include <memory>
#include <optional>
#include <unordered_map>
#include <unordered_set>
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
    /// Running since it last moved, was resized or was animated by another effect, like KWin's
    /// maximize: all changes the window doesn't damage itself for. Until it's settled, it's drawn
    /// by itself every frame, and its backgrounds aren't looked for in the frames in between.
    QElapsedTimer settling;
    /// Where it was, to tell when that changes.
    KWin::RectF lastFrame;
    /// How far KWin's cross-fade from its old picture to its new one had got, like maximizing's.
    qreal crossFade = 0;
    /// Its background colors, as they're stored in the texture.
    QList<QVector3D> keys;
    /// It's see-through by itself, like a terminal set up that way, and keeps its own opacity.
    bool clearByItself = false;
    /// When they were last looked for, since they're looked for again now and then.
    QElapsedTimer looked;
    QMetaObject::Connection damaged;
};

/// A window's own contents drawn by themselves, while parts of them are drawn blurred.
struct BlurredContents {
    /// With smaller copies of itself, which the blur is read from.
    std::unique_ptr<KWin::GLTexture> texture;
    std::unique_ptr<KWin::GLFramebuffer> framebuffer;
    /// How many copies there are, the texture itself included.
    int levels = 1;
};

/// What one glass shape keeps between frames, per render view.
struct ShapeRender {
    ShapeRender() = default;
    // So that a vector of them moves them rather than copying.
    ShapeRender(ShapeRender &&) noexcept = default;
    ShapeRender &operator=(ShapeRender &&) noexcept = default;

    /// Level 0 is the background behind the shape at device resolution, cached: only the
    /// repainted part is copied each frame. Further levels are the blur chain: level 1 ends up
    /// with the blur, and the first level after the blur's own with the very wide one.
    std::vector<std::unique_ptr<KWin::GLTexture>> textures;
    std::vector<std::unique_ptr<KWin::GLFramebuffer>> framebuffers;
    /// The part of level 0 that's been copied since the textures were made, relative to the shape.
    KWin::Region filled;
    /// Where the shape was, in device pixels, when it was last blurred.
    KWin::Rect blurredAt;
    /// When it was last blurred.
    QElapsedTimer blurred;
};

/// How a window was last drawn on one view, to tell whether it looks any different now.
struct WindowLook {
    KWin::RectF geometry;
    qreal opacity = 1;
    qreal brightness = 1;
    qreal saturation = 1;
    qreal xScale = 1;
    qreal yScale = 1;
    QVector3D translation;
    /// The last damage that's been drawn.
    quint64 damage = 0;
    /// The last change to its glass that's been drawn.
    quint64 glass = 0;

    bool sameAs(const WindowLook &other) const
    {
        return geometry == other.geometry && opacity == other.opacity && brightness == other.brightness
            && saturation == other.saturation && xScale == other.xScale && yScale == other.yScale
            && translation == other.translation;
    }
};

/// What's known about one view's last frame, to tell what's changed on it since.
struct ViewHistory {
    std::unordered_map<KWin::EffectWindow *, WindowLook> looks;
    /// The windows that were animated, which may have been drawn anywhere.
    std::unordered_set<KWin::EffectWindow *> animated;
    /// The last change to everything that's been drawn.
    quint64 everything = 0;
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
    /// The parts of its own contents that are out of focus for now.
    QList<ContentBlur> contentBlurs;
    /// There while any of it is.
    std::unique_ptr<BlurredContents> blurred;
    /// The window's closing. Its glass is fading out with it, and needs repainting all the way.
    bool closing = false;
    /// It's an app's window, whose glass the effect put there, not the app.
    bool automatic = false;
    /// It's drawn less than opaque, so that what's behind shows.
    bool dim = false;
    /// Only its background is, which takes drawing it by itself first.
    std::unique_ptr<KeyedWindow> keyed;
    /// When its shapes last changed, counted along with windows' damage.
    quint64 changed = 0;
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
    /// Draws a window's contents with parts of them blurred, and all of them by `everywhere` logical
    /// pixels, or returns false if it can't.
    bool drawBlurred(const KWin::RenderTarget &renderTarget, const KWin::RenderViewport &viewport, KWin::EffectWindow *w, const KWin::Region &deviceRegion, const KWin::WindowPaintData &data, WindowGlass &glass, float everywhere);
    void windowMoved(KWin::EffectWindow *w, const KWin::RectF &oldGeometry);
    void placeDialog(KWin::EffectWindow *w);
    /// Draws a window's glass, blurring what's behind it again if that may have changed. Returns
    /// whether it did.
    bool paintGlass(const KWin::RenderTarget &renderTarget, const KWin::RenderViewport &viewport, KWin::EffectWindow *w, const KWin::Region &deviceRegion, const KWin::WindowPaintData &data, bool behindChanged);
    /// Whether what's behind a window may have changed since the last frame on this view.
    bool behindChanged(KWin::EffectWindow *w) const;
    /// Notes how a window is drawn this frame, and what it may have changed on screen.
    void noteDrawn(KWin::EffectWindow *w, const KWin::WindowPaintData &data, bool reblurred);
    /// Something changed what's on screen everywhere, or in a way that isn't followed.
    void changedEverything();
    /// A minimize animation is moving the window, which goes without glass on its way, like Tahoe's.
    static bool isMinimizing(KWin::EffectWindow *w);
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
    std::unique_ptr<KWin::GLShader> m_blurredShader;
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

    // Glass is only blurred again when what's behind it may have changed, which is followed from
    // what's drawn below it: any window that's damaged, moved, faded or animated.
    std::unordered_map<KWin::RenderView *, ViewHistory> m_history;
    /// The last damage to each window, in the order damage came.
    std::unordered_map<KWin::EffectWindow *, quint64> m_damage;
    quint64 m_damageCount = 0;
    /// Changes everything is drawn again for, like the stacking order changing.
    quint64 m_everything = 1;
    /// This frame: the windows being animated, and those that were last frame.
    std::unordered_set<KWin::EffectWindow *> m_animatedNow;
    std::unordered_set<KWin::EffectWindow *> m_animatedLately;
    /// Where each window is in the stacking order this frame.
    std::unordered_map<KWin::EffectWindow *, int> m_stackIndex;
    /// What's changed on screen so far this frame, in logical coordinates: below the window being
    /// drawn, since they're drawn from the bottom up.
    KWin::Region m_changedBelow;
};

}
