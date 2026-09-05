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
  makeFontsConf,
  makeWrapper,
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
  fontsConf = makeFontsConf {
    fontDirectories = [ ];
    includes = [ "${fontconfig.out}/etc/fonts/conf.d" ];
  };
in
  rustPlatform.buildRustPackage {
    pname = "glyphst-reader";
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
      "glyphst-reader"
    ];

    nativeBuildInputs = [
      clang
      gnumake
      makeWrapper
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

    postInstall = ''
      mkdir -p $out/share/glyphst-reader/pdf-translation
      cp -R crates/pdf-translation/assets/. $out/share/glyphst-reader/pdf-translation/
      wrapProgram $out/bin/glyphst-reader \
        --set FONTCONFIG_FILE ${fontsConf} \
        --set GLYPHST_PDF_TRANSLATION_ASSETS $out/share/glyphst-reader/pdf-translation
      install -Dm644 crates/reader/glyphst-reader.desktop $out/share/applications/glyphst-reader.desktop
    '';

    meta = {
      description = "A GPUI-based PDF and EPUB reader";
      license = lib.licenses.agpl3Only;
      mainProgram = "glyphst-reader";
      platforms = lib.platforms.linux;
    };
  }
