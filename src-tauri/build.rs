fn main() {
    use flate2::{Compression, write::DeflateEncoder};
    use std::{fs, io::Write, path::PathBuf};

    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let notices = root.join("../THIRD_PARTY_NOTICES.md");
    let license = root.join("../LICENSE");
    let fallback = root.join("licenses/MIT.txt");
    for path in [&notices, &license, &fallback] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(&fs::read(notices).expect("read third-party notices"))
        .unwrap();
    fs::write(out.join("notices.deflate"), encoder.finish().unwrap()).unwrap();
    let source = if license.exists() { license } else { fallback };
    fs::copy(source, out.join("project-license.txt")).expect("embed project license");
    tauri_build::build()
}
