//! Windows only: the app icon and version information in
//! `glyphst-reader.exe`, so it shows in Explorer, the taskbar, the Start
//! menu and Alt+Tab. Elsewhere this does nothing.
//!
//! A missing resource compiler is a warning, so a cross build still links,
//! unless `GLYPHST_REQUIRE_WINRES` is set (`script/bundle` sets it).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=resources/app-icon/glyphst-reader.ico");
    println!("cargo:rerun-if-env-changed=GLYPHST_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // A same-day release's number: 26.10.25+1 is released as 26.10.25.1.
    let version = env!("CARGO_PKG_VERSION").replace('+', ".");
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("resources/app-icon/glyphst-reader.ico")
        .set("ProductName", "Glyphst Reader")
        .set("FileDescription", "Glyphst Reader")
        .set("ProductVersion", &version)
        .set("FileVersion", &version)
        .set("OriginalFilename", "glyphst-reader.exe")
        .set("InternalName", "glyphst-reader");
    if let Err(error) = resource.compile() {
        if std::env::var_os("GLYPHST_REQUIRE_WINRES").is_some() {
            println!("cargo::error=embedding the Windows resources failed: {error}");
            return;
        }
        println!("cargo:warning=glyphst-reader.exe built without its icon and version: {error}");
    }
}
