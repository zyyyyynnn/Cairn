use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let root_dir = Path::new(&manifest_dir).parent().unwrap().parent().unwrap();
    let icon_path = root_dir.join("assets").join("icons").join("cairn.ico");
    println!("cargo:rerun-if-changed={}", icon_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        let out_dir = env::var("OUT_DIR").unwrap_or_default();
        let rc_path = Path::new(&out_dir).join("cairnd.rc");
        let res_path = Path::new(&out_dir).join("cairnd.res");

        let mut res = winres::WindowsResource::new();
        if icon_path.exists() {
            res.set_icon(icon_path.to_str().unwrap());
        }
        res.set("FileDescription", "Cairn Service");
        res.set("ProductName", "Cairn");
        res.set("CompanyName", "Cairn Contributors");
        res.set("OriginalFilename", "cairnd.exe");
        res.set("LegalCopyright", "Copyright (c) 2026 Cairn Contributors");

        if let Err(e) = res.write_resource_file(&rc_path) {
            eprintln!("cargo:warning=Failed to write resource file: {}", e);
            return;
        }

        let rc_exe = find_rc_exe().unwrap_or_else(|| PathBuf::from("rc.exe"));
        let status = Command::new(&rc_exe)
            .arg(format!("/fo{}", res_path.display()))
            .arg(&rc_path)
            .status();

        match status {
            Ok(s) if s.success() => {
                // Link resource ONLY into the cairnd binary, NEVER into the library (which would pollute downstream crates like xtask)
                println!("cargo:rustc-link-arg-bin=cairnd={}", res_path.display());
            }
            Ok(s) => {
                eprintln!("cargo:warning=rc.exe failed with exit code: {:?}", s.code());
            }
            Err(e) => {
                eprintln!("cargo:warning=Failed to run rc.exe: {}", e);
            }
        }
    }
}

fn find_rc_exe() -> Option<PathBuf> {
    let roots = [
        r"C:\Program Files (x86)\Windows Kits\10\bin",
        r"C:\Program Files\Windows Kits\10\bin",
    ];
    for root in &roots {
        if let Ok(entries) = std::fs::read_dir(root) {
            let mut versions: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir() && p.join("x64").join("rc.exe").exists())
                .collect();
            versions.sort();
            if let Some(latest) = versions.pop() {
                return Some(latest.join("x64").join("rc.exe"));
            }
        }
    }
    None
}
