{
  lib,
  stdenv,
  craneLib,
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
  # crane vendors every Cargo.lock source in its own directory, so the crates.io
  # hayro/moxcms crates (Typst, image) and the engine's same-version forks coexist.
  # Git dependencies use builtins.fetchGit with the evaluating user's credentials,
  # which the private engine repository requires.
  commonArgs = {
    pname = "glyphst-reader";
    version = workspaceCargo.workspace.package.version;
    strictDeps = true;

    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../.cargo
        ../Cargo.lock
        ../Cargo.toml
        ../crates
      ];
    };

    cargoExtraArgs = "--locked -p glyphst-reader";
    doCheck = false;

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
  };
in
  craneLib.buildPackage (commonArgs
    // {
      cargoArtifacts = craneLib.buildDepsOnly commonArgs;

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
    })
