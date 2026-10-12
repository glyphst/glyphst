{
  description = "A GPUI-based PDF and EPUB reader";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      crane,
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
          craneLib = (crane.mkLib pkgs).overrideToolchain mkRustToolchain;
          glyphstReader = pkgs.callPackage ./nix/package.nix {
            inherit craneLib;
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
            # gilrs (egui-spike's gamepad input) links libudev.
            udev
          ];
          runtimeLibraries = with pkgs; [
            libglvnd
            # winit (pdf-gpu's zoom_view example) loads it at run time.
            libxkbcommon
            vulkan-loader
            wayland
          ];
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo-audit
              cargo-nextest
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
