# The makshell KWin effect: liquid glass for the shell, through our makshell-glass-v1 protocol.

cmake_minimum_required(VERSION 3.25)
project(kwin-makshell LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(ECM 6.0 REQUIRED NO_MODULE)
list(APPEND CMAKE_MODULE_PATH ${ECM_MODULE_PATH})

include(KDEInstallDirs)
include(KDECMakeSettings)

# Everything KWin::kwin links, which its CMake package leaves to us to find.
find_package(Qt6 REQUIRED COMPONENTS Core DBus Gui Quick Widgets)
# ECM's wrapper around qtwaylandscanner, which generates the protocol's server classes.
find_package(QtWaylandScanner REQUIRED)
find_package(KF6 REQUIRED COMPONENTS Config CoreAddons GlobalAccel)
find_package(KWin REQUIRED)
# KWin's plugin API changes between releases; the code checks this where it differs.
add_compile_definitions(MAKSHELL_KWIN_MINOR=${KWin_VERSION_MINOR})
find_package(KDecoration3 REQUIRED)

kcoreaddons_add_plugin(makshell
    SOURCES
        src/main.cpp
        src/glasseffect.cpp
        src/glassmanager.cpp
        src/arranger.cpp
        src/glass.qrc
    INSTALL_NAMESPACE "kwin/effects/plugins"
)

ecm_add_qtwayland_server_protocol(makshell
    PROTOCOL ${CMAKE_CURRENT_SOURCE_DIR}/../protocols/makshell-glass-v1.xml
    BASENAME makshell-glass-v1
)

target_link_libraries(makshell PRIVATE
    KWin::kwin
    KF6::ConfigCore
    KF6::GlobalAccel
    Qt6::Core
    Qt6::DBus
    Qt6::Gui
    Wayland::Server
)

# The genie, a separate effect since it deforms windows off-screen.
kcoreaddons_add_plugin(makshell-genie
    SOURCES
        src/genie.cpp
    INSTALL_NAMESPACE "kwin/effects/plugins"
)
target_link_libraries(makshell-genie PRIVATE
    KWin::kwin
    KF6::ConfigCore
    Qt6::Core
    Qt6::DBus
    Qt6::Gui
)

# The window decoration: traffic lights and a title bar, loaded by KWin separately from the effect.
kcoreaddons_add_plugin(makshell-decoration
    SOURCES
        src/decoration/decoration.cpp
        src/decoration/decoration.qrc
    INSTALL_NAMESPACE "org.kde.kdecoration3"
)
# KWin looks decorations up by the name of their file.
set_target_properties(makshell-decoration PROPERTIES OUTPUT_NAME "org.kde.makshell")
target_link_libraries(makshell-decoration PRIVATE
    KDecoration3::KDecoration
    KF6::ConfigCore
    KF6::CoreAddons
    Qt6::DBus
    Qt6::Gui
)

# What Alt+Tab shows, which KWin looks for among its data by name.
install(DIRECTORY tabbox/makshell DESTINATION ${KDE_INSTALL_DATADIR}/kwin-wayland/tabbox)
# The lock screen, which KDE's screen locker shows from a Plasma shell package, with the shell's fonts.
install(DIRECTORY lockscreen/org.makshell.desktop DESTINATION ${KDE_INSTALL_DATADIR}/plasma/shells)
install(FILES ../assets/fonts/Inter-Regular.ttf ../assets/fonts/Inter-SemiBold.ttf ../assets/fonts/Inter-Bold.ttf
        DESTINATION ${KDE_INSTALL_DATADIR}/plasma/shells/org.makshell.desktop/contents/fonts)
