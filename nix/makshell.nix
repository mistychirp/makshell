# The makshell shell, with the desktop entry KWin needs to let it manage windows.
{
  lib,
  rustPlatform,
  runCommand,
  runtimeShell,
  pkg-config,
  libxkbcommon,
  wayland,
  vulkan-loader,
  libGL,
  # MacOS-Like's source (github:Scrimas/MacOS-Like), which builds the app icons.
  macos-like,
  bash,
  python3,
  librsvg,
  gtk3,
  util-linux,
  coreutils,
  findutils,
  gnused,
  gnugrep,
  gawk,
}:

let
  # setup.sh copies its base theme with shutil.copy2, which keeps the store's read-only mode, and
  # then can't write over those copies. Its own copy, made writable, isn't rebuilt with the shell.
  macosLike = runCommand "makshell-macos-like" { } ''
    cp -r ${macos-like} $out
    chmod -R u+w $out
    substituteInPlace $out/setup.sh --replace-fail \
      "            shutil.copy2(v, p)" \
      "            shutil.copy2(v, p); os.chmod(p, os.stat(p).st_mode | 0o200)"
  '';

  # What setup.sh runs: Pillow and rsvg-convert to draw, gtk-update-icon-cache and flock.
  iconTools = lib.makeBinPath [
    bash
    (python3.withPackages (ps: [ ps.pillow ]))
    librsvg
    gtk3
    util-linux
    coreutils
    findutils
    gnused
    gnugrep
    gawk
  ];
in
rustPlatform.buildRustPackage {
  pname = "makshell";
  version = "0.1.2";

  src = lib.fileset.toSource {
    root = ./..;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../assets
      ../crates
      ../vendor
      ../protocols
      ../contrib/makshell.desktop
      ../contrib/makshell-settings.desktop
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ libxkbcommon ];

  postInstall = ''
    install -Dm0644 contrib/makshell.desktop $out/share/applications/makshell.desktop
    substituteInPlace $out/share/applications/makshell.desktop \
      --replace-fail @makshell@ $out/bin/makshell
    install -Dm0644 contrib/makshell-settings.desktop $out/share/applications/makshell-settings.desktop
    substituteInPlace $out/share/applications/makshell-settings.desktop \
      --replace-fail @makshell@ $out/bin/makshell
    cat > $out/bin/makshell-settings <<EOF
    #!${runtimeShell}
    exec $out/bin/makshell settings "\$@"
    EOF
    chmod +x $out/bin/makshell-settings

    # Builds the MacOS-Like icon themes for the user's apps, which the shell runs.
    mkdir -p $out/libexec
    cat > $out/libexec/makshell-icons <<EOF
    #!${runtimeShell}
    export PATH=${iconTools}:\$PATH
    exec ${bash}/bin/bash ${macosLike}/setup.sh "\$@"
    EOF
    chmod +x $out/libexec/makshell-icons

    # KWin only reads the first desktop entry naming an executable, so a second one for this binary
    # can take window management away from the shell.
    entries=$(grep -lE "^Exec=$out/bin/makshell( |\$)" $out/share/applications/*.desktop | wc -l)
    if [ "$entries" != 1 ]; then
      echo "expected one desktop entry to run $out/bin/makshell, found $entries" >&2
      exit 1
    fi
  '';

  # wgpu and the Wayland client stack load these with dlopen. They go in the binary's runpath
  # rather than a wrapper script: KWin only grants window management to the executable its
  # desktop entry names, so the entry has to name the real binary.
  postFixup = ''
    patchelf --add-rpath ${
      lib.makeLibraryPath [
        wayland
        vulkan-loader
        libGL
        libxkbcommon
      ]
    } $out/bin/makshell
  '';

  meta = {
    description = "A macOS Tahoe-style desktop shell for KWin";
    mainProgram = "makshell";
    platforms = lib.platforms.linux;
  };
}
