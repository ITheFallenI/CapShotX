use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../../Icon/capshotx.ico");
    embed_windows_resources("CapShotX", "CapShotX - Screenshots & Screen Recording", "CapShotX.exe");
}

fn embed_windows_resources(product: &str, description: &str, original_filename: &str) {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = manifest_dir.join("../../Icon/capshotx.ico");
    if !ico.exists() {
        return;
    }
    let Some(rc_exe) = find_rc_exe() else {
        return;
    };

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc_path = out_dir.join("app.rc");
    let res_path = out_dir.join("app.res");
    let ico_escaped = ico.to_string_lossy().replace('\\', "\\\\");

    let rc_source = format!(
        r#"#pragma code_page(65001)
1 ICON "{ico_escaped}"
1 VERSIONINFO
FILEVERSION 0,1,0,0
PRODUCTVERSION 0,1,0,0
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "{product}\0"
            VALUE "FileDescription", "{description}\0"
            VALUE "FileVersion", "0.1.0\0"
            VALUE "InternalName", "{product}\0"
            VALUE "OriginalFilename", "{original_filename}\0"
            VALUE "ProductName", "{product}\0"
            VALUE "ProductVersion", "0.1.0\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
    );

    if std::fs::write(&rc_path, rc_source).is_err() {
        return;
    }
    let status = Command::new(rc_exe)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .status();
    if status.is_ok_and(|s| s.success()) && res_path.exists() {
        println!("cargo:rustc-link-arg-bins={}", res_path.display());
    }
}

fn find_rc_exe() -> Option<PathBuf> {
    if Command::new("rc.exe").arg("/?").output().is_ok() {
        return Some(PathBuf::from("rc.exe"));
    }
    let kits = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(kits)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path().join(r"x64\rc.exe")))
        .filter(|p| p.is_file())
        .collect();
    candidates.sort();
    candidates.pop()
}
