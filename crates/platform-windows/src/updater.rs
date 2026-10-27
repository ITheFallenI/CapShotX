//! GitHub Releases auto-updater using native WinHTTP, pure-Rust SHA-256
//! verification, and in-place Windows executable swap (`<exe>` -> `<exe>.old`).

use std::path::{Path, PathBuf};

use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen,
    WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

/// GitHub repository `owner/name` checked for releases.
pub const GITHUB_REPO: &str = "ITheFallenI/CapShotX";

/// Metadata for a newer release found on GitHub Releases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// Clean version string without leading `v`, e.g. `"0.2.0"`.
    pub version: String,
    /// Original git tag, e.g. `"v0.2.0"`.
    pub tag_name: String,
    /// HTML page for the release on GitHub.
    pub html_url: String,
    /// Direct download URL for the standalone `CapShotX.exe` asset.
    pub exe_download_url: String,
    /// Expected lowercase hex SHA-256 digest of `CapShotX.exe`, if provided by GitHub.
    pub exe_sha256: Option<String>,
    /// Size of `CapShotX.exe` in bytes.
    pub exe_size: u64,
}

struct WinHttpHandle(*mut core::ffi::c_void);

impl Drop for WinHttpHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: valid HINTERNET handle returned by WinHttp*.
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// Removes `<current_exe>.old` left behind by a previous self-update.
pub fn cleanup_old_exe() {
    if let Ok(exe) = std::env::current_exe() {
        let old = old_exe_path(&exe);
        if old.exists() {
            let _ = std::fs::remove_file(&old);
        }
        let staging = staging_exe_path(&exe);
        if staging.exists() {
            let _ = std::fs::remove_file(&staging);
        }
    }
}

fn old_exe_path(exe: &Path) -> PathBuf {
    let mut os = exe.as_os_str().to_os_string();
    os.push(".old");
    PathBuf::from(os)
}

fn staging_exe_path(exe: &Path) -> PathBuf {
    let mut os = exe.as_os_str().to_os_string();
    os.push(".new");
    PathBuf::from(os)
}

/// Queries `https://api.github.com/repos/ITheFallenI/CapShotX/releases/latest`
/// and returns `Ok(Some(ReleaseInfo))` when a newer version than `current_version`
/// is published, or `Ok(None)` when already up to date.
pub fn check_for_update(current_version: &str) -> Result<Option<ReleaseInfo>, String> {
    let api_url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let body = https_get(&api_url, 2 * 1024 * 1024)?;
    let text = String::from_utf8(body).map_err(|e| format!("Invalid UTF-8 from GitHub API: {e}"))?;
    parse_latest_release_json(&text, current_version)
}

/// Downloads the updated `CapShotX.exe`, verifies its PE header and SHA-256
/// digest, atomically swaps the running executable on disk, and relaunches
/// CapShotX (`--wait-restart`).
pub fn download_and_install_update(release: &ReleaseInfo) -> Result<(), String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Could not resolve current executable path: {e}"))?;

    // Cap download at 64 MB for safety.
    let bytes = https_get(&release.exe_download_url, 64 * 1024 * 1024)?;
    if bytes.len() < 1024 || &bytes[..2] != b"MZ" {
        return Err("Downloaded update is not a valid Windows executable.".to_owned());
    }

    if let Some(expected) = &release.exe_sha256 {
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!(
                "Update SHA-256 verification failed (expected {expected}, got {actual})."
            ));
        }
    }

    let staging = staging_exe_path(&current_exe);
    let old = old_exe_path(&current_exe);

    let _ = std::fs::remove_file(&staging);
    std::fs::write(&staging, &bytes)
        .map_err(|e| format!("Could not write staged update {}: {e}", staging.display()))?;

    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(&current_exe, &old) {
        let _ = std::fs::remove_file(&staging);
        return Err(format!(
            "Could not rename running executable {}: {e}",
            current_exe.display()
        ));
    }

    if let Err(e) = std::fs::rename(&staging, &current_exe) {
        // Roll back immediately so the existing binary remains intact.
        let _ = std::fs::rename(&old, &current_exe);
        let _ = std::fs::remove_file(&staging);
        return Err(format!(
            "Could not activate updated executable {}: {e}",
            current_exe.display()
        ));
    }

    // If running from an installed copy, keep the Installed Apps version string in sync.
    if let (Some(install_dir), Some(exe_dir)) =
        (crate::setup_ops::existing_install_dir(), current_exe.parent())
        && paths_equal(&install_dir, exe_dir)
    {
        let uninstaller = install_dir.join("Uninstall CapShotX.exe");
        let size_kb = (bytes.len() as u32 / 1024).max(1);
        let _ = crate::setup_ops::register_uninstaller(
            &install_dir,
            &current_exe,
            &uninstaller,
            &release.version,
            size_kb,
        );
    }

    std::process::Command::new(&current_exe)
        .arg("--wait-restart")
        .current_dir(current_exe.parent().unwrap_or_else(|| Path::new(".")))
        .spawn()
        .map_err(|e| format!("Updated binary installed, but relaunch failed: {e}"))?;

    Ok(())
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .eq_ignore_ascii_case(b.to_string_lossy().trim_end_matches(['\\', '/']))
}

