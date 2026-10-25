//! Embeds `assets/` (the Typst packages translation imports), so a lone
//! executable can unpack them where the compiler reads them.

use std::{
    env, fs,
    hash::{DefaultHasher, Hasher},
    path::{Path, PathBuf},
};

fn main() {
    let assets = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets");
    println!("cargo:rerun-if-changed={}", assets.display());
    let mut files = Vec::new();
    collect(&assets, &mut files);
    files.sort();
    let mut hasher = DefaultHasher::new();
    let mut entries = String::new();
    for file in &files {
        let relative = file.strip_prefix(&assets).unwrap();
        let relative = relative
            .components()
            .map(|part| part.as_os_str().to_str().expect("asset paths are UTF-8"))
            .collect::<Vec<_>>()
            .join("/");
        hasher.write(relative.as_bytes());
        hasher.write(&fs::read(file).unwrap());
        entries.push_str(&format!(
            "    ({relative:?}, include_bytes!({:?})),\n",
            file.display().to_string()
        ));
    }
    let source = format!(
        "/// Every file under `assets/`, by its path there.\n\
         pub const ASSETS: &[(&str, &[u8])] = &[\n{entries}];\n\
         /// Names this set of assets, for the folder they unpack into.\n\
         pub const ASSETS_HASH: &str = \"{:016x}\";\n",
        hasher.finish()
    );
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("assets.rs");
    fs::write(out, source).unwrap();
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_dir() {
            collect(&path, files);
        } else {
            files.push(path);
        }
    }
}
