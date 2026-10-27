use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::Compression;
use flate2::write::ZlibEncoder;

fn main() {
    println!("cargo:rerun-if-changed=../../Icon/capshotx.ico");
    println!("cargo:rerun-if-env-changed=CAPSHOTX_PAYLOAD_EXE");

    stage_payload();
    embed_windows_resources(
        "CapShotX Setup",
        "CapShotX Setup - Native Windows Installer",
        "CapShotX-Setup.exe",
    );
}

fn stage_payload() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let candidate = std::env::var_os("CAPSHOTX_PAYLOAD_EXE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("../../target/release/CapShotX.exe"));

    let raw_bytes = if candidate.is_file() {
        println!("cargo:rerun-if-changed={}", candidate.display());
        std::fs::read(&candidate).expect("failed to read CapShotX.exe payload")
    } else {
        // Allow `cargo check --workspace` / `cargo test --workspace` to succeed
        // even before a release CapShotX.exe payload has been built.
        b"STUB".to_vec()
    };

    let raw_len = raw_bytes.len();
    let zz_path = out_dir.join("CapShotX.exe.zz");
    let size_path = out_dir.join("payload_size.txt");

    let file = std::fs::File::create(&zz_path).expect("failed to create CapShotX.exe.zz");
    let mut enc = ZlibEncoder::new(file, Compression::best());
    enc.write_all(&raw_bytes)
        .expect("failed to compress CapShotX.exe payload");
    enc.finish()
        .expect("failed to finish compressing CapShotX.exe payload");

    std::fs::write(&size_path, raw_len.to_string()).expect("failed to write payload_size.txt");
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
    let rc_path = out_dir.join("installer.rc");
    let res_path = out_dir.join("installer.res");
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
            VALUE "CompanyName", "CapShotX\0"
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