/// Performs an HTTPS `GET` request via WinHTTP (following HTTPS redirects automatically).
pub fn https_get(url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let (host, port, path) = split_https_url(url)?;
    let host_w = HSTRING::from(&host);
    let path_w = HSTRING::from(&path);

    // SAFETY: WinHTTP API calls use valid wide strings and RAII handle wrappers.
    unsafe {
        let session = WinHttpOpen(
            w!("CapShotX-Updater/1.0"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err("WinHttpOpen failed.".to_owned());
        }
        let session = WinHttpHandle(session);
        let _ = WinHttpSetTimeouts(session.0, 10_000, 15_000, 30_000, 60_000);

        let connect = WinHttpConnect(session.0, PCWSTR(host_w.as_ptr()), port, 0);
        if connect.is_null() {
            return Err(format!("Could not connect to {host}."));
        }
        let connect = WinHttpHandle(connect);

        let request = WinHttpOpenRequest(
            connect.0,
            w!("GET"),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
             std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if request.is_null() {
            return Err(format!("Could not open HTTPS request for {url}."));
        }
        let request = WinHttpHandle(request);

        let headers = w!("Accept: application/vnd.github+json, application/octet-stream, */*\r\nX-GitHub-Api-Version: 2022-11-28\r\n");
        WinHttpSendRequest(
            request.0,
            Some(headers.as_wide()),
            None,
            0,
            0,
            0,
        )
        .map_err(|e| format!("Network request failed: {e}"))?;

        WinHttpReceiveResponse(request.0, std::ptr::null_mut())
            .map_err(|e| format!("Failed to receive server response: {e}"))?;

        let mut status_code: u32 = 0;
        let mut status_size = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status_code as *mut u32 as *mut _),
            &mut status_size,
            std::ptr::null_mut(),
        )
        .map_err(|e| format!("Failed to read HTTP status: {e}"))?;

        if status_code != 200 {
            return Err(format!("GitHub returned HTTP {status_code} for {url}."));
        }

        let mut out = Vec::new();
        loop {
            let mut avail: u32 = 0;
            WinHttpQueryDataAvailable(request.0, &mut avail)
                .map_err(|e| format!("Error checking response stream: {e}"))?;
            if avail == 0 {
                break;
            }
            let chunk_len = avail.min(64 * 1024) as usize;
            if out.len() + chunk_len > max_bytes {
                return Err("Response exceeded maximum allowed download size.".to_owned());
            }
            let start = out.len();
            out.resize(start + chunk_len, 0);
            let mut read: u32 = 0;
            WinHttpReadData(
                request.0,
                out[start..].as_mut_ptr() as *mut _,
                chunk_len as u32,
                &mut read,
            )
            .map_err(|e| format!("Error reading response stream: {e}"))?;
            out.truncate(start + read as usize);
            if read == 0 {
                break;
            }
        }
        Ok(out)
    }
}

fn split_https_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("Only https:// URLs are supported (got {url})"))?;
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_owned(),
            p.parse::<u16>().map_err(|_| format!("Invalid port in {url}"))?,
        ),
        None => (authority.to_owned(), 443),
    };
    if host.is_empty() {
        return Err(format!("Missing host in {url}"));
    }
    Ok((host, port, path.to_owned()))
}

/// Parses GitHub's `/releases/latest` JSON payload without requiring `serde` in `platform-windows`.
pub fn parse_latest_release_json(
    json: &str,
    current_version: &str,
) -> Result<Option<ReleaseInfo>, String> {
    let tag_name = extract_json_string(json, "tag_name")
        .ok_or_else(|| "GitHub release JSON did not contain 'tag_name'.".to_owned())?;
    let remote_version = normalize_version(&tag_name);
    if !is_newer_version(current_version, &remote_version) {
        return Ok(None);
    }

    let html_url = extract_json_string(json, "html_url")
        .unwrap_or_else(|| format!("https://github.com/{GITHUB_REPO}/releases/latest"));

    // Locate the `CapShotX.exe` portable asset object inside `"assets": [...]`.
    let (exe_download_url, exe_sha256, exe_size) = find_asset(json, "CapShotX.exe")
        .ok_or_else(|| format!("Release {tag_name} does not contain a CapShotX.exe asset."))?;

    Ok(Some(ReleaseInfo {
        version: remote_version,
        tag_name,
        html_url,
        exe_download_url,
        exe_sha256,
        exe_size,
    }))
}

