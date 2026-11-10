# NixOS module: a "makshell" login session, KWin with the makshell shell and its glass plugin.
#
# Both are built from the system's own packages, so the plugin always matches the KWin it loads
# into. The session shares KWin's configuration (kwinrc, kxkbrc, shortcuts) with Plasma.
#
# The flake hands it MacOS-Like's source first: nixosModules.default = import ./nix/module.nix { ... }.
{ macos-like }:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.makshell;

  makshell = pkgs.callPackage ./makshell.nix { inherit macos-like; };
  kwin-makshell = pkgs.kdePackages.callPackage ../kwin/package.nix { };
  inherit (pkgs.kdePackages) kwin polkit-kde-agent-1;

  # Runs inside KWin for as long as the session lasts: KWin exits when this does.
  startup = pkgs.writeShellScript "makshell-startup" ''
    # Hand KWin's environment (WAYLAND_DISPLAY and friends) to systemd and D-Bus activation,
    # so portals and services started on demand can reach the session.
    ${pkgs.dbus}/bin/dbus-update-activation-environment --systemd --all
    systemctl --user start makshell-session.target
    # The window decoration. This is KWin's shared configuration, but a session without our plugin
    # falls back to its default.
    ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group org.kde.kdecoration2 --key library org.kde.makshell
    # Our genie instead of KWin's minimize animations. Plasma's own sessions share this too, and
    # without our plugin there they'd have no animation, so these are put back when this one ends.
    ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Plugins --key squashEnabled --type bool false
    ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Plugins --key magiclampEnabled --type bool false
    # Windows open at once and close with a quick fade, measured from Tahoe, rather than zooming. The
    # effect runs for four times what FadeOutTime says, on a curve that's nearly done after a third.
    ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Plugins --key scaleEnabled --type bool false
    ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Plugins --key fadeEnabled --type bool true
    # Alt+Tab is one icon for each app, like macOS's, on glass.
    ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group TabBox --key LayoutName makshell
    ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group TabBox --key ApplicationsMode 1
    # Menus are there at once, also measured from Tahoe; ours fade out by themselves.
    ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Plugins --key fadingpopupsEnabled --type bool false
    ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Effect-fade --key FadeInTime 1
    ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Effect-fade --key FadeOutTime 30
    # Switching desktops slides the windows, and the wallpaper stays, as on macOS. There's one
    # wallpaper for all of them, which would otherwise slide into a copy of itself past a black gap.
    ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Effect-slide --key SlideBackground --type bool false
    ${pkgs.dbus}/bin/dbus-send --session --type=method_call --dest=org.kde.KWin /KWin org.kde.KWin.reconfigure
    # Unlocks KWallet with the login password, which PAM kept for this.
    systemctl --user start --no-block plasma-kwallet-pam.service

    ${polkit-kde-agent-1}/libexec/polkit-kde-authentication-agent-1 &

    # Logging out: the shell quit. The apps in the session are asked to close while KWin is still there,
    # so they close rather than crash as their windows go. Then the portals stop, since without a display
    # KDE's aborts, and its crash reporter with it, over and over. Then this ends, and KWin with it, which
    # the display manager sees as the session ending. Ending it through logind instead also kills the
    # display manager's own process for the session, which SDDM takes as a crash: no login screen after.
    end_session() {
        scope=$(sed -n 's/^0:://p' /proc/$$/cgroup)
        procs=/sys/fs/cgroup$scope/cgroup.procs
        if [ -n "$scope" ] && [ -r "$procs" ]; then
            # This script, and KWin and what started it, up to the display manager, stay.
            keep=" $$ "
            pid=$kwin_pid
            while [ -n "$pid" ] && [ "$pid" -gt 1 ]; do
                keep="$keep$pid "
                pid=$(awk '{print $4}' "/proc/$pid/stat" 2>/dev/null)
            done
            apps=""
            for pid in $(cat "$procs"); do
                case "$keep" in *" $pid "*) continue ;; esac
                # KWin's own, which goes with it.
                [ "$(cat "/proc/$pid/comm" 2>/dev/null)" = Xwayland ] && continue
                kill -TERM "$pid" 2>/dev/null && apps="$apps $pid"
            done
            # Up to five seconds for them to finish.
            tries=0
            while [ -n "$apps" ] && [ "$tries" -lt 10 ]; do
                sleep 0.5
                left=""
                for pid in $apps; do
                    kill -0 "$pid" 2>/dev/null && left="$left $pid"
                done
                apps=$left
                tries=$((tries + 1))
            done
        fi
        systemctl --user stop plasma-xdg-desktop-portal-kde.service xdg-desktop-portal.service
        systemctl --user stop makshell-session.target
    }

    # The shell, restarted if it crashes, for as long as this KWin lives. If KWin itself crashes,
    # its wrapper starts a new one on the same socket, which runs a new copy of this script; this
    # copy has to bow out, or there would be two shells.
    kwin_pid=$PPID
    while kill -0 "$kwin_pid" 2>/dev/null; do
      if ${makshell}/bin/makshell; then
        ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Plugins --key squashEnabled --delete
        ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Plugins --key magiclampEnabled --delete
        ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Plugins --key scaleEnabled --delete
        ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Plugins --key fadeEnabled --delete
        ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Plugins --key fadingpopupsEnabled --delete
        ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group TabBox --key LayoutName --delete
        ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group TabBox --key ApplicationsMode --delete
        ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Effect-fade --key FadeInTime --delete
        ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Effect-fade --key FadeOutTime --delete
        ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kwinrc --group Effect-slide --key SlideBackground --delete
        end_session
        exit 0
      fi
      sleep 1
    done
  '';

  session-script = pkgs.writeShellScript "makshell-session" ''
    export XDG_SESSION_TYPE=wayland
    # KDE apps and portals behave as they do in Plasma.
    export XDG_CURRENT_DESKTOP=KDE
    export XDG_SESSION_DESKTOP=KDE
    export KDE_SESSION_VERSION=6
    # KDE's screen locker shows the lock screen of the shell package named here, which is ours, from
    # the plugin's data. Plasma's own sessions don't have this, and keep theirs.
    export PLASMA_DEFAULT_SHELL=org.makshell.desktop
    export QT_PLUGIN_PATH=${kwin-makshell}/lib/qt-6/plugins''${QT_PLUGIN_PATH:+:$QT_PLUGIN_PATH}
    # KWin finds the window switcher among its data.
    export XDG_DATA_DIRS=${kwin-makshell}/share''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}
    # The cursor, before KWin starts, since that's when it reads it. Apps that draw their own go by
    # the variables.
    export XCURSOR_THEME=macOS XCURSOR_SIZE=24
    ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kcminputrc --group Mouse --key cursorTheme macOS
    ${pkgs.kdePackages.kconfig}/bin/kwriteconfig6 --file kcminputrc --group Mouse --key cursorSize 24

    exec ${kwin}/bin/kwin_wayland_wrapper --xwayland --exit-with-session ${startup}
  '';

  session =
    (pkgs.writeTextDir "share/wayland-sessions/makshell.desktop" ''
      [Desktop Entry]
      Name=makshell
      Comment=KWin with the makshell desktop
      Exec=${session-script}
      Type=Application
      DesktopNames=KDE
    '')
    // {
      providedSessions = [ "makshell" ];
    };
in
{
  options.programs.makshell = {
    enable = lib.mkEnableOption "the makshell desktop, a login session running KWin with makshell";
  };

  config = lib.mkIf cfg.enable {
    services.displayManager.sessionPackages = [ session ];
    # A laptop's battery, in the menu bar.
    services.upower.enable = lib.mkDefault true;

    environment.systemPackages = [
      # Its desktop entry, which KWin reads to let it manage windows, has to be in the
      # system's data dirs.
      makshell
      kwin-makshell
      # tuigreet lists the sessions it finds in the system profile.
      session
      # The session's cursor, from https://github.com/ful1e5/apple_cursor
      pkgs.apple-cursor
    ];

    systemd.user.targets.makshell-session = {
      description = "makshell session";
      bindsTo = [ "graphical-session.target" ];
      wants = [ "graphical-session-pre.target" ];
      after = [ "graphical-session-pre.target" ];
    };
  };
}
