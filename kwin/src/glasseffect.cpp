/*
    SPDX-FileCopyrightText: 2026 the makshell contributors

    SPDX-License-Identifier: GPL-3.0-or-later
*/

#include "glasseffect.h"
#include "arranger.h"
#include "window.h"
#include "glassmanager.h"

#include "core/output.h"
#include "core/pixelgrid.h"
#include "scene/itemgeometry.h"
#include "core/rendertarget.h"
#include "core/renderviewport.h"
#include "effect/effecthandler.h"
#include "opengl/eglcontext.h"
#include "scene/backgroundeffectitem.h"
#include "wayland/surface.h"

#include <KConfigGroup>
#include <KSharedConfig>

#include <KGlobalAccel>

#include <QAction>
#include <QDBusConnection>
#include <QDBusMessage>
#include <QDBusPendingCall>
#include <QKeySequence>

#include <QLoggingCategory>
#include <QMatrix4x4>
#include <QPointer>
#include <QScopeGuard>
#include <QTimer>
#include <QWindow>

#include <algorithm>
#include <cmath>
#include <map>
#include <unordered_map>
#include <numbers>

Q_LOGGING_CATEGORY(MAKSHELL_GLASS, "makshell_glass", QtWarningMsg)

using namespace KWin;

namespace Makshell
{

/// The app ID of the shell's dialogs, which the window manager places and keeps on top.
static constexpr QLatin1StringView s_dialogAppId("makshell-alert");

/// The title of the window switcher's window, and its corner radius. It's KWin's own window, so it has
/// no surface to ask for glass with, and this is how it's told apart.
static constexpr QLatin1StringView s_switcherTitle("makshell-switcher");
static constexpr int s_switcherRadius = 30;

/// The decoration's corner radius, which app windows' glass follows.
static constexpr int s_windowRadius = 16;
/// About what a window that draws its own title bar has, like a GTK app's.
static constexpr int s_clientDecoratedRadius = 12;
/// How much of a window can be see-through by itself, as its rounded corners are, before it counts
/// as being see-through.
static constexpr double s_cornersShare = 0.02;
/// How big a copy of a window its background colors are found in.
static constexpr QSize s_keySampleSize(64, 64);
/// How much of a window a color has to cover to be its background, and how many it can have.
static constexpr double s_keyShare = 0.06;
static constexpr int s_maxKeys = 4;
/// How much of a window has to be see-through for it to be see-through by itself.
static constexpr double s_clearShare = 0.3;
/// How often a window that keeps changing has its background colors looked for again.
static constexpr int s_keyLookMs = 1000;
/// How long a keyed window is drawn by itself every frame after it last moved or was animated.
static constexpr int s_keyedSettleMs = 300;
/// The least opaque windows can be made, past which they'd be hard to read.
static constexpr float s_leastWindowOpacity = 0.5f;

/// Where the apps that are left as they are, with no blur and no translucency, are kept in kwinrc.
static const QString s_exceptionsKey = QStringLiteral("BlurExceptions");

/// How long a new surface stays out of sight if its glass doesn't come.
static constexpr int s_awaitGlassMs = 200;

static bool isDialog(Style style)
{
    return style == Style::Window || style == Style::WindowInactive;
}

/// Brightness probes downsample the background to this many pixels before reading it back.
static constexpr QSize s_probeSize(32, 4);
/// How far past the edges of glass to repaint when it changes, in logical pixels.
static constexpr int s_edgeRepaint = 2;
/// How many shapes' textures are kept when there are fewer shapes than there were.
static constexpr size_t s_spareRenders = 6;
/// How long glass a client let go of stays if its window turns out not to be closing.
static constexpr int s_releasedGlassMs = 1000;
/// How long glass that's drawn again goes at most without blurring what's behind it again.
static constexpr int s_reblurAnywayMs = 500;

/// Saturation as a color matrix, applied to row vectors like KWin's blur does.
static QMatrix4x4 saturationMatrix(float saturation)
{
    const float r = (1.0f - saturation) * 0.2126f;
    const float g = (1.0f - saturation) * 0.7152f;
    const float b = (1.0f - saturation) * 0.0722f;
    return QMatrix4x4(r + saturation, r, r, 0.0f,
                      g, g + saturation, g, 0.0f,
                      b, b, b + saturation, 0.0f,
                      0.0f, 0.0f, 0.0f, 1.0f);
}

static float srgbToLinear(float c)
{
    return c <= 0.04045f ? c / 12.92f : std::pow((c + 0.055f) / 1.055f, 2.4f);
}

GlassEffect::GlassEffect()
{
    m_glassShader = ShaderManager::instance()->generateShaderFromFile(ShaderTrait::MapTexture,
                                                                      QStringLiteral(":/effects/makshell/shaders/vertex.vert"),
                                                                      QStringLiteral(":/effects/makshell/shaders/glass.frag"));
    m_downsampleShader = ShaderManager::instance()->generateShaderFromFile(ShaderTrait::MapTexture,
                                                                           QStringLiteral(":/effects/makshell/shaders/vertex.vert"),
                                                                           QStringLiteral(":/effects/makshell/shaders/downsample.frag"));
    m_upsampleShader = ShaderManager::instance()->generateShaderFromFile(ShaderTrait::MapTexture,
                                                                         QStringLiteral(":/effects/makshell/shaders/vertex.vert"),
                                                                         QStringLiteral(":/effects/makshell/shaders/upsample.frag"));
    m_keyedShader = ShaderManager::instance()->generateShaderFromFile(ShaderTrait::MapTexture, QString(),
                                                                      QStringLiteral(":/effects/makshell/shaders/keyed.frag"));
    if (!m_glassShader || !m_downsampleShader || !m_upsampleShader || !m_keyedShader) {
        qCWarning(MAKSHELL_GLASS) << "failed to load the glass shaders";
        return;
    }

    m_manager = std::make_unique<GlassManager>(effects->waylandDisplay());
    m_arranger = std::make_unique<Arranger>();
    connect(m_manager.get(), &GlassManager::glassChanged, this, [this](SurfaceInterface *surface) {
        if (EffectWindow *w = effects->findWindow(surface)) {
            updateWindow(w);
        }
    });

    // What changes what's on screen, so glass knows when what's behind it has. Damage covers a
    // window's surfaces, its decoration and its shadow; the rest is drawn differently or not at all.
    const auto follow = [this](EffectWindow *w) {
        connect(w, &EffectWindow::windowDamaged, this, [this](EffectWindow *w) {
            m_damage[w] = ++m_damageCount;
        });
        connect(w, &EffectWindow::minimizedChanged, this, &GlassEffect::changedEverything);
        connect(w, &EffectWindow::windowHiddenChanged, this, &GlassEffect::changedEverything);
    };
    for (EffectWindow *w : effects->stackingOrder()) {
        follow(w);
    }
    connect(effects, &EffectsHandler::windowAdded, this, follow);
    connect(effects, &EffectsHandler::windowAdded, this, &GlassEffect::changedEverything);
    connect(effects, &EffectsHandler::windowClosed, this, &GlassEffect::changedEverything);
    connect(effects, &EffectsHandler::stackingOrderChanged, this, &GlassEffect::changedEverything);
    connect(effects, &EffectsHandler::showingDesktopChanged, this, &GlassEffect::changedEverything);
    connect(effects, &EffectsHandler::desktopChanged, this, &GlassEffect::changedEverything);
    connect(effects, &EffectsHandler::screenAdded, this, &GlassEffect::changedEverything);
    connect(effects, &EffectsHandler::screenRemoved, this, &GlassEffect::changedEverything);

    // Glass can arrive before its window maps, and windows can go without dropping it.
    connect(effects, &EffectsHandler::windowAdded, this, &GlassEffect::placeDialog);
    connect(effects, &EffectsHandler::windowAdded, this, [this](EffectWindow *w) {
        if (w->surface() && m_manager->usesGlass(w->surface())) {
            m_awaitingGlass[w].start();
        }
        updateWindow(w);
        updateSwitcherGlass(w);
        watchAppWindow(w);
        updateAppGlass(w);
    });

    connect(effects, &EffectsHandler::windowClosed, this, [this](EffectWindow *w) {
        if (auto it = m_windows.find(w); it != m_windows.end()) {
            it->second.closing = true;
            if (it->second.appear.kind != 0 && !effects->isScreenLocked()) {
                it->second.deletedRef.emplace(w);
                it->second.disappearing.start();
            }
            w->addRepaintFull();
            effects->addRepaintFull();
        }
    });
    connect(effects, &EffectsHandler::windowDeleted, this, [this](EffectWindow *w) {
        changedEverything();
        m_damage.erase(w);
        m_animatedNow.erase(w);
        m_animatedLately.erase(w);
        for (auto &[view, history] : m_history) {
            history.looks.erase(w);
            history.animated.erase(w);
        }
        m_awaitingGlass.erase(w);
        if (auto it = m_windows.find(w); it != m_windows.end()) {
            effects->makeOpenGLContextCurrent();
            m_windows.erase(it);
            effects->addRepaintFull();
        }
    });
    connect(effects, &EffectsHandler::viewRemoved, this, [this](RenderView *view) {
        effects->makeOpenGLContextCurrent();
        for (auto &[window, glass] : m_windows) {
            glass.render.erase(view);
        }
        m_history.erase(view);
    });

    // KWin's slide between desktops lifts every dock above all windows while it lasts, so that panels
    // stay over a full-screen window. The desktop's icons and widgets are a dock to KWin too, and
    // belong under windows. Lifting happens as the switch starts, so this goes right after.
    const auto keepDesktopDown = [this] {
        QTimer::singleShot(0, this, [] {
            for (EffectWindow *w : effects->stackingOrder()) {
                if (w->isDock() && w->window()->layer() == BelowLayer) {
                    effects->setElevatedWindow(w, false);
                }
            }
        });
    };
    connect(effects, &EffectsHandler::desktopChanged, this, keepDesktopDown);
    connect(effects, &EffectsHandler::desktopChanging, this, keepDesktopDown);

    m_saveSoon = new QTimer(this);
    m_saveSoon->setSingleShot(true);
    m_saveSoon->setInterval(500);
    connect(m_saveSoon, &QTimer::timeout, this, [] {
        KSharedConfig::openConfig(QStringLiteral("kwinrc"))->sync();
    });
    QDBusConnection::sessionBus().registerObject(QStringLiteral("/org/makshell/Glass"), this, QDBusConnection::ExportScriptableSlots);

    // Spotlight's shortcut. It's here because global shortcuts belong to whatever registers them, and
    // KWin's are the ones that can be changed along with all the others.
    auto *spotlight = new QAction(this);
    spotlight->setObjectName(QStringLiteral("Spotlight"));
    spotlight->setText(QStringLiteral("Spotlight Search"));
    KGlobalAccel::self()->setDefaultShortcut(spotlight, {Qt::META | Qt::Key_Space});
    KGlobalAccel::self()->setShortcut(spotlight, {Qt::META | Qt::Key_Space});
    // The media keys. Plasma's own volume and brightness controls aren't running here, so these take
    // their place under the same names, and keep whatever keys were set for them.
    struct MediaKey {
        const char *component;
        const char *name;
        QKeySequence key;
    };
    const MediaKey mediaKeys[] = {
        {"kmix", "increase_volume", Qt::Key_VolumeUp},
        {"kmix", "decrease_volume", Qt::Key_VolumeDown},
        {"kmix", "increase_volume_small", Qt::SHIFT | Qt::Key_VolumeUp},
        {"kmix", "decrease_volume_small", Qt::SHIFT | Qt::Key_VolumeDown},
        {"kmix", "mute", Qt::Key_VolumeMute},
        {"org_kde_powerdevil", "Increase Screen Brightness", Qt::Key_MonBrightnessUp},
        {"org_kde_powerdevil", "Decrease Screen Brightness", Qt::Key_MonBrightnessDown},
        {"org_kde_powerdevil", "Increase Screen Brightness Small", Qt::SHIFT | Qt::Key_MonBrightnessUp},
        {"org_kde_powerdevil", "Decrease Screen Brightness Small", Qt::SHIFT | Qt::Key_MonBrightnessDown},
    };
    for (const MediaKey &mediaKey : mediaKeys) {
        auto *action = new QAction(this);
        const QString name = QString::fromLatin1(mediaKey.name);
        action->setObjectName(name);
        action->setText(name);
        action->setProperty("componentName", QString::fromLatin1(mediaKey.component));
        KGlobalAccel::self()->setDefaultShortcut(action, {mediaKey.key});
        KGlobalAccel::self()->setShortcut(action, {mediaKey.key});
        connect(action, &QAction::triggered, this, [name] {
            auto message = QDBusMessage::createMethodCall(QStringLiteral("org.makshell.Shell"),
                                                          QStringLiteral("/org/makshell/Windows"),
                                                          QStringLiteral("org.makshell.Windows"),
                                                          QStringLiteral("MediaKey"));
            message << name;
            QDBusConnection::sessionBus().asyncCall(message);
        });
    }

    connect(spotlight, &QAction::triggered, this, [] {
        const auto message = QDBusMessage::createMethodCall(QStringLiteral("org.makshell.Shell"),
                                                            QStringLiteral("/org/makshell/Windows"),
                                                            QStringLiteral("org.makshell.Windows"),
                                                            QStringLiteral("ToggleSpotlight"));
        QDBusConnection::sessionBus().asyncCall(message);
    });

    // This was called macshell before, and so were its settings.
    {
        const KSharedConfig::Ptr kwinrc = KSharedConfig::openConfig(QStringLiteral("kwinrc"));
        const KConfigGroup old = kwinrc->group(QStringLiteral("Effect-macshell"));
        KConfigGroup now = kwinrc->group(QStringLiteral("Effect-makshell"));
        if (old.exists() && !now.exists()) {
            old.copyTo(&now);
            kwinrc->deleteGroup(QStringLiteral("Effect-macshell"));
            kwinrc->sync();
        }
    }
    reconfigure(ReconfigureAll);
    for (EffectWindow *w : effects->stackingOrder()) {
        updateWindow(w);
        watchAppWindow(w);
        updateAppGlass(w);
    }
    m_valid = true;
}

GlassEffect::~GlassEffect()
{
    QDBusConnection::sessionBus().unregisterObject(QStringLiteral("/org/makshell/Glass"));
    effects->makeOpenGLContextCurrent();
    // Letting go of a closed window deletes it and its items, and tells us about it. So the items go
    // first, and the windows once nothing here refers to them.
    std::vector<EffectWindowDeletedRef> closed;
    for (auto &[window, glass] : m_windows) {
        glass.item.reset();
        glass.render.clear();
        if (glass.deletedRef) {
            closed.push_back(std::move(*glass.deletedRef));
            glass.deletedRef.reset();
        }
    }
    m_windows.clear();
}

bool GlassEffect::supported()
{
    return effects->isOpenGLCompositing();
}

void GlassEffect::reconfigure(ReconfigureFlags flags)
{
    Q_UNUSED(flags)
    const KConfigGroup group = KSharedConfig::openConfig(QStringLiteral("kwinrc"))->group(QStringLiteral("Effect-makshell"));
    const GlassSettings defaults;
    m_settings.blurPasses = std::clamp(group.readEntry("BlurPasses", defaults.blurPasses), 1, 4);
    m_settings.blurOffset = group.readEntry("BlurOffset", defaults.blurOffset);
    m_settings.frostPasses = std::clamp(group.readEntry("FrostPasses", defaults.frostPasses), 1, 5);
    m_settings.frostOffset = group.readEntry("FrostOffset", defaults.frostOffset);
    m_settings.frostSaturation = group.readEntry("FrostSaturation", defaults.frostSaturation);
    m_settings.frostTint = group.readEntry("FrostTint", defaults.frostTint);
    m_settings.frostEdgeWhite = group.readEntry("FrostEdgeWhite", defaults.frostEdgeWhite);
    m_settings.frostOuterEdge = group.readEntry("FrostOuterEdge", defaults.frostOuterEdge);
    m_settings.liquidBrightness = group.readEntry("LiquidBrightness", defaults.liquidBrightness);
    m_settings.liquidSaturation = group.readEntry("LiquidSaturation", defaults.liquidSaturation);
    m_settings.liquidEdgeFade = group.readEntry("LiquidEdgeFade", defaults.liquidEdgeFade);
    m_settings.liquidEdgeGain = group.readEntry("LiquidEdgeGain", defaults.liquidEdgeGain);
    m_settings.controlTint = group.readEntry("ControlTint", defaults.controlTint);
    m_settings.controlTintGrey = group.readEntry("ControlTintGrey", defaults.controlTintGrey);
    m_settings.controlSaturation = group.readEntry("ControlSaturation", defaults.controlSaturation);
    m_settings.controlRim = group.readEntry("ControlRim", defaults.controlRim);
    m_settings.backdropBrightness = group.readEntry("BackdropBrightness", defaults.backdropBrightness);
    m_settings.grain = group.readEntry("Grain", defaults.grain);
    m_settings.rimTint = group.readEntry("RimTint", defaults.rimTint);
    m_settings.glassIor = group.readEntry("GlassIor", defaults.glassIor);
    m_settings.glassIorSpread = group.readEntry("GlassIorSpread", defaults.glassIorSpread);
    m_settings.glassProfile = group.readEntry("GlassProfile", defaults.glassProfile);
    m_settings.glassShoulder = group.readEntry("GlassShoulder", defaults.glassShoulder);
    m_settings.glassGloss = group.readEntry("GlassGloss", defaults.glassGloss);
    m_settings.glassThickness = group.readEntry("GlassThickness", defaults.glassThickness);
    m_settings.glassBezel = group.readEntry("GlassBezel", defaults.glassBezel);
    m_settings.inactiveTint = group.readEntry("InactiveTint", defaults.inactiveTint);
    m_settings.chipTint = group.readEntry("ChipTint", defaults.chipTint);
    m_settings.chipRim = group.readEntry("ChipRim", defaults.chipRim);
    m_settings.bubbleTint = group.readEntry("BubbleTint", defaults.bubbleTint);
    m_settings.bubbleEdgeGain = group.readEntry("BubbleEdgeGain", defaults.bubbleEdgeGain);
    m_settings.windowOpacity = std::clamp(group.readEntry("WindowOpacity", defaults.windowOpacity), s_leastWindowOpacity, 1.0f);
    m_settings.windowBackgroundsOnly = group.readEntry("WindowBackgroundsOnly", defaults.windowBackgroundsOnly);
    m_exceptions = group.readEntry(s_exceptionsKey, QStringList());
    if (m_valid) {
        for (EffectWindow *w : effects->stackingOrder()) {
            updateAppGlass(w);
        }
    }
    changedEverything();
    effects->addRepaintFull();
}

/// What the Settings app may change, besides the green button's click, which the decoration reads.
static const std::map<QString, float GlassSettings::*> s_tunable = {
    {QStringLiteral("GlassThickness"), &GlassSettings::glassThickness},
    {QStringLiteral("GlassBezel"), &GlassSettings::glassBezel},
    {QStringLiteral("GlassIor"), &GlassSettings::glassIor},
    {QStringLiteral("GlassIorSpread"), &GlassSettings::glassIorSpread},
    {QStringLiteral("GlassProfile"), &GlassSettings::glassProfile},
    {QStringLiteral("GlassGloss"), &GlassSettings::glassGloss},
    {QStringLiteral("Grain"), &GlassSettings::grain},
};
static const QString s_greenFullScreen = QStringLiteral("GreenFullScreen");
/// Not among the glass's own, which resetting them leaves alone.
static const QString s_windowOpacity = QStringLiteral("WindowOpacity");
static const QString s_windowBackgroundsOnly = QStringLiteral("WindowBackgroundsOnly");

static KConfigGroup settingsGroup()
{
    return KSharedConfig::openConfig(QStringLiteral("kwinrc"))->group(QStringLiteral("Effect-makshell"));
}

double GlassEffect::setting(const QString &key) const
{
    if (key == s_greenFullScreen) {
        return settingsGroup().readEntry(key, 0.0);
    }
    if (key == s_windowOpacity) {
        return m_settings.windowOpacity;
    }
    if (key == s_windowBackgroundsOnly) {
        return m_settings.windowBackgroundsOnly;
    }
    const auto found = s_tunable.find(key);
    return found == s_tunable.end() ? 0.0 : m_settings.*(found->second);
}

void GlassEffect::setSetting(const QString &key, double value)
{
    if (key != s_greenFullScreen && key != s_windowOpacity && key != s_windowBackgroundsOnly && !s_tunable.contains(key)) {
        return;
    }
    settingsGroup().writeEntry(key, value);
    m_saveSoon->start();
    reconfigure(ReconfigureAll);
}

void GlassEffect::resetSettings()
{
    KConfigGroup group = settingsGroup();
    for (const auto &[key, member] : s_tunable) {
        group.deleteEntry(key);
    }
    m_saveSoon->start();
    reconfigure(ReconfigureAll);
}

QStringList GlassEffect::exceptions() const
{
    return m_exceptions;
}

void GlassEffect::setException(const QString &app, bool excepted)
{
    QStringList apps = m_exceptions;
    apps.removeAll(app);
    if (excepted) {
        apps.append(app);
    }
    settingsGroup().writeEntry(s_exceptionsKey, apps);
    m_saveSoon->start();
    reconfigure(ReconfigureAll);
}

bool GlassEffect::isException(EffectWindow *w) const
{
    // Listed by desktop entry, which on Wayland is usually the app ID too, but not always.
    const QString windowClass = w->window()->resourceClass();
    const QString entry = w->window()->desktopFileName();
    return std::any_of(m_exceptions.cbegin(), m_exceptions.cend(), [&](const QString &app) {
        return app.compare(windowClass, Qt::CaseInsensitive) == 0 || (!entry.isEmpty() && app.compare(entry, Qt::CaseInsensitive) == 0);
    });
}

bool GlassEffect::takesAppGlass(EffectWindow *w) const
{
    // A window that draws its own title bar has its shadow in its surface too, but its frame is where
    // it says its edges are, without the shadow, and that's what the glass covers.
    // One with no title bar at all that covers its whole screen is an overlay, like a screenshot
    // tool's for choosing an area, which shows the screen through it as it is.
    const bool overlay = !w->hasDecoration() && w->screen() && w->frameGeometry().contains(RectF(w->screen()->geometry()));
    return !w->isDeleted() && (w->isNormalWindow() || w->isDialog()) && !w->isFullScreen() && !overlay
        && !w->internalWindow() && !(w->surface() && m_manager->usesGlass(w->surface()));
}

/// Whether a window's own content lets what's behind it show, like a terminal set to be see-through.
static bool seeThrough(EffectWindow *w)
{
    SurfaceInterface *surface = w->surface();
    if (!surface) {
        return false;
    }
    // Only within the window's frame: one that draws its own title bar has its shadow in its surface,
    // which is see-through, and so are its rounded corners, which are too small to count.
    const RectF frame = RectF(w->frameGeometry()).translated(-w->bufferGeometry().topLeft()) & RectF(QPointF(), surface->size());
    if (frame.isEmpty()) {
        return false;
    }
    double clear = 0;
    for (const Rect &rect : RegionF(frame).subtracted(surface->opaque()).rounded().rects()) {
        clear += double(rect.width()) * rect.height();
    }
    return clear > s_cornersShare * frame.width() * frame.height();
}

void GlassEffect::watchAppWindow(EffectWindow *w)
{
    if (w->isNormalWindow() || w->isDialog()) {
        connect(w, &EffectWindow::windowFrameGeometryChanged, this, [this](EffectWindow *w) {
            updateAppGlass(w);
        });
        connect(w, &EffectWindow::windowMaximizedStateChanged, this, [this](EffectWindow *w) {
            updateAppGlass(w);
        });
        connect(w, &EffectWindow::windowFullScreenChanged, this, &GlassEffect::updateAppGlass);
        if (SurfaceInterface *surface = w->surface()) {
            connect(surface, &SurfaceInterface::opaqueChanged, this, [this, window = QPointer<EffectWindow>(w)]() {
                if (window) {
                    updateAppGlass(window);
                }
            });
        }
    }
}

void GlassEffect::updateAppGlass(EffectWindow *w)
{
    auto it = m_windows.find(w);
    const bool ours = it != m_windows.end() && it->second.automatic;
    if (it != m_windows.end() && !ours) {
        // The app's own.
        return;
    }
    const bool fits = takesAppGlass(w) && !isException(w);
    // With windows see-through, each is drawn by itself first, which shows whether it's see-through by
    // itself as well, and keeps its own opacity if it is. Otherwise, there's only what an app says
    // about its window to go by, which not all of them do, and a window that is gets the blur.
    const bool translucentWindows = m_settings.windowOpacity < 1.0f;
    const bool clear = fits && !translucentWindows && seeThrough(w);
    const bool dim = fits && translucentWindows;
    if (!clear && !dim) {
        if (ours) {
            effects->makeOpenGLContextCurrent();
            m_windows.erase(it);
            w->addRepaintFull();
        }
        return;
    }

    WindowGlass &data = m_windows[w];
    data.automatic = true;
    data.dim = dim;
    if (dim) {
        if (!data.keyed) {
            data.keyed = std::make_unique<KeyedWindow>();
            KeyedWindow *keyed = data.keyed.get();
            keyed->damaged = connect(w, &EffectWindow::windowDamaged, this, [keyed] {
                keyed->dirty = true;
            });
        }
    } else if (data.keyed) {
        effects->makeOpenGLContextCurrent();
        data.keyed.reset();
    }
    // Shapes are surface-local, and this one covers the whole frame, title bar included.
    const QPointF offset = w->bufferGeometry().topLeft() - w->frameGeometry().topLeft();
    const QSizeF size = w->frameGeometry().size();
    const int radius = w->window()->maximizeMode() == MaximizeFull ? 0
        : w->hasDecoration()                                      ? s_windowRadius
                                                                  : s_clientDecoratedRadius;
    data.shapes = {Shape{.rect = QRect((-offset).toPoint(), size.toSize()), .radius = radius, .style = Style::AppWindow}};
    if (!data.item) {
        data.item = std::make_unique<BackgroundEffectItem>(w->windowItem());
        connect(w, &EffectWindow::windowFrameGeometryChanged, this, &GlassEffect::windowMoved, Qt::UniqueConnection);
    }
    const int blurReach = m_settings.frostOffset * (1 << m_settings.frostPasses) * 2;
    data.item->setPixelsToExpandRepaintsBelowOpaqueRegions(blurReach);
    data.item->setEffectBoundingRect(RectF(QPointF(), size));
    data.lastBounds = RectF(w->frameGeometry()).roundedOut();
    data.changed = ++m_damageCount;
    w->addRepaintFull();
}

void GlassEffect::placeDialog(EffectWindow *w)
{
    if (!w->windowClass().contains(s_dialogAppId, Qt::CaseInsensitive)) {
        return;
    }
    // Clients can't place their windows or keep them on top. A dialog has no business being lost
    // behind others or listed among the apps.
    Window *window = w->window();
    window->setKeepAbove(true);
    window->setSkipTaskbar(true);
    window->setSkipSwitcher(true);
    window->setSkipPager(true);
    const RectF area = effects->clientArea(MaximizeArea, effects->activeScreen());
    window->move(area.center() - QPointF(w->width(), w->height()) / 2);
}

void GlassEffect::updateSwitcherGlass(EffectWindow *w)
{
    if (!w->internalWindow() || w->internalWindow()->title() != s_switcherTitle) {
        return;
    }
    const QRect rect(QPoint(), w->frameGeometry().size().toSize());
    WindowGlass &data = m_windows[w];
    data.shapes = {Shape{.rect = rect, .radius = s_switcherRadius, .style = Style::Card}};
    if (!data.item) {
        data.item = std::make_unique<BackgroundEffectItem>(w->windowItem());
        connect(w, &EffectWindow::windowFrameGeometryChanged, this, &GlassEffect::windowMoved, Qt::UniqueConnection);
    }
    data.item->setPixelsToExpandRepaintsBelowOpaqueRegions(std::ceil(m_settings.glassThickness * 2.4f));
    data.item->setEffectBoundingRect(RectF(Rect(rect)));
    data.lastBounds = Rect(rect).translated(w->pos().toPoint());
    data.changed = ++m_damageCount;
    w->addRepaintFull();
}

void GlassEffect::windowMoved(EffectWindow *w, const RectF &oldGeometry)
{
    // It starts out small, and sizes itself to what's in it.
    updateSwitcherGlass(w);
    auto it = m_windows.find(w);
    if (it == m_windows.end() || it->second.lastBounds.isEmpty()) {
        return;
    }
    const QPointF moved = w->frameGeometry().topLeft() - oldGeometry.topLeft();
    const Rect before = it->second.lastBounds;
    const Rect after = RectF(before).translated(moved).roundedOut();
    it->second.lastBounds = after;
    const Rect touched = before | after;
    effects->addRepaint(Rect(touched.x() - s_edgeRepaint, touched.y() - s_edgeRepaint, touched.width() + 2 * s_edgeRepaint, touched.height() + 2 * s_edgeRepaint));
}

void GlassEffect::updateWindow(EffectWindow *w)
{
    GlassSurface *glass = w->surface() ? m_manager->glassFor(w->surface()) : nullptr;
    if (auto it = m_windows.find(w); it != m_windows.end() && it->second.automatic) {
        if (!glass) {
            return;
        }
        // The app asked for glass of its own after all, which takes over.
        effects->makeOpenGLContextCurrent();
        m_windows.erase(it);
    }
    if (glass && !glass->shapes().isEmpty()) {
        m_awaitingGlass.erase(w);
    }
    if (!glass) {
        // A client lets go of its glass as it closes the surface; that glass still has a closing
        // animation to fade out in. If the window turns out to be staying, it goes after all.
        if (m_windows.contains(w) && !w->isDeleted()) {
            QTimer::singleShot(s_releasedGlassMs, this, [this, window = QPointer<EffectWindow>(w)]() {
                auto it = m_windows.find(window.data());
                if (!window || it == m_windows.end() || it->second.closing || window->isDeleted()
                    || (window->surface() && m_manager->glassFor(window->surface()))) {
                    return;
                }
                effects->makeOpenGLContextCurrent();
                m_windows.erase(it);
                window->addRepaintFull();
                effects->addRepaintFull();
            });
        }
        return;
    }
    if (glass->shapes().isEmpty()) {
        if (auto it = m_windows.find(w); it != m_windows.end()) {
            effects->makeOpenGLContextCurrent();
            m_windows.erase(it);
            w->addRepaintFull();
            // At a fractional scale, the edge of glass can land in a row of device pixels that the
            // window's own repaint misses, and stay on screen until something else repaints there.
            effects->addRepaintFull();
        }
        return;
    }

    // Shapes are surface-local; the scene item wants frame-local.
    const QPointF offset = w->bufferGeometry().topLeft() - w->frameGeometry().topLeft();
    RectF bounds;
    int passes = m_settings.blurPasses;
    float blurOffset = m_settings.blurOffset;
    for (const Shape &shape : glass->shapes()) {
        bounds |= RectF(Rect(shape.rect)).translated(offset);
        if (shape.style == Style::Frosted || isDialog(shape.style)) {
            passes = m_settings.frostPasses;
            blurOffset = m_settings.frostOffset;
        }
    }

    WindowGlass &data = m_windows[w];
    data.shapes = glass->shapes();
    if (glass->appear().kind != 0 && !data.appearing.isValid() && !w->isDeleted()) {
        data.appear = glass->appear();
        data.appearing.start();
    }
    if (!data.item) {
        data.item = std::make_unique<BackgroundEffectItem>(w->windowItem());
        connect(w, &EffectWindow::windowFrameGeometryChanged, this, &GlassEffect::windowMoved, Qt::UniqueConnection);
    }
    // The lens and the blur read the background well away from each pixel, so repaints under the
    // glass have to reach that far.
    const int blurReach = blurOffset * (1 << passes) * 2;
    data.item->setPixelsToExpandRepaintsBelowOpaqueRegions(std::ceil(m_settings.glassThickness * 2.4f) + blurReach);
    data.item->setEffectBoundingRect(bounds);
    data.changed = ++m_damageCount;
    w->addRepaintFull();

    // Past the edges of where the glass was and now is, for the same reason as when it goes away.
    const Rect now = bounds.translated(w->pos()).roundedOut();
    const Rect touched = data.lastBounds.isEmpty() ? now : now | data.lastBounds;
    data.lastBounds = now;
    effects->addRepaint(Rect(touched.x() - s_edgeRepaint, touched.y() - s_edgeRepaint, touched.width() + 2 * s_edgeRepaint, touched.height() + 2 * s_edgeRepaint));
}

/// How a surface looks at some point of coming or going.
struct AppearState {
    float scale = 1;
    float opacity = 1;
    float slide = 0;
    bool done = true;
};

/// A spring from 0 to 1, in the terms Apple describes them in: how long it takes to get most of the
/// way there, and how little it overshoots, with 1 being not at all.
static float spring(float seconds, float response, float damping)
{
    const float omega = 2 * std::numbers::pi_v<float> / response;
    if (damping >= 1) {
        return 1 - std::exp(-omega * seconds) * (1 + omega * seconds);
    }
    const float decay = damping * omega;
    const float ringing = omega * std::sqrt(1 - damping * damping);
    return 1 - std::exp(-decay * seconds) * (std::cos(ringing * seconds) + decay / ringing * std::sin(ringing * seconds));
}

static AppearState appearState(const WindowGlass &glass)
{
    // Measured from a recording of Tahoe, frame by frame. Panels fade with next to no scaling, menus
    // are there at once, alerts spring out of a point, going about 7% past their size and back, and
    // vanish at once. Notification Center comes in fast and slows down, and goes picking up speed.
    AppearState state;
    if (glass.appear.kind == 0) {
        return state;
    }
    constexpr int fade = 1;
    constexpr int pop = 2;
    constexpr int slide = 3;
    constexpr int grow = 4;
    constexpr int quick = 5;
    if (glass.disappearing.isValid()) {
        const float seconds = glass.disappearing.elapsed() / 1000.0f;
        const float duration = glass.appear.kind == slide   ? 0.2f
            : glass.appear.kind == fade || glass.appear.kind == quick ? 0.13f
            : glass.appear.kind == pop                       ? 0.1f
                                                             : 0.0f;
        const float t = duration > 0 ? std::min(seconds / duration, 1.0f) : 1.0f;
        const float eased = t * t * (3 - 2 * t);
        state.opacity = glass.appear.kind == slide ? 1 - eased * eased : 1 - eased;
        state.slide = glass.appear.kind == slide ? eased : 0;
        state.done = t >= 1;
        return state;
    }
    const float seconds = glass.appearing.elapsed() / 1000.0f;
    switch (glass.appear.kind) {
    case fade:
    case quick: {
        const float duration = glass.appear.kind == quick ? 0.04f : 0.13f;
        const float t = std::min(seconds / duration, 1.0f);
        state.opacity = t * t * (3 - 2 * t);
        state.done = t >= 1;
        break;
    }
    case slide: {
        state.slide = std::exp(-seconds / 0.073f);
        state.done = state.slide < 0.002f;
        if (state.done) {
            state.slide = 0;
        }
        break;
    }
    case grow: {
        const float value = spring(seconds, 0.26f, 0.62f);
        state.scale = 0.4f + 0.6f * value;
        state.opacity = std::min(seconds / 0.11f, 1.0f);
        state.done = seconds >= 0.6f;
        if (state.done) {
            state.scale = 1;
        }
        break;
    }
    case pop:
    default:
        state.done = true;
        break;
    }
    return state;
}

void GlassEffect::prePaintScreen(ScreenPrePaintData &data)
{
    m_currentView = data.view;
    m_animatedNow.clear();
    for (const auto &[window, glass] : m_windows) {
        if (!appearState(glass).done) {
            data.mask |= PAINT_SCREEN_WITH_TRANSFORMED_WINDOWS;
            break;
        }
    }
    effects->prePaintScreen(data);
}

PaintResult GlassEffect::paintScreen(const RenderTarget &renderTarget, const RenderViewport &viewport, int mask, const Region &deviceRegion, LogicalOutput *screen)
{
    m_screenTarget = &renderTarget;
    const auto done = qScopeGuard([this] {
        m_screenTarget = nullptr;
    });

    // Windows are drawn from the bottom up, and each adds what it changes to this, for the glass of
    // the ones above. A whole screen drawn differently changes everything.
    ViewHistory &history = m_history[m_currentView];
    const bool everything = history.everything != m_everything || effects->activeFullScreenEffect() || (mask & PAINT_SCREEN_TRANSFORMED);
    history.everything = m_everything;
    m_changedBelow = everything ? Region::infinite() : Region();
    // A window that's done animating may have been drawn anywhere last frame, and isn't now.
    m_animatedLately = std::move(history.animated);
    history.animated = m_animatedNow;
    m_stackIndex.clear();
    int index = 0;
    for (EffectWindow *w : effects->stackingOrder()) {
        m_stackIndex[w] = index++;
    }
    return effects->paintScreen(renderTarget, viewport, mask, deviceRegion, screen);
}

void GlassEffect::changedEverything()
{
    ++m_everything;
}

bool GlassEffect::isMinimizing(EffectWindow *w)
{
    return w->data(WindowMinimizedGrabRole).isValid() || w->data(WindowUnminimizedGrabRole).isValid();
}

bool GlassEffect::behindChanged(EffectWindow *w) const
{
    if (m_changedBelow.intersects(w->expandedGeometry().roundedOut())) {
        return true;
    }
    const auto history = m_history.find(m_currentView);
    const auto glass = m_windows.find(w);
    if (history == m_history.end() || glass == m_windows.end()) {
        return true;
    }
    const auto look = history->second.looks.find(w);
    if (look == history->second.looks.end() || look->second.glass != glass->second.changed) {
        return true;
    }
    // An animated window below may be drawn anywhere: the genie's goes all the way to the dock. One
    // that's gone from the stacking order was below everything left.
    const auto index = m_stackIndex.find(w);
    const auto below = [&](EffectWindow *animated) {
        if (animated == w) {
            return true;
        }
        const auto other = m_stackIndex.find(animated);
        return other == m_stackIndex.end() || index == m_stackIndex.end() || other->second < index->second;
    };
    return std::ranges::any_of(m_animatedNow, below) || std::ranges::any_of(m_animatedLately, below);
}

void GlassEffect::noteDrawn(EffectWindow *w, const WindowPaintData &data, bool reblurred)
{
    const auto damage = m_damage.find(w);
    const auto glass = m_windows.find(w);
    const WindowLook now{
        .geometry = w->expandedGeometry(),
        .opacity = data.opacity(),
        .brightness = data.brightness(),
        .saturation = data.saturation(),
        .xScale = data.xScale(),
        .yScale = data.yScale(),
        .translation = data.translation(),
        .damage = damage == m_damage.end() ? 0 : damage->second,
        .glass = glass == m_windows.end() ? 0 : glass->second.changed,
    };
    auto [look, added] = m_history[m_currentView].looks.try_emplace(w, now);
    if (added || !look->second.sameAs(now) || look->second.damage != now.damage || reblurred) {
        if (now.xScale != 1 || now.yScale != 1 || !now.translation.isNull()) {
            // Scaled or moved by an effect: somewhere around where it is, but that's all that's known.
            m_changedBelow = Region::infinite();
        } else {
            m_changedBelow |= (look->second.geometry | now.geometry).roundedOut();
        }
    }
    look->second = now;
}

void GlassEffect::prePaintWindow(RenderView *view, EffectWindow *w, WindowPrePaintData &data)
{
    if (m_awaitingGlass.contains(w)) {
        data.setTranslucent();
    }
    if (auto it = m_windows.find(w); it != m_windows.end() && !appearState(it->second).done) {
        data.setTranslucent();
        data.setTransformed();
    }
    if (auto it = m_windows.find(w); it != m_windows.end() && it->second.dim) {
        data.setTranslucent();
    }
    effects->prePaintWindow(view, w, data);
    if (data.mask & PAINT_WINDOW_TRANSFORMED) {
        m_animatedNow.insert(w);
    }
    // Moved, resized or animated by another effect, the window looks different without being
    // damaged. Its own picture is drawn again every frame until it's settled, then once more, with
    // its backgrounds looked for in that, or a frame from in between would stay.
    if (auto it = m_windows.find(w); it != m_windows.end() && it->second.keyed) {
        KeyedWindow &keyed = *it->second.keyed;
        const RectF frame = w->frameGeometry();
        if ((data.mask & PAINT_WINDOW_TRANSFORMED) || frame != keyed.lastFrame) {
            keyed.settling.start();
            keyed.lastFrame = frame;
        }
        if (keyed.settling.isValid()) {
            keyed.dirty = true;
            if (keyed.settling.elapsed() >= s_keyedSettleMs) {
                keyed.settling.invalidate();
                keyed.looked.invalidate();
            }
        }
    }
}

PaintResult GlassEffect::paintWindow(const RenderTarget &renderTarget, const RenderViewport &viewport, EffectWindow *w, int mask, const Region &deviceRegion, WindowPaintData &data)
{
    if (auto it = m_awaitingGlass.find(w); it != m_awaitingGlass.end()) {
        if (it->second.elapsed() < s_awaitGlassMs) {
            data.multiplyOpacity(0);
        } else {
            m_awaitingGlass.erase(it);
        }
    }
    if (auto it = m_windows.find(w); it != m_windows.end() && it->second.appear.kind != 0) {
        const AppearState state = appearState(it->second);
        if (!state.done || it->second.disappearing.isValid()) {
            const QPointF origin = it->second.appear.origin;
            data.multiplyOpacity(state.opacity);
            data.setXScale(data.xScale() * state.scale);
            data.setYScale(data.yScale() * state.scale);
            data.translate(origin.x() * (1 - state.scale) + state.slide * w->width(), origin.y() * (1 - state.scale));
        }
    }
    return effects->paintWindow(renderTarget, viewport, w, mask, deviceRegion, data);
}

void GlassEffect::postPaintScreen()
{
    // Letting go of a closed window can delete it, which changes m_windows, so that waits for the
    // end of the loop.
    if (!m_awaitingGlass.empty()) {
        effects->addRepaintFull();
    }
    // A settling keyed window needs frames until it's settled, whether anything else asks for them.
    for (const auto &[window, glass] : m_windows) {
        if (glass.keyed && glass.keyed->settling.isValid()) {
            effects->addRepaint(window->expandedGeometry());
        }
    }
    std::vector<EffectWindowDeletedRef> finished;
    for (auto &[window, glass] : m_windows) {
        if (!appearState(glass).done) {
            effects->addRepaintFull();
        } else if (glass.disappearing.isValid() && glass.deletedRef) {
            finished.push_back(std::move(*glass.deletedRef));
            glass.deletedRef.reset();
            effects->addRepaintFull();
        }
    }
    finished.clear();
    // A closing window only repaints itself, but glass reaches a little past it at a fractional
    // scale. Without this, that row of pixels stays as it was until the window's gone.
    for (const auto &[window, glass] : m_windows) {
        if (glass.closing && !glass.lastBounds.isEmpty()) {
            const Rect bounds = glass.lastBounds;
            effects->addRepaint(Rect(bounds.x() - s_edgeRepaint, bounds.y() - s_edgeRepaint, bounds.width() + 2 * s_edgeRepaint, bounds.height() + 2 * s_edgeRepaint));
        }
    }
    effects->postPaintScreen();
}

PaintResult GlassEffect::drawWindow(const RenderTarget &renderTarget, const RenderViewport &viewport, EffectWindow *w, int mask, const Region &deviceRegion, WindowPaintData &data)
{
    const bool onScreen = &renderTarget == m_screenTarget;
    const bool reblurred = paintGlass(renderTarget, viewport, w, deviceRegion, data, onScreen && m_windows.contains(w) && behindChanged(w));
    if (onScreen) {
        noteDrawn(w, data, reblurred);
    }
    // The glass is behind the window as it is; the window itself lets some of it through. On its way
    // to the dock and back, it has no glass, and nothing shows through it either.
    if (auto it = m_windows.find(w); it != m_windows.end() && it->second.dim && !isMinimizing(w)) {
        if (it->second.keyed && &renderTarget == m_screenTarget
            && drawKeyed(renderTarget, viewport, w, deviceRegion, data, *it->second.keyed)) {
            return MAKSHELL_PAINTED;
        }
        data.multiplyOpacity(m_settings.windowOpacity);
    }
    return effects->drawWindow(renderTarget, viewport, w, mask, deviceRegion, data);
}

KeyedWindow::~KeyedWindow()
{
    QObject::disconnect(damaged);
}

bool GlassEffect::drawKeyed(const RenderTarget &renderTarget, const RenderViewport &viewport, EffectWindow *w, const Region &deviceRegion, const WindowPaintData &data, KeyedWindow &keyed)
{
    // Drawn by itself the way KWin's offscreen effects do it, shadow and all, at its screen's scale.
    const qreal textureScale = w->screen()->scale();
    const RectF expanded = snapToPixels(w->expandedGeometry(), textureScale);
    const QSize textureSize = (expanded.size() * textureScale).toSize();
    if (textureSize.isEmpty()) {
        return false;
    }
    if (!keyed.texture || keyed.texture->size() != textureSize) {
        keyed.framebuffer.reset();
        keyed.texture = GLTexture::allocate(GL_RGBA8, textureSize);
        if (!keyed.texture) {
            return false;
        }
        keyed.texture->setFilter(GL_LINEAR);
        keyed.texture->setWrapMode(GL_CLAMP_TO_EDGE);
        keyed.framebuffer = std::make_unique<GLFramebuffer>(keyed.texture.get());
        keyed.dirty = true;
        // Changing size, which starts with its shadow going when it's maximized, before it moves
        // or anything animates it: its backgrounds are looked for once that's over.
        keyed.settling.start();
    }
    const RectF frameInTexture = RectF(w->frameGeometry()).translated(-expanded.topLeft());
    if (data.crossFadeProgress() != keyed.crossFade) {
        keyed.crossFade = data.crossFadeProgress();
        keyed.dirty = true;
        // Half one picture and half the other, it has no backgrounds of its own to look for.
        keyed.settling.start();
    }
    if (keyed.dirty) {
        RenderTarget target(keyed.framebuffer.get());
        RenderViewport targetViewport(expanded, textureScale, target, QPoint());
        GLFramebuffer::pushFramebuffer(keyed.framebuffer.get());
        glClearColor(0.0, 0.0, 0.0, 0.0);
        glClear(GL_COLOR_BUFFER_BIT);
        WindowPaintData plain;
        plain.setOpacity(1.0);
        // KWin's cross-fade (maximizing's, for one) draws the window's new picture only once it's
        // under way: without this, only the old one, stretched, would show.
        plain.setCrossFadeProgress(data.crossFadeProgress());
        (void)effects->drawWindow(target, targetViewport, w, PAINT_WINDOW_TRANSFORMED | PAINT_WINDOW_TRANSLUCENT, Region::infinite(), plain);
        GLFramebuffer::popFramebuffer();
        keyed.dirty = false;
        if (!keyed.settling.isValid() && (!keyed.looked.isValid() || keyed.looked.elapsed() >= s_keyLookMs)) {
            const Rect frameDevice = RectF(frameInTexture.x() * textureScale, frameInTexture.y() * textureScale,
                                           frameInTexture.width() * textureScale, frameInTexture.height() * textureScale)
                                         .rounded();
            findBackgrounds(keyed, frameDevice);
        }
    }
    const double scale = viewport.scale();
    const RectF expandedOnScreen = snapToPixels(w->expandedGeometry(), scale);
    const RectF frameOnScreen = snapToPixels(w->frameGeometry(), scale);
    RectF visible = expandedOnScreen;
    visible.moveTopLeft(expandedOnScreen.topLeft() - frameOnScreen.topLeft());
    WindowQuad quad;
    quad[0] = WindowVertex(visible.topLeft(), QPointF(0, 0));
    quad[1] = WindowVertex(visible.topRight(), QPointF(1, 0));
    quad[2] = WindowVertex(visible.bottomRight(), QPointF(1, 1));
    quad[3] = WindowVertex(visible.bottomLeft(), QPointF(0, 1));

    RenderGeometry geometry;
    geometry.appendWindowQuad(quad, scale);
    const QMatrix4x4 textureMatrix = keyed.texture->matrix(NormalizedCoordinates);
    geometry.postProcessTextureCoordinates(textureMatrix);

    GLVertexBuffer *vbo = GLVertexBuffer::streamingBuffer();
    vbo->reset();
    vbo->setAttribLayout(std::span(GLVertexBuffer::GLVertex2DLayout), sizeof(GLVertex2D));
    const auto map = vbo->map<GLVertex2D>(geometry.size());
    if (!map) {
        return false;
    }
    geometry.copy(*map);
    vbo->unmap();
    vbo->bindArrays();

    ShaderBinder binder(m_keyedShader.get());
    GLShader *shader = m_keyedShader.get();
    const qreal rgb = data.brightness() * data.opacity();
    const qreal alpha = data.opacity();
    QMatrix4x4 mvp = viewport.projectionMatrix();
    mvp.translate(std::round(w->x() * scale), std::round(w->y() * scale));
    const auto toXYZ = renderTarget.colorDescription()->containerColorimetry().toXYZ();
    shader->setUniform(GLShader::Mat4Uniform::ModelViewProjectionMatrix, mvp * data.toMatrix(scale));
    shader->setUniform(GLShader::Vec4Uniform::ModulationConstant, QVector4D(rgb, rgb, rgb, alpha));
    shader->setUniform(GLShader::FloatUniform::Saturation, data.saturation());
    shader->setUniform(GLShader::Vec3Uniform::PrimaryBrightness, QVector3D(toXYZ(1, 0), toXYZ(1, 1), toXYZ(1, 2)));
    shader->setColorspaceUniforms(ColorDescription::sRGB, renderTarget.colorDescription(), RenderingIntent::Perceptual);

    // Where the window is in the texture, in the coordinates it's sampled with.
    const QPointF first = textureMatrix.map(QPointF(frameInTexture.left() / expanded.width(), frameInTexture.top() / expanded.height()));
    const QPointF second = textureMatrix.map(QPointF(frameInTexture.right() / expanded.width(), frameInTexture.bottom() / expanded.height()));
    shader->setUniform("frame", QVector4D(std::min(first.x(), second.x()), std::min(first.y(), second.y()),
                                          std::max(first.x(), second.x()), std::max(first.y(), second.y())));
    static constexpr const char *keyUniforms[s_maxKeys] = {"keys[0]", "keys[1]", "keys[2]", "keys[3]"};
    for (int i = 0; i < s_maxKeys; ++i) {
        shader->setUniform(keyUniforms[i], i < keyed.keys.size() ? keyed.keys[i] : QVector3D());
    }
    shader->setUniform("keyCount", int(std::min<qsizetype>(keyed.keys.size(), s_maxKeys)));
    const int mode = keyed.clearByItself                                                 ? 0
        : m_settings.windowBackgroundsOnly > 0.5f && !keyed.keys.isEmpty() ? 1
                                                                           : 2;
    shader->setUniform("mode", mode);
    shader->setUniform("backgroundOpacity", m_settings.windowOpacity);

    const bool clipping = deviceRegion != Region::infinite();
    const Region clipRegion = clipping ? viewport.transform().map(deviceRegion, renderTarget.transformedSize()) : Region::infinite();
    if (clipping) {
        glEnable(GL_SCISSOR_TEST);
    }
    glEnable(GL_BLEND);
    glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
    keyed.texture->bind();
    vbo->draw(clipRegion, GL_TRIANGLES, 0, geometry.count(), clipping);
    keyed.texture->unbind();
    glDisable(GL_BLEND);
    if (clipping) {
        glDisable(GL_SCISSOR_TEST);
    }
    vbo->unbindArrays();
    return true;
}

void GlassEffect::findBackgrounds(KeyedWindow &keyed, const Rect &frame)
{
    keyed.looked.start();
    if (!m_keySampleTexture) {
        m_keySampleTexture = GLTexture::allocate(GL_RGBA8, s_keySampleSize);
        if (!m_keySampleTexture) {
            return;
        }
        m_keySampleFramebuffer = std::make_unique<GLFramebuffer>(m_keySampleTexture.get());
    }
    // Nearest, so that each sample is a pixel of the window as it is, not text blended into what's
    // around it.
    GLFramebuffer::pushFramebuffer(keyed.framebuffer.get());
    m_keySampleFramebuffer->blitFromFramebuffer(frame & Rect(QPoint(), keyed.texture->size()), Rect(QPoint(), s_keySampleSize), GL_NEAREST);
    GLFramebuffer::popFramebuffer();
    const QImage image = m_keySampleTexture->toImage().convertToFormat(QImage::Format_RGBA8888);

    // The colors that cover the most of it, a few steps apart, and how much of it is see-through.
    struct Bin {
        int count = 0;
        double red = 0, green = 0, blue = 0;
    };
    std::unordered_map<int, Bin> bins;
    int counted = 0;
    int clear = 0;
    for (int y = 0; y < image.height(); ++y) {
        const uchar *line = image.constScanLine(y);
        for (int x = 0; x < image.width(); ++x) {
            const uchar *pixel = line + x * 4;
            // What's partly see-through is the window's own background, if it's see-through by itself,
            // or its rounded corners, or a little of its shadow.
            if (pixel[3] < 250) {
                clear++;
                continue;
            }
            Bin &bin = bins[(pixel[0] >> 3) << 10 | (pixel[1] >> 3) << 5 | (pixel[2] >> 3)];
            bin.count++;
            bin.red += pixel[0];
            bin.green += pixel[1];
            bin.blue += pixel[2];
            counted++;
        }
    }
    keyed.clearByItself = clear > s_clearShare * (clear + counted);
    std::vector<Bin> found;
    for (const auto &[key, bin] : bins) {
        if (bin.count >= counted * s_keyShare) {
            found.push_back(bin);
        }
    }
    std::sort(found.begin(), found.end(), [](const Bin &a, const Bin &b) {
        return a.count > b.count;
    });
    keyed.keys.clear();
    for (const Bin &bin : found) {
        if (keyed.keys.size() == s_maxKeys) {
            break;
        }
        keyed.keys.append(QVector3D(bin.red / bin.count, bin.green / bin.count, bin.blue / bin.count) / 255.0f);
    }
}

bool GlassEffect::ensureTextures(ShapeRender &render, const QSize &size, GLenum format, int levels)
{
    if (int(render.textures.size()) == levels && render.textures[0]->size() == size && render.textures[0]->internalFormat() == format) {
        return true;
    }
    render.framebuffers.clear();
    render.textures.clear();
    for (int i = 0; i < levels; ++i) {
        auto texture = GLTexture::allocate(format, QSize(std::max(1, size.width() >> i), std::max(1, size.height() >> i)));
        if (!texture) {
            qCWarning(MAKSHELL_GLASS) << "failed to allocate a glass texture";
            render.textures.clear();
            render.framebuffers.clear();
            return false;
        }
        texture->setFilter(GL_LINEAR);
        texture->setWrapMode(GL_CLAMP_TO_EDGE);
        auto framebuffer = std::make_unique<GLFramebuffer>(texture.get());
        if (!framebuffer->valid()) {
            qCWarning(MAKSHELL_GLASS) << "failed to create a glass framebuffer";
            render.textures.clear();
            render.framebuffers.clear();
            return false;
        }
        render.textures.push_back(std::move(texture));
        render.framebuffers.push_back(std::move(framebuffer));
    }
    return true;
}

/// Uploads a quad covering a whole `size` texture, for offscreen passes.
static void uploadQuad(GLVertexBuffer *vbo, const QSize &size)
{
    vbo->reset();
    vbo->setAttribLayout(std::span(GLVertexBuffer::GLVertex2DLayout), sizeof(GLVertex2D));
    if (auto result = vbo->map<GLVertex2D>(6)) {
        auto map = *result;
        const float w = size.width();
        const float h = size.height();
        map[0] = GLVertex2D{.position = QVector2D(0, 0), .texcoord = QVector2D(0, 1)};
        map[1] = GLVertex2D{.position = QVector2D(w, h), .texcoord = QVector2D(1, 0)};
        map[2] = GLVertex2D{.position = QVector2D(0, h), .texcoord = QVector2D(0, 0)};
        map[3] = GLVertex2D{.position = QVector2D(0, 0), .texcoord = QVector2D(0, 1)};
        map[4] = GLVertex2D{.position = QVector2D(w, 0), .texcoord = QVector2D(1, 1)};
        map[5] = GLVertex2D{.position = QVector2D(w, h), .texcoord = QVector2D(1, 0)};
        vbo->unmap();
    }
}

void GlassEffect::frost(ShapeRender &render, int passes, float offset)
{
    // Dual Kawase, as in KWin's blur: halve the background each pass, then double it back up. All
    // the way down, then back up to the first level past the blur's own for the wide blur; then
    // from the blur's own last level, which that didn't touch, up to level 1 for the blur.
    GLVertexBuffer *vbo = GLVertexBuffer::streamingBuffer();
    uploadQuad(vbo, render.textures[0]->size());
    vbo->bindArrays();

    QMatrix4x4 projection;
    projection.ortho(QRectF(QPointF(), QSizeF(render.textures[0]->size())));

    ShaderManager::instance()->pushShader(m_downsampleShader.get());
    m_downsampleShader->setUniform(GLShader::Mat4Uniform::ModelViewProjectionMatrix, projection);
    m_downsampleShader->setUniform("offset", offset);
    for (size_t i = 1; i < render.framebuffers.size(); ++i) {
        GLTexture *read = render.textures[i - 1].get();
        m_downsampleShader->setUniform("halfpixel", QVector2D(0.5 / read->width(), 0.5 / read->height()));
        read->bind();
        GLFramebuffer::pushFramebuffer(render.framebuffers[i].get());
        vbo->draw(GL_TRIANGLES, 0, 6);
        GLFramebuffer::popFramebuffer();
    }
    ShaderManager::instance()->popShader();

    ShaderManager::instance()->pushShader(m_upsampleShader.get());
    m_upsampleShader->setUniform(GLShader::Mat4Uniform::ModelViewProjectionMatrix, projection);
    m_upsampleShader->setUniform("offset", offset);
    const auto upsample = [&](size_t from, size_t to) {
        for (size_t i = from; i > to; --i) {
            GLTexture *read = render.textures[i].get();
            m_upsampleShader->setUniform("halfpixel", QVector2D(0.5 / read->width(), 0.5 / read->height()));
            read->bind();
            GLFramebuffer::pushFramebuffer(render.framebuffers[i - 1].get());
            vbo->draw(GL_TRIANGLES, 0, 6);
            GLFramebuffer::popFramebuffer();
        }
    };
    upsample(render.framebuffers.size() - 1, passes + 1);
    upsample(passes, 1);
    ShaderManager::instance()->popShader();

    vbo->unbindArrays();
}

float GlassEffect::measureLuminance(ShapeRender &render, bool linear)
{
    if (!m_probeTexture) {
        m_probeTexture = GLTexture::allocate(GL_RGBA8, s_probeSize);
        if (!m_probeTexture) {
            return -1;
        }
        m_probeTexture->setFilter(GL_LINEAR);
        m_probeFramebuffer = std::make_unique<GLFramebuffer>(m_probeTexture.get());
    }

    // Each probe pixel averages a few background pixels; spread over the whole shape, that's
    // plenty to tell light from dark.
    GLFramebuffer::pushFramebuffer(render.framebuffers[0].get());
    m_probeFramebuffer->blitFromFramebuffer(Rect(QPoint(), render.textures[0]->size()), Rect(QPoint(), s_probeSize));
    GLFramebuffer::popFramebuffer();

    const QImage image = m_probeTexture->toImage().convertToFormat(QImage::Format_RGBA8888);
    double total = 0;
    for (int y = 0; y < image.height(); ++y) {
        const uchar *line = image.constScanLine(y);
        for (int x = 0; x < image.width(); ++x) {
            float rgb[3];
            for (int c = 0; c < 3; ++c) {
                const float value = line[x * 4 + c] / 255.0f;
                // A float target holds linear light already; 8-bit ones hold sRGB.
                rgb[c] = linear ? value : srgbToLinear(value);
            }
            total += 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
        }
    }
    return total / (image.width() * image.height());
}

bool GlassEffect::paintGlass(const RenderTarget &renderTarget, const RenderViewport &viewport, EffectWindow *w, const Region &deviceRegion, const WindowPaintData &data, bool behindChanged)
{
    auto it = m_windows.find(w);
    if (it == m_windows.end()) {
        return false;
    }
    bool reblurred = false;
    // Gone once the surface is, while the window finishes closing.
    GlassSurface *glass = w->surface() ? m_manager->glassFor(w->surface()) : nullptr;
    const QList<Shape> &shapes = it->second.shapes;
    // What's past the shapes is kept for a moment, for shapes that come back.
    std::vector<ShapeRender> &renders = it->second.render[m_currentView];
    if (renders.size() < size_t(shapes.size()) || renders.size() > size_t(shapes.size()) + s_spareRenders) {
        renders.resize(shapes.size() + (renders.size() < size_t(shapes.size()) ? 0 : s_spareRenders));
    }
    // There's a reference into this while spares are added to it, so it mustn't have to move.
    renders.reserve(shapes.size() + s_spareRenders);

    const GLenum format = renderTarget.texture() ? renderTarget.texture()->internalFormat() : GL_RGBA8;
    const bool linear = format == GL_RGBA16F || format == GL_RGBA32F;
    const QPointF offset = w->bufferGeometry().topLeft() - w->frameGeometry().topLeft();
    const float opacity = w->opacity() * data.opacity();
    const float scale = std::min(data.xScale(), data.yScale());
    // On its way to the dock and back, a window has no glass, as on Tahoe, and nothing shows through
    // it: one that's see-through by itself, like a terminal, is backed by little more than the color
    // of what was behind it.
    const bool minimizing = isMinimizing(w);

    for (qsizetype i = 0; i < shapes.size(); ++i) {
        const Shape &shape = shapes[i];
        const bool probe = glass && i == 0 && glass->tracksBrightness();
        if (shape.style == Style::Transparent && !probe) {
            continue;
        }

        // The shape in global logical coordinates, following the window's transform like blur.
        RectF local = RectF(Rect(shape.rect)).translated(offset);
        local = RectF(local.x() * data.xScale() + data.xTranslation(),
                      local.y() * data.yScale() + data.yTranslation(),
                      local.width() * data.xScale(),
                      local.height() * data.yScale());
        const RectF logical = local.translated(w->pos());
        const Rect deviceRect = viewport.mapToDeviceCoordinatesAligned(logical);
        if (deviceRect.isEmpty()) {
            continue;
        }

        // An app's window has its own background to tint what's behind, so this is only the blur.
        const bool appWindow = shape.style == Style::AppWindow;
        const bool frosted = shape.style == Style::Frosted || isDialog(shape.style) || appWindow;
        const bool control = shape.style == Style::Control;
        ShapeRender &render = renders[i];
        // Controls sit on a backdrop, which has done the blurring for them.
        const bool blurred = shape.style != Style::Clear && shape.style != Style::Transparent && !control;
        const int passes = frosted ? m_settings.frostPasses : m_settings.blurPasses;
        // Beyond the blur's own levels: four more halvings for the wide blur.
        const int wideLevel = passes + 1;
        const int levels = blurred ? wideLevel + 4 : 1;
        const Rect logicalBounds = logical.roundedOut();
        // Nothing behind it here to read, so it keeps what it has, which may be a different size.
        const bool offscreen = &renderTarget != m_screenTarget;
        if (offscreen && render.textures.empty()) {
            continue;
        }
        // A shape coming or going shifts the ones after it, and setting up textures is slow enough
        // to miss frames, so textures that fit go with the shape they fit.
        const auto fits = [&](const ShapeRender &candidate) {
            return int(candidate.textures.size()) == levels && candidate.textures[0]->size() == deviceRect.size() && candidate.textures[0]->internalFormat() == format;
        };
        bool swapped = false;
        if (!offscreen && !fits(render)) {
            for (size_t other = i + 1; other < renders.size(); ++other) {
                if (fits(renders[other])) {
                    std::swap(render, renders[other]);
                    swapped = true;
                    break;
                }
            }
            // What it has may be what another shape wants in a moment.
            if (!swapped && !render.textures.empty() && renders.size() < size_t(shapes.size()) + s_spareRenders) {
                ShapeRender kept;
                std::swap(render, kept);
                renders.push_back(std::move(kept));
            }
        }
        const bool fresh = swapped || render.textures.empty() || render.textures[0]->size() != deviceRect.size();
        if (!offscreen && !ensureTextures(render, deviceRect.size(), format, levels)) {
            continue;
        }

        // Copy the part of the background that was just repainted; the rest is cached from
        // earlier frames. Copying more would pick up last frame's glass, and copying less, which
        // going through logical coordinates does at a fractional scale, leaves lines behind wherever
        // the repainted area is split: rows that are never copied, and stay black in new textures.
        const Region repainted = offscreen ? Region()
            : deviceRegion == Region::infinite()
            ? Region(deviceRect)
            : deviceRegion & deviceRect;
        const OutputTransform transform = renderTarget.texture() ? renderTarget.texture()->contentTransform() : OutputTransform();
        if (transform == OutputTransform::Normal) {
            for (const Rect &dirty : repainted.rects()) {
                render.framebuffers[0]->blitFromFramebuffer(dirty, dirty.translated(-deviceRect.topLeft()));
            }
        } else {
            // A turned screen can only be copied by way of logical coordinates.
            const Region logicalDirty = viewport.mapFromDeviceCoordinatesContained(repainted) & logicalBounds;
            for (const Rect &dirty : logicalDirty.rects()) {
                const Rect destination = viewport.mapToDeviceCoordinatesAligned(dirty).translated(-deviceRect.topLeft());
                render.framebuffers[0]->blitFromRenderTarget(renderTarget, viewport, dirty, destination);
            }
        }
        if (!offscreen && fresh && repainted.boundingRect() != deviceRect) {
            // New textures start out empty; fill them in on the next frame.
            effects->addRepaint(logicalBounds);
        }
        // Whether that copied any of the background for the first time.
        bool filledMore = false;
        if (!offscreen) {
            if (fresh) {
                render.filled = Region();
            }
            const Region copied = repainted.translated(-deviceRect.topLeft());
            filledMore = !(copied - render.filled).isEmpty();
            render.filled |= copied;
        }

        if (probe && !offscreen) {
            if (const int wait = glass->msUntilProbe(); wait == 0) {
                const float luminance = measureLuminance(render, linear);
                if (luminance >= 0) {
                    glass->reportBrightness(luminance);
                }
            } else if (!it->second.probeScheduled) {
                // Measure again once it's due, in case nothing else repaints by then.
                it->second.probeScheduled = true;
                QTimer::singleShot(wait, this, [this, window = QPointer<EffectWindow>(w), logicalBounds]() {
                    if (auto it = m_windows.find(window.data()); window && it != m_windows.end()) {
                        it->second.probeScheduled = false;
                    }
                    effects->addRepaint(logicalBounds);
                });
            }
        }
        if (shape.style == Style::Transparent) {
            continue;
        }

        // Blurring is most of the work, and what's behind is mostly as it was, even when the glass is
        // drawn again for what's in front. Now and then it's blurred anyway, in case a change went
        // unnoticed.
        const bool reblur = behindChanged || filledMore || fresh || render.blurredAt != deviceRect
            || !render.blurred.isValid() || render.blurred.elapsed() >= s_reblurAnywayMs;
        if (blurred && !offscreen && reblur) {
            frost(render, passes, frosted ? m_settings.frostOffset : m_settings.blurOffset);
            render.blurredAt = deviceRect;
            render.blurred.start();
            reblurred = true;
        }

        // Draw where the window is being repainted, in device pixels relative to the shape.
        const Region deviceDirty = deviceRegion == Region::infinite() ? Region(deviceRect) : deviceRegion & deviceRect;
        if (deviceDirty.isEmpty()) {
            continue;
        }
        GLVertexBuffer *vbo = GLVertexBuffer::streamingBuffer();
        vbo->reset();
        vbo->setAttribLayout(std::span(GLVertexBuffer::GLVertex2DLayout), sizeof(GLVertex2D));
        const int vertexCount = deviceDirty.rects().size() * 6;
        if (auto result = vbo->map<GLVertex2D>(vertexCount)) {
            auto map = *result;
            size_t index = 0;
            const float width = deviceRect.width();
            const float height = deviceRect.height();
            for (const Rect &rect : deviceDirty.rects()) {
                const RectF r = RectF(rect.translated(-deviceRect.topLeft()));
                const auto vertex = [&](float x, float y) {
                    return GLVertex2D{.position = QVector2D(x, y), .texcoord = QVector2D(x / width, 1.0f - y / height)};
                };
                map[index++] = vertex(r.left(), r.top());
                map[index++] = vertex(r.right(), r.bottom());
                map[index++] = vertex(r.left(), r.bottom());
                map[index++] = vertex(r.left(), r.top());
                map[index++] = vertex(r.right(), r.top());
                map[index++] = vertex(r.right(), r.bottom());
            }
            vbo->unmap();
        } else {
            qCWarning(MAKSHELL_GLASS) << "failed to map the vertex buffer";
            continue;
        }
        vbo->bindArrays();

        QMatrix4x4 projection = viewport.projectionMatrix();
        // The projection is in global device coordinates, and the rectangle is relative to the target.
        const QPointF targetOrigin = viewport.renderRect().topLeft() * viewport.scale();
        projection.translate(deviceRect.x() + targetOrigin.x(), deviceRect.y() + targetOrigin.y());

        const bool liquid = shape.style == Style::Liquid;
        const bool bubble = shape.style == Style::Bubble;
        const bool chip = shape.style == Style::Chip;
        const bool backdrop = shape.style == Style::Backdrop;
        const float saturation = control ? m_settings.controlSaturation : liquid ? m_settings.liquidSaturation : blurred ? m_settings.frostSaturation : 1.0f;
        const float tintAlpha = appWindow                          ? 0.0f
            : shape.style == Style::WindowInactive                 ? m_settings.inactiveTint
            : frosted || shape.style == Style::Card                  ? m_settings.frostTint
            : bubble                                                 ? m_settings.bubbleTint
            : control                                                ? m_settings.controlTint
                                                                     : 0.0f;
        ShaderManager::instance()->pushShader(m_glassShader.get());
        m_glassShader->setUniform(GLShader::Mat4Uniform::ModelViewProjectionMatrix, projection);
        m_glassShader->setUniform("geoSize", QVector2D(logical.width(), logical.height()));
        m_glassShader->setUniform("boxSize", shape.box.isEmpty() ? QVector2D(logical.width(), logical.height()) : QVector2D(shape.box.width() * data.xScale(), shape.box.height() * data.yScale()));
        m_glassShader->setUniform("boxOffset", QVector2D(shape.boxOffset.x() * data.xScale(), shape.boxOffset.y() * data.yScale()));
        m_glassShader->setUniform("rotation", shape.rotation);
        static constexpr const char *dropletUniforms[4] = {"droplets[0]", "droplets[1]", "droplets[2]", "droplets[3]"};
        for (int droplet = 0; droplet < 4; ++droplet) {
            const QVector4D value = droplet < shape.droplets.size() ? shape.droplets[droplet] : QVector4D();
            // From the middle of the shape, rather than of what it's drawn in.
            m_glassShader->setUniform(dropletUniforms[droplet],
                                      QVector4D(value.x() - float(shape.boxOffset.x()), value.y() - float(shape.boxOffset.y()), value.z(), value.w()));
        }
        m_glassShader->setUniform("cornerRadius", (shape.boxRadius >= 0 ? shape.boxRadius : float(shape.radius)) * scale);
        m_glassShader->setUniform("deviceScale", float(viewport.scale()));
        m_glassShader->setUniform("wide", 1);
        m_glassShader->setUniform("sharp", 2);
        m_glassShader->setUniform("brightness", backdrop ? m_settings.backdropBrightness : liquid ? m_settings.liquidBrightness : 1.0f);
        m_glassShader->setUniform("colorMatrix", saturationMatrix(saturation));
        // Measured from Tahoe: menus have a dark tint with a hint of purple, under the edge, and its
        // small buttons a light grey one.
        const QVector3D tintColor = chip   ? QVector3D(0.21, 0.23, 0.27)
            : control                       ? QVector3D(m_settings.controlTintGrey, m_settings.controlTintGrey, m_settings.controlTintGrey)
                                            : QVector3D(0.09, 0.06, 0.1);
        m_glassShader->setUniform("tint", QVector4D(tintColor, chip ? m_settings.chipTint : tintAlpha));
        m_glassShader->setUniform("rim", control ? m_settings.controlRim : chip ? m_settings.chipRim : 0.0f);
        m_glassShader->setUniform("rimDirectional", control ? 1.0f : 0.0f);
        m_glassShader->setUniform("rimInner", 1.0f);
        m_glassShader->setUniform("rimSoft", control ? 1.0f : 0.0f);
        const bool lens = control || liquid || bubble || shape.style == Style::Card;
        m_glassShader->setUniform("thickness", lens ? m_settings.glassThickness : 0.0f);
        m_glassShader->setUniform("bezel", m_settings.glassBezel);
        m_glassShader->setUniform("shoulder", m_settings.glassShoulder);
        m_glassShader->setUniform("profile", m_settings.glassProfile);
        m_glassShader->setUniform("ior", m_settings.glassIor);
        m_glassShader->setUniform("iorSpread", m_settings.glassIorSpread);
        m_glassShader->setUniform("gloss", m_settings.glassGloss);
        m_glassShader->setUniform("grain", blurred || control ? m_settings.grain : 0.0f);
        m_glassShader->setUniform("rimTint", m_settings.rimTint);
        m_glassShader->setUniform("softEdges", shape.softEdges);
        m_glassShader->setUniform("tail", QVector3D(shape.tail.x() * data.xScale(), shape.tail.y() * data.xScale(), shape.tail.z() * data.yScale()));
        m_glassShader->setUniform("edgeFade", frosted ? 0.0f : m_settings.liquidEdgeFade);
        m_glassShader->setUniform("edgeGain", bubble ? m_settings.bubbleEdgeGain : frosted ? 1.0f : m_settings.liquidEdgeGain);
        m_glassShader->setUniform("edgeWhite", frosted && !appWindow ? m_settings.frostEdgeWhite : 0.0f);
        m_glassShader->setUniform("outerAlpha", frosted && !appWindow ? m_settings.frostOuterEdge : 0.0f);
        m_glassShader->setUniform("opacity", opacity * shape.opacity);

        glActiveTexture(GL_TEXTURE2);
        render.textures[0]->bind();
        glActiveTexture(GL_TEXTURE1);
        render.textures[blurred ? wideLevel : 0]->bind();
        glActiveTexture(GL_TEXTURE0);
        render.textures[minimizing ? render.textures.size() - 1 : chip ? wideLevel : blurred ? 1 : 0]->bind();
        glEnable(GL_BLEND);
        glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
        vbo->draw(GL_TRIANGLES, 0, vertexCount);
        glDisable(GL_BLEND);
        glActiveTexture(GL_TEXTURE2);
        glBindTexture(GL_TEXTURE_2D, 0);
        glActiveTexture(GL_TEXTURE1);
        glBindTexture(GL_TEXTURE_2D, 0);
        glActiveTexture(GL_TEXTURE0);
        ShaderManager::instance()->popShader();
        vbo->unbindArrays();
    }
    return reblurred;
}

bool GlassEffect::isActive() const
{
    return m_valid && (!m_windows.empty() || !m_awaitingGlass.empty()) && !effects->isScreenLocked();
}

bool GlassEffect::blocksDirectScanout() const
{
    return false;
}

}

#include "moc_glasseffect.cpp"
