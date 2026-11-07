{
  description = "makshell: a macOS Tahoe-style desktop for KWin";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  # macOS Tahoe's app icons for every app, which the shell builds for the apps a user has
  # (crates/shell/src/icons.rs). install.sh fetches the same commit, read from flake.lock.
  inputs.macos-like = {
    url = "github:Scrimas/MacOS-Like/838dfd392b24d2e5886b6618bee63f84328baa52";
    flake = false;
  };

  outputs =
    { nixpkgs, macos-like, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      # A "makshell" login session: programs.makshell.enable = true;
      nixosModules.default = import ./nix/module.nix { inherit macos-like; };

      packages = forAllSystems (pkgs: {
        makshell = pkgs.callPackage ./nix/makshell.nix { inherit macos-like; };
        kwin-makshell = pkgs.kdePackages.callPackage ./kwin/package.nix { };
      });

      devShells = forAllSystems (
        pkgs:
        let
          # Loaded with dlopen at runtime by wgpu and the Wayland client stack.
          runtimeLibs = with pkgs; [
            wayland
            libxkbcommon
            vulkan-loader
            libGL
          ];
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              clippy
              rustfmt
              rust-analyzer
              pkg-config
            ];
            buildInputs = runtimeLibs;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
          };
        }
      );
    };
}