fn find_asset(json: &str, asset_name: &str) -> Option<(String, Option<String>, u64)> {
    let needle = format!("\"name\":\"{asset_name}\"");
    let needle_spaced = format!("\"name\": \"{asset_name}\"");
    let pos = json.find(&needle).or_else(|| json.find(&needle_spaced))?;

    // Bound search to the enclosing `{ ... }` object of this asset entry.
    let obj_start = json[..pos].rfind('{')?;
    let obj_end = find_matching_brace(&json[obj_start..])? + obj_start;
    let slice = &json[obj_start..=obj_end];

    let url = extract_json_string(slice, "browser_download_url")?;
    let digest = extract_json_string(slice, "digest").and_then(|d| {
        d.strip_prefix("sha256:")
            .map(str::to_owned)
            .or_else(|| (d.len() == 64).then_some(d))
    });
    let size = extract_json_u64(slice, "size").unwrap_or(0);
    Some((url, digest, size))
}

fn find_matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;
    for (i, b) in s.bytes().enumerate() {
        if in_str {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let mut search_from = 0;
    while let Some(rel) = json[search_from..].find(&pat) {
        let after_key = search_from + rel + pat.len();
        let rest = json[after_key..].trim_start();
        let Some(rest) = rest.strip_prefix(':') else {
            search_from = after_key;
            continue;
        };
        let rest = rest.trim_start();
        if rest.starts_with("null") {
            return None;
        }
        let Some(rest) = rest.strip_prefix('"') else {
            search_from = after_key;
            continue;
        };
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => {
                    if let Some(esc) = chars.next() {
                        match esc {
                            '"' | '\\' | '/' => out.push(esc),
                            'n' => out.push('\n'),
                            'r' => out.push('\r'),
                            't' => out.push('\t'),
                            _ => out.push(esc),
                        }
                    }
                }
                other => out.push(other),
            }
        }
        return None;
    }
    None
}

fn extract_json_u64(json: &str, key: &str) -> Option<u64> {
    let pat = format!("\"{key}\"");
    let idx = json.find(&pat)?;
    let rest = json[idx + pat.len()..].trim_start().strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

pub fn normalize_version(tag: &str) -> String {
    tag.trim().trim_start_matches(['v', 'V']).to_owned()
}

/// Returns `true` if `remote` is strictly newer than `current` (semver `major.minor.patch`).
pub fn is_newer_version(current: &str, remote: &str) -> bool {
    parse_semver_tuple(remote) > parse_semver_tuple(current)
}

fn parse_semver_tuple(v: &str) -> (u32, u32, u32) {
    let clean = normalize_version(v);
    let core = clean.split('-').next().unwrap_or(&clean);
    let mut parts = core.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// Standard FIPS 180-4 SHA-256 implementation returning a 64-character lowercase hex string.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64) * 8;
    let mut padded = data.to_vec();
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = String::with_capacity(64);
    for word in h {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{word:08x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_nist_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn compares_versions_correctly() {
        assert!(is_newer_version("0.1.0", "v0.1.1"));
        assert!(is_newer_version("0.1.0", "0.2.0"));
        assert!(is_newer_version("0.1.9", "v1.0.0"));
        assert!(!is_newer_version("0.1.0", "v0.1.0"));
        assert!(!is_newer_version("0.2.0", "v0.1.9"));
    }

    #[test]
    fn parses_github_release_json() {
        let sample = r#"{
            "tag_name": "v0.2.0",
            "html_url": "https://github.com/ITheFallenI/CapShotX/releases/tag/v0.2.0",
            "assets": [
                {
                    "name": "CapShotX-Setup.exe",
                    "size": 11226624,
                    "digest": "sha256:4e99d4f728730a30a99c57ff71c19e65becead807c0363205180b379d333c1ca",
                    "browser_download_url": "https://github.com/ITheFallenI/CapShotX/releases/download/v0.2.0/CapShotX-Setup.exe"
                },
                {
                    "name": "CapShotX.exe",
                    "size": 10731008,
                    "digest": "sha256:9591741321848623582b1928ad9829b106b5fb2ce1e99aee23ab7dd806e8b7ae",
                    "browser_download_url": "https://github.com/ITheFallenI/CapShotX/releases/download/v0.2.0/CapShotX.exe"
                }
            ]
        }"#;

        let info = parse_latest_release_json(sample, "0.1.0")
            .unwrap()
            .expect("should detect newer release");
        assert_eq!(info.version, "0.2.0");
        assert_eq!(info.tag_name, "v0.2.0");
        assert_eq!(
            info.exe_download_url,
            "https://github.com/ITheFallenI/CapShotX/releases/download/v0.2.0/CapShotX.exe"
        );
        assert_eq!(
            info.exe_sha256.as_deref(),
            Some("9591741321848623582b1928ad9829b106b5fb2ce1e99aee23ab7dd806e8b7ae")
        );
        assert_eq!(info.exe_size, 10731008);

        assert!(parse_latest_release_json(sample, "0.2.0").unwrap().is_none());
    }
}
