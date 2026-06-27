{
  lib,
  stdenv,
  rustPlatform,
  pkg-config,
  clang,
  libclang,
  perl,
  gnumake,
  python3,
  unzip,
  copyDesktopItems,
  makeDesktopItem,
  alsa-lib,
  fontconfig,
  freetype,
  glib,
  libdrm,
  libgbm,
  libglvnd,
  libva,
  libxkbcommon,
  libx11,
  libxcb,
  libxcomposite,
  libxcursor,
  libxdamage,
  libxext,
  libxfixes,
  libxi,
  libxrandr,
  vulkan-loader,
  wayland,
}: let
  workspaceCargo = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  runtimeLibraries = [
    vulkan-loader
    wayland
    libva
    libglvnd
  ];
in
  rustPlatform.buildRustPackage {
    pname = "glyphst";
    version = workspaceCargo.workspace.package.version;

    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../.cargo
        ../Cargo.lock
        ../Cargo.toml
        ../crates
      ];
    };

    cargoLock = {
      lockFile = ../Cargo.lock;
      allowBuiltinFetchGit = true;
    };

    cargoBuildFlags = [
      "-p"
      "glyphst"
    ];

    nativeBuildInputs = [
      clang
      copyDesktopItems
      gnumake
      perl
      pkg-config
      python3
      rustPlatform.bindgenHook
      unzip
    ];

    buildInputs = [
      alsa-lib
      fontconfig
      freetype
      glib
      libdrm
      libgbm
      libglvnd
      libva
      libx11
      libxcb
      libxcomposite
      libxcursor
      libxdamage
      libxext
      libxfixes
      libxi
      libxkbcommon
      libxrandr
      vulkan-loader
      wayland
    ];

    env.LIBCLANG_PATH = "${libclang.lib}/lib";
    NIX_LDFLAGS = "-rpath ${lib.makeLibraryPath runtimeLibraries}";
    dontPatchELF = stdenv.hostPlatform.isLinux;

    doCheck = false;

    desktopItems = [
      (makeDesktopItem {
        name = "glyphst";
        desktopName = "Glyphst";
        genericName = "PDF Viewer";
        comment = "Read PDF documents";
        exec = "glyphst %U";
        icon = "x-office-document";
        categories = [
          "Office"
          "Viewer"
        ];
        mimeTypes = ["application/pdf"];
        startupNotify = true;
        startupWMClass = "glyphst";
      })
    ];

    meta = {
      description = "A GPUI-based PDF viewer";
      license = lib.licenses.gpl3Only;
      mainProgram = "glyphst";
      platforms = lib.platforms.linux;
    };
  }
