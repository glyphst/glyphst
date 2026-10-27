{
  lib,
  stdenv,
  craneLib,
  rustPlatform,
  pkg-config,
  clang,
  dbus,
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
  udev,
  vulkan-loader,
  wayland,
}: let
  workspaceCargo = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  runtimeLibraries = [
    # rfd's file dialogs dlopen libdbus to reach the xdg portal.
    dbus
    # winit loads it at run time.
    libxkbcommon
    vulkan-loader
    wayland
    libva
    libglvnd
  ];
  fontsConf = makeFontsConf {
    fontDirectories = [];
    includes = ["${fontconfig.out}/etc/fonts/conf.d"];
  };
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../.cargo
      ../Cargo.lock
      ../Cargo.toml
      ../crates
    ];
  };
  # crane vendors every Cargo.lock source in its own directory and fetches Git
  # dependencies (craft-libs, merman) with builtins.fetchGit.
  # mupdf-sys copies its bundled MuPDF with fs::copy, which keeps the store's
  # read-only mode, and then patches fitz/font.c in place. Make each copy
  # owner-writable.
  writableMupdfCopies = buildRs: ''
    substituteInPlace ${buildRs} --replace-fail \
      'fs::copy(&src_path, &dst_path)' \
      'fs::copy(&src_path, &dst_path).and_then(|_| { let mut perms = fs::metadata(&dst_path)?.permissions(); let mode = std::os::unix::fs::PermissionsExt::mode(&perms) | 0o200; std::os::unix::fs::PermissionsExt::set_mode(&mut perms, mode); fs::set_permissions(&dst_path, perms) })'
  '';
  cargoVendorDir = craneLib.vendorCargoDeps {
    inherit src;
    # From crates.io: the package is the crate.
    overrideVendorCargoPackage = p: drv:
      if p.name == "mupdf-sys"
      then drv.overrideAttrs (_: {postPatch = writableMupdfCopies "build.rs";})
      else drv;
    # From Git (mupdf-rs main until a release has #277): the checkout is the
    # repository.
    overrideVendorGitCheckout = ps: drv:
      if lib.any (p: p.name == "mupdf-sys") ps
      then drv.overrideAttrs (_: {postPatch = writableMupdfCopies "mupdf-sys/build.rs";})
      else drv;
  };
  commonArgs = {
    pname = "glyphst-reader";
    version = workspaceCargo.workspace.package.version;
    strictDeps = true;

    inherit src cargoVendorDir;

    # Shipped builds use the thin-LTO profile; `release` stays quick locally.
    CARGO_PROFILE = "dist";
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
      udev
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
        install -Dm644 crates/reader/glyphst-reader-url.desktop $out/share/applications/glyphst-reader-url.desktop
        mkdir -p $out/share/icons
        cp -R crates/reader/resources/app-icon/hicolor $out/share/icons/
      '';

      meta = {
        description = "A PDF and EPUB reader on egui and wgpu";
        license = lib.licenses.agpl3Only;
        mainProgram = "glyphst-reader";
        platforms = lib.platforms.linux;
      };
    })
