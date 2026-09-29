{
  description = "anytui -- chat tui for any";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    # The crate uses edition 2024 and recent let-chains, which can outrun
    # nixpkgs' rustc, so the toolchain comes from the overlay, pinned like
    # everything else.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "clippy" "rustfmt" "rust-analyzer" ];
        };
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };
        manifest = (pkgs.lib.importTOML ./Cargo.toml).package;

        any-tui = rustPlatform.buildRustPackage {
          pname = manifest.name;
          version = manifest.version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          # No native deps: reqwest is built without TLS (the client only
          # talks plain HTTP to the local any server).
          meta = {
            description = manifest.description or "Terminal chat client for any";
            license = pkgs.lib.licenses.mit;
            mainProgram = "any-tui";
          };
        };
      in
      {
        # `nix build` → ./result/bin/any-tui
        packages.default = any-tui;

        # `nix run . -- --api http://127.0.0.1:7001/v1 --no-auto-read`
        apps.default = flake-utils.lib.mkApp { drv = any-tui; };

        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain  # cargo/rustc/clippy/rustfmt/rust-analyzer
          ];
        };
      });
}
