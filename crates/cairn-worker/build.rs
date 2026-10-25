use std::path::Path;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let root = Path::new(&manifest_dir).parent().unwrap().parent().unwrap();
    let icon_path = root.join("assets").join("icons").join("cairn.ico");
    println!("cargo:rerun-if-changed={}", icon_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        let mut res = winres::WindowsResource::new();
        if icon_path.exists() {
            res.set_icon(icon_path.to_str().unwrap());
        }
        res.set("FileDescription", "Cairn Service Worker");
        res.set("ProductName", "Cairn");
        res.set("CompanyName", "Cairn Contributors");
        res.set("OriginalFilename", "cairn-worker.exe");
        res.set("LegalCopyright", "Copyright (c) 2026 Cairn Contributors");
        if let Err(e) = res.compile() {
            eprintln!("cargo:warning=Failed to compile Windows resource: {}", e);
        }
    }
}
