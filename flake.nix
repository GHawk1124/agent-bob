{
  description = "Crane and multi-target builds with rust-flake";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    flake-parts.url = "github:hercules-ci/flake-parts";
    rust-flake.url = "github:juspay/rust-flake";
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];

      imports = [
        inputs.rust-flake.flakeModules.default
        inputs.rust-flake.flakeModules.nixpkgs
        ./nix/cross-targets.nix
      ];

      perSystem =
        {
          config,
          pkgs,
          lib,
          ...
        }:
        {
          rust-project = {
            # Shared build dependencies for all crates
            defaults.perCrate.crane.args = {
              nativeBuildInputs = with pkgs; [
                pkg-config
              ];
              buildInputs = [ ];
            };
          };

          packages.default =
            let
              crateNames = lib.attrNames config.rust-project.crates;
              mainCrate = lib.head crateNames;
            in
            config.rust-project.crates.${mainCrate}.crane.outputs.drv.crate;

          # Dev shell with rust-analyzer and extras
          devShells.default = pkgs.mkShell {
            inputsFrom = [ config.rust-project.crates."agent-bob".crane.outputs.drv.crate or { } ];

            packages = with pkgs; [
              rust-analyzer
              cargo-nextest
            ];

            RUST_BACKTRACE = 1;
            RUST_LOG = "info";

            shellHook = ''
              echo "Rust $(rustc --version | cut -d' ' -f2)"
              echo "Targets: x86_64-linux (native) | windows | arm64-linux"
              echo ""
              echo "Build commands:"
              echo "  nix build               > Native build"
              echo "  nix build .#windows     > Windows .exe"
              echo "  nix build .#arm64-linux > Linux ARM64"
              echo ""
              echo "Checks:"
              echo "  nix flake check         > Run clippy on all crates"
            '';
          };
        };
    };
}
