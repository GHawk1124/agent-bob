{
  inputs,
  lib,
  ...
}:
{
  perSystem =
    {
      system,
      ...
    }:
    let
      inherit (inputs) nixpkgs rust-flake flake-parts;

      # Helper to create cross-compiled packages
      mkCrossSystem =
        crossSystem:
        let
          pkgsCross = import nixpkgs {
            inherit system crossSystem;
            overlays = [ ];
          };

          # Create a separate rust-flake instance for this cross system
          crossFlake = flake-parts.lib.mkFlake { inherit inputs; } {
            systems = [ system ];
            imports = [
              rust-flake.flakeModules.default
              rust-flake.flakeModules.nixpkgs
            ];

            perSystem = {
              pkgs = pkgsCross;
              rust-project = {
                defaults.perCrate.crane.args = {
                  nativeBuildInputs = with pkgsCross; [ pkg-config ];
                  doCheck = false; # Disable tests for cross builds
                };
              };
            };
          };
        in
        crossFlake.config.perSystem system;
    in
    {
      packages = {
        # Windows cross-compile
        windows =
          let
            cross = mkCrossSystem {
              config = "x86_64-w64-mingw32";
              libc = "msvcrt";
            };
          in
          cross.packages.default or (throw "Windows build failed: check crate name");

        # ARM64 Linux cross-compile
        arm64-linux =
          let
            cross = mkCrossSystem "aarch64-linux";
          in
          cross.packages.default or (throw "ARM64 build failed: check crate name");
      };
    };
}
