{
  description = "A GPUI-based PDF and EPUB reader";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      ...
    }:
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
        pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
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
          glyphstReader = pkgs.callPackage ./nix/package.nix {
            inherit rustPlatform;
          };
        in
        {
          glyphst-reader = glyphstReader;
          default = glyphstReader;
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          lib = pkgs.lib;
          rustToolchain = mkRustToolchain pkgs;
          fontsConf = pkgs.makeFontsConf {
            fontDirectories = [ ];
            includes = [ "${pkgs.fontconfig.out}/etc/fonts/conf.d" ];
          };
          buildLibraries = with pkgs; [
            fontconfig
            freetype
            libxcb
            libxkbcommon
          ];
          runtimeLibraries = with pkgs; [
            libglvnd
            vulkan-loader
            wayland
          ];
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo-audit
              mold
              pkg-config
              prek
              rust-analyzer
              rustToolchain
              unzip
            ];

            buildInputs = buildLibraries;

            FONTCONFIG_FILE = fontsConf;
            LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
            NIX_LDFLAGS = "-rpath ${lib.makeLibraryPath runtimeLibraries}";
            RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
          };
        }
      );

      formatter = forAllSystems (system: (import nixpkgs { inherit system; }).nixfmt);
    };
}
