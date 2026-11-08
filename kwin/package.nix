# The makshell KWin effect, built against nixpkgs' KWin.
{
  lib,
  stdenv,
  cmake,
  extra-cmake-modules,
  wayland-scanner,
  kwin,
  kdecoration,
  qtbase,
  qtdeclarative,
  qtwayland,
  kconfig,
  kcoreaddons,
  kglobalaccel,
  kwindowsystem,
  wayland,
  libepoxy,
  libdrm,
  vulkan-headers,
}:

stdenv.mkDerivation {
  pname = "kwin-makshell";
  version = "0.1.7";

  # The plugin reads the protocol XML from the repository root.
  src = lib.fileset.toSource {
    root = ./..;
    fileset = lib.fileset.unions [
      ./CMakeLists.txt
      ./src
      ./tabbox
      ./lockscreen
      ../protocols/makshell-glass-v1.xml
      ../assets/fonts/Inter-Regular.ttf
      ../assets/fonts/Inter-SemiBold.ttf
      ../assets/fonts/Inter-Bold.ttf
    ];
  };
  sourceRoot = "source/kwin";

  nativeBuildInputs = [
    cmake
    extra-cmake-modules
    wayland-scanner
  ];
  buildInputs = [
    kwin
    kdecoration
    qtbase
    qtdeclarative
    qtwayland
    kconfig
    kcoreaddons
    kglobalaccel
    kwindowsystem
    wayland
    libepoxy
    libdrm
    vulkan-headers
  ];

  # A KWin plugin, not an application: nothing to wrap.
  dontWrapQtApps = true;

  meta = {
    description = "Liquid glass for makshell, as a KWin effect";
    license = lib.licenses.gpl3Plus;
    platforms = lib.platforms.linux;
  };
}
