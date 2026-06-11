{
  description = "A GPUI-based PDF viewer";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      mkPkgs =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
      mkRustToolchain =
        pkgs:
        pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "clippy"
            "rust-src"
            "rustfmt"
          ];
        };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          rustToolchain = mkRustToolchain pkgs;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
          glyphst = pkgs.callPackage ./nix/package.nix { inherit rustPlatform; };
        in
        {
          inherit glyphst;
          default = glyphst;
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          lib = pkgs.lib;
          rustToolchain = mkRustToolchain pkgs;
          linuxLibraries = with pkgs; [
            alsa-lib
            fontconfig
            freetype
            glib
            libdrm
            libgbm
            libglvnd
            libva
            libxkbcommon
            libx11
            libxcb
            libxcomposite
            libxcursor
            libxdamage
            libxext
            libxfixes
            libxi
            libxrandr
            mupdf
            vulkan-loader
            wayland
          ];
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo-audit
              clang
              lld
              mold
              pkg-config
              prek
              rust-analyzer
              rustToolchain
              unzip
            ];

            buildInputs = linuxLibraries;

            LD_LIBRARY_PATH = lib.makeLibraryPath linuxLibraries;
            LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
            NIX_LDFLAGS = "-rpath ${
              lib.makeLibraryPath [
                pkgs.vulkan-loader
                pkgs.wayland
                pkgs.libva
                pkgs.libglvnd
              ]
            }";
            RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
          };
        }
      );

      formatter = forAllSystems (system: (import nixpkgs { inherit system; }).nixfmt);
    };
}
