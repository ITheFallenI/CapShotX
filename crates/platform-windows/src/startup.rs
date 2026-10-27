//! Launch-at-sign-in registration for Windows.
//!
//! Windows Explorer ignores `HKCU\...\Run` entries that require Administrator
//! elevation, while Task Scheduler `<LogonTrigger>` with `<RunLevel>HighestAvailable`
//! starts CapShotX elevated at sign-in with zero UAC prompt (including on
//! battery power and with no 72-hour timeout).
//!
//! - When `enable && run_as_admin` and elevated: registers a per-user Scheduled
//!   Task (`CapShotX`) and clears any duplicate `HKCU\...\Run` value.
//! - When `enable` and non-elevated (either `run_as_admin == false` or UAC was
//!   declined): sets `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\CapShotX`.
//! - When `!enable`: removes both the registry value and any Scheduled Task.

use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::Command;

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::core::{PCWSTR, w};

use crate::shell;

const RUN_SUBKEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE_NAME: PCWSTR = w!("CapShotX");
const TASK_NAME: &str = "CapShotX";

/// Synchronises Windows sign-in launch registration with the user's preferences
/// and the current `.exe` location.
pub fn sync(enable: bool, run_as_admin: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("Could not resolve executable path: {e}"))?;
    sync_for_exe(&exe, enable, run_as_admin)
}

/// Synchronises Windows sign-in launch registration for a specific `exe` path
/// (used by both the app itself and `CapShotX-Setup.exe`).
pub fn sync_for_exe(exe: &Path, enable: bool, run_as_admin: bool) -> Result<(), String> {
    if !enable {
        remove_run_registry()?;
        if shell::is_elevated() {
            delete_scheduled_task();
        }
        return Ok(());
    }

    if run_as_admin && shell::is_elevated() {
        remove_run_registry()?;
        if !scheduled_task_matches(exe) {
            register_scheduled_task(exe)?;
        }
        Ok(())
    } else {
        if !run_as_admin && shell::is_elevated() {
            delete_scheduled_task();
        }
        set_run_registry(exe)
    }
}

/// Removes all startup registrations (registry `Run` entry and Scheduled Task).
pub fn remove_all() {
    let _ = remove_run_registry();
    delete_scheduled_task();
}

fn format_run_command(exe: &Path) -> String {
    format!("\"{}\" --autostart", exe.display())
}

fn set_run_registry(exe: &Path) -> Result<(), String> {
    let cmd = format_run_command(exe);
    let mut wide: Vec<u16> = cmd.encode_utf16().collect();
    wide.push(0);
    let bytes = unsafe {
        // SAFETY: `wide` is contiguous `u16` data valid for this call.
        std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2)
    };

    // SAFETY: opens/creates HKCU Run key, sets REG_SZ with null terminator, closes key.
    unsafe {
        let mut hkey = HKEY::default();
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            RUN_SUBKEY,
            Some(0),
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        )
        .ok()
        .map_err(|e| format!("Could not open startup registry key: {e}"))?;
        let res = RegSetValueExW(hkey, VALUE_NAME, Some(0), REG_SZ, Some(bytes))
            .ok()
            .map_err(|e| format!("Could not write startup registry value: {e}"));
        let _ = RegCloseKey(hkey);
        res
    }
}

fn remove_run_registry() -> Result<(), String> {
    // SAFETY: opens HKCU Run key if present and deletes the CapShotX value.
    unsafe {
        let mut hkey = HKEY::default();
        let open = RegOpenKeyExW(HKEY_CURRENT_USER, RUN_SUBKEY, Some(0), KEY_SET_VALUE, &mut hkey);
        if open == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        open.ok()
            .map_err(|e| format!("Could not open startup registry key: {e}"))?;
        let del = RegDeleteValueW(hkey, VALUE_NAME);
        let _ = RegCloseKey(hkey);
        if del == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        del.ok()
            .map_err(|e| format!("Could not remove startup registry value: {e}"))
    }
}

fn current_user_id() -> Option<String> {
    let user = std::env::var("USERNAME").ok().filter(|s| !s.is_empty())?;
    match std::env::var("USERDOMAIN").ok().filter(|s| !s.is_empty()) {
        Some(domain) => Some(format!(r"{domain}\{user}")),
        None => Some(user),
    }
}

fn scheduled_task_matches(exe: &Path) -> bool {
    let Ok(out) = Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME, "/XML"])
        .creation_flags(CREATE_NO_WINDOW.0)
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let xml = String::from_utf8_lossy(&out.stdout);
    let expected = format!("<Command>{}</Command>", xml_escape(&exe.to_string_lossy()));
    xml.contains(&expected)
        && xml.contains("<Arguments>--autostart</Arguments>")
        && xml.contains("HighestAvailable")
}

fn delete_scheduled_task() {
    let _ = Command::new("schtasks")
        .args(["/Delete", "/TN", TASK_NAME, "/F"])
        .creation_flags(CREATE_NO_WINDOW.0)
        .output();
}

fn register_scheduled_task(exe: &Path) -> Result<(), String> {
    let user = current_user_id().ok_or_else(|| "Could not determine current Windows user".to_string())?;
    let xml = build_task_xml(&user, exe);

    // Task Scheduler XML expects UTF-16LE with BOM.
    let mut utf16 = Vec::with_capacity(2 + xml.len() * 2);
    utf16.extend_from_slice(&[0xFF, 0xFE]);
    for unit in xml.encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }

    let tmp = std::env::temp_dir().join(format!("CapShotX_task_{}.xml", std::process::id()));
    std::fs::write(&tmp, &utf16)
        .map_err(|e| format!("Could not write temporary task XML: {e}"))?;

    let output = Command::new("schtasks")
        .args(["/Create", "/TN", TASK_NAME, "/XML"])
        .arg(&tmp)
        .arg("/F")
        .creation_flags(CREATE_NO_WINDOW.0)
        .output();
    let _ = std::fs::remove_file(&tmp);

    let output = output.map_err(|e| format!("Could not invoke schtasks: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Task Scheduler rejected startup task: {}", stderr.trim()));
    }
    Ok(())
}

fn build_task_xml(user_id: &str, exe: &Path) -> String {
    let user = xml_escape(user_id);
    let command = xml_escape(&exe.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>--autostart</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn task_xml_and_run_command_escape_paths_and_keep_battery_unlimited_settings() {
        let exe = PathBuf::from(r"C:\Apps\R&D's Tools\CapShotX.exe");
        assert_eq!(
            format_run_command(&exe),
            "\"C:\\Apps\\R&D's Tools\\CapShotX.exe\" --autostart"
        );
        let xml = build_task_xml(r"DESKTOP\Tyrone", &exe);
        assert!(xml.contains("<Command>C:\\Apps\\R&amp;D&apos;s Tools\\CapShotX.exe</Command>"));
        assert!(xml.contains("<Arguments>--autostart</Arguments>"));
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(xml.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("<Priority>5</Priority>"));
    }
}
