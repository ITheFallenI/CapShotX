//! System-wide shortcuts via `RegisterHotKey` (no low-level keyboard hooks).
//!
//! Hotkeys are owned by a dedicated thread with its own message loop, so they
//! keep working regardless of what the UI thread is doing.

use std::fmt;
use std::str::FromStr;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
     GetKeyState, GetKeyboardState, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_APP, WM_HOTKEY, WM_QUIT,
};

/// A key combination such as `Ctrl+Shift+2` or `Ctrl+X`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
    /// Windows virtual-key code.
    pub vk: u16,
}

impl Hotkey {
    pub fn has_modifier(&self) -> bool {
        self.ctrl || self.shift || self.alt || self.win
    }

    fn modifiers(&self) -> HOT_KEY_MODIFIERS {
        let mut m = MOD_NOREPEAT;
        if self.ctrl {
            m |= MOD_CONTROL;
        }
        if self.shift {
            m |= MOD_SHIFT;
        }
        if self.alt {
            m |= MOD_ALT;
        }
        if self.win {
            m |= MOD_WIN;
        }
        m
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.ctrl, "Ctrl"),
            (self.shift, "Shift"),
            (self.alt, "Alt"),
            (self.win, "Win"),
        ] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        match key_name(self.vk) {
            Some(name) => f.write_str(&name),
            None => write!(f, "0x{:02X}", self.vk),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseHotkeyError(pub String);

impl fmt::Display for ParseHotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseHotkeyError {}

impl FromStr for Hotkey {
    type Err = ParseHotkeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut hotkey = Hotkey {
            ctrl: false,
            shift: false,
            alt: false,
            win: false,
            vk: 0,
        };
        let mut key = None;
        for part in s.split('+').map(str::trim) {
            if part.is_empty() {
                continue;
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => hotkey.ctrl = true,
                "shift" => hotkey.shift = true,
                "alt" => hotkey.alt = true,
                "win" | "super" | "meta" => hotkey.win = true,
                _ if key.is_some() => {
                    return Err(ParseHotkeyError(format!("more than one key in \"{s}\"")));
                }
                _ => {
                    key = Some(
                        vk_from_name(part)
                            .ok_or_else(|| ParseHotkeyError(format!("unknown key \"{part}\"")))?,
                    )
                }
            }
        }
        hotkey.vk = key.ok_or_else(|| ParseHotkeyError(format!("no key in \"{s}\"")))?;
        Ok(hotkey)
    }
}

const NAMED_KEYS: &[(&str, u16)] = &[
    ("Backspace", 0x08),
    ("Tab", 0x09),
    ("Clear", 0x0C),
    ("Enter", 0x0D),
    ("Pause", 0x13),
    ("CapsLock", 0x14),
    ("Escape", 0x1B),
    ("Space", 0x20),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("End", 0x23),
    ("Home", 0x24),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("PrintScreen", 0x2C),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Help", 0x2F),
    ("Menu", 0x5D),
    ("Sleep", 0x5F),
    ("Numpad0", 0x60),
    ("Numpad1", 0x61),
    ("Numpad2", 0x62),
    ("Numpad3", 0x63),
    ("Numpad4", 0x64),
    ("Numpad5", 0x65),
    ("Numpad6", 0x66),
    ("Numpad7", 0x67),
    ("Numpad8", 0x68),
    ("Numpad9", 0x69),
    ("NumpadMultiply", 0x6A),
    ("NumpadAdd", 0x6B),
    ("NumpadSubtract", 0x6D),
    ("NumpadDecimal", 0x6E),
    ("NumpadDivide", 0x6F),
    ("NumLock", 0x90),
    ("ScrollLock", 0x91),
    ("BrowserBack", 0xA6),
    ("BrowserForward", 0xA7),
    ("BrowserRefresh", 0xA8),
    ("BrowserStop", 0xA9),
    ("BrowserSearch", 0xAA),
    ("BrowserFavorites", 0xAB),
    ("BrowserHome", 0xAC),
    ("AudioVolumeMute", 0xAD),
    ("AudioVolumeDown", 0xAE),
    ("AudioVolumeUp", 0xAF),
    ("MediaTrackNext", 0xB0),
    ("MediaTrackPrevious", 0xB1),
    ("MediaStop", 0xB2),
    ("MediaPlayPause", 0xB3),
    ("LaunchMail", 0xB4),
    ("LaunchApp1", 0xB6),
    ("LaunchApp2", 0xB7),
    ("Semicolon", 0xBA),
    ("Equals", 0xBB),
    ("Comma", 0xBC),
    ("Minus", 0xBD),
    ("Period", 0xBE),
    ("Slash", 0xBF),
    ("Backtick", 0xC0),
    ("OpenBracket", 0xDB),
    ("Backslash", 0xDC),
    ("CloseBracket", 0xDD),
    ("Quote", 0xDE),
    ("Oem8", 0xDF),
    ("IntlBackslash", 0xE2),
];

/// Maps a key name (as used in settings and by egui) to a virtual-key code.
pub fn vk_from_name(name: &str) -> Option<u16> {
    let trimmed = name.trim();
    let upper = trimmed.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    if bytes.len() == 1 && (bytes[0].is_ascii_uppercase() || bytes[0].is_ascii_digit()) {
        return Some(bytes[0] as u16);
    }
    if let Some(n) = upper.strip_prefix('F').and_then(|n| n.parse::<u16>().ok())
        && (1..=24).contains(&n)
    {
        return Some(0x70 + n - 1);
    }
    if let Some(hex) = upper.strip_prefix("0X")
        && let Ok(vk) = u16::from_str_radix(hex, 16)
        && (0x08..=0xFE).contains(&vk)
        && !is_modifier_vk(vk as u8)
    {
        return Some(vk);
    }
    match upper.as_str() {
        "PLUS" => return Some(0xBB),
        "ESC" => return Some(0x1B),
        "RETURN" => return Some(0x0D),
        "ARROWLEFT" => return Some(0x25),
        "ARROWUP" => return Some(0x26),
        "ARROWRIGHT" => return Some(0x27),
        "ARROWDOWN" => return Some(0x28),
        "CONTEXTMENU" | "APPS" => return Some(0x5D),
        "MULTIPLY" => return Some(0x6A),
        "COLON" => return Some(0xBA),
        "PIPE" => return Some(0xDC),
        "QUESTIONMARK" => return Some(0xBF),
        "EXCLAMATIONMARK" => return Some(0x31),
        "OPENCURLYBRACKET" => return Some(0xDB),
        "CLOSECURLYBRACKET" => return Some(0xDD),
        "GRAVE" | "BACKQUOTE" | "`" => return Some(0xC0),
        _ => {}
    }
    NAMED_KEYS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(trimmed))
        .map(|&(_, vk)| vk)
}

pub fn key_name(vk: u16) -> Option<String> {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => Some((vk as u8 as char).to_string()),
        0x70..=0x87 => Some(format!("F{}", vk - 0x70 + 1)),
        _ => NAMED_KEYS
            .iter()
            .find(|&&(_, v)| v == vk)
            .map(|(n, _)| (*n).to_owned()),
    }
}

/// Returns `true` if `vk` is currently down in the calling thread's message queue.
pub fn is_vk_down(vk: u16) -> bool {
    // SAFETY: GetKeyState has no pointer preconditions.
    unsafe { GetKeyState(vk as i32) < 0 }
}

/// Returns `(ctrl, shift, alt, win)` from the calling thread's message queue.
pub fn current_modifiers() -> (bool, bool, bool, bool) {
    (
        is_vk_down(0x11) || is_vk_down(0xA2) || is_vk_down(0xA3),
        is_vk_down(0x10) || is_vk_down(0xA0) || is_vk_down(0xA1),
        is_vk_down(0x12) || is_vk_down(0xA4) || is_vk_down(0xA5),
        is_vk_down(0x5B) || is_vk_down(0x5C),
    )
}

fn is_modifier_vk(vk: u8) -> bool {
    matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

fn is_capturable_vk(vk: u8) -> bool {
    if is_modifier_vk(vk) {
        return false;
    }
    // Exclude mouse buttons (0x01..=0x06), unassigned/IME-internal codes, and 0xFF.
    !matches!(
        vk,
        0x00..=0x07
            | 0x0A..=0x0B
            | 0x0E..=0x0F
            | 0x15..=0x1A
            | 0x1C..=0x1F
            | 0x3A..=0x40
            | 0x5E
            | 0x88..=0x8F
            | 0x92..=0x9F
            | 0xB8..=0xB9
            | 0xC1..=0xDA
            | 0xE0..=0xE1
            | 0xE3..=0xFE
            | 0xFF
    )
}

fn read_keyboard_state() -> [bool; 256] {
    let mut raw = [0u8; 256];
    // SAFETY: 256-byte stack buffer as required by GetKeyboardState.
    let ok = unsafe { GetKeyboardState(&mut raw).is_ok() };
    let mut down = [false; 256];
    if ok {
        for (i, &b) in raw.iter().enumerate() {
            down[i] = (b & 0x80) != 0;
        }
    }
    down
}

/// Captures any key or key chord pressed on the calling UI thread using
/// `GetKeyboardState` (no keyboard hooks), including combinations like
/// `Ctrl+X`, `Ctrl+C`, `Ctrl+V`, `Win+...`, and Numpad keys that GUI toolkits
/// otherwise intercept or collapse.
pub struct KeyCapture {
    was_down: [bool; 256],
}

impl Default for KeyCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyCapture {
    pub fn new() -> Self {
        Self {
            was_down: read_keyboard_state(),
        }
    }

    /// Names of modifier keys currently held down (`Ctrl`, `Shift`, `Alt`, `Win`).
    pub fn held_modifiers(&self) -> Vec<&'static str> {
        let (ctrl, shift, alt, win) = current_modifiers();
        let mut out = Vec::with_capacity(4);
        for (on, name) in [(ctrl, "Ctrl"), (shift, "Shift"), (alt, "Alt"), (win, "Win")] {
            if on {
                out.push(name);
            }
        }
        out
    }

    /// Polls for the next non-modifier key transition from up to down.
    pub fn poll(&mut self) -> Option<Hotkey> {
        let now = read_keyboard_state();
        let ctrl = now[0x11] || now[0xA2] || now[0xA3];
        let shift = now[0x10] || now[0xA0] || now[0xA1];
        let alt = now[0x12] || now[0xA4] || now[0xA5];
        let win = now[0x5B] || now[0x5C];

        let mut triggered_vk = None;
        for vk in 0x08u8..=0xFE {
            if is_capturable_vk(vk) && now[vk as usize] && !self.was_down[vk as usize] {
                triggered_vk = Some(vk as u16);
                break;
            }
        }
        self.was_down = now;

        triggered_vk.map(|vk| Hotkey {
            ctrl,
            shift,
            alt,
            win,
            vk,
        })
    }
}

/// Why a hotkey could not be registered, phrased for users.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HotkeyError {
    AlreadyInUse,
    ServiceStopped,
    Other(String),
}

impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInUse => {
                f.write_str("Already used by Windows or another app. Choose a different shortcut.")
            }
            Self::ServiceStopped => f.write_str("The shortcut service is not running."),
            Self::Other(msg) => write!(f, "Windows refused this shortcut: {msg}"),
        }
    }
}

impl std::error::Error for HotkeyError {}

enum Request {
    Register(u32, Hotkey, Sender<Result<(), HotkeyError>>),
    Unregister(u32, Sender<()>),
}

/// Owns the hotkey thread. Dropping it unregisters every hotkey.
pub struct HotkeyService {
    thread_id: u32,
    requests: Sender<Request>,
    thread: Option<JoinHandle<()>>,
}

impl HotkeyService {
    /// Starts the service; `on_hotkey` is called on the hotkey thread with
    /// the id of each pressed hotkey.
    pub fn start(on_hotkey: impl Fn(u32) + Send + 'static) -> std::io::Result<Self> {
        let (requests, request_rx) = mpsc::channel::<Request>();
        let (id_tx, id_rx) = mpsc::channel();

        let thread = std::thread::Builder::new()
            .name("hotkeys".into())
            .spawn(move || {
                // SAFETY: standard thread message loop; RegisterHotKey with no
                // window posts WM_HOTKEY to this thread's queue.
                unsafe {
                    let mut msg = MSG::default();
                    // Forces creation of this thread's message queue.
                    let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                    let _ = id_tx.send(GetCurrentThreadId());

                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        match msg.message {
                            WM_HOTKEY => on_hotkey(msg.wParam.0 as u32),
                            WM_WAKE => {
                                for request in request_rx.try_iter() {
                                    match request {
                                        Request::Register(id, hotkey, reply) => {
                                            let _ = UnregisterHotKey(None, id as i32);
                                            let result = RegisterHotKey(
                                                None,
                                                id as i32,
                                                hotkey.modifiers(),
                                                hotkey.vk as u32,
                                            )
                                            .map_err(|err| {
                                                if err.code() == ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult() {
                                                    HotkeyError::AlreadyInUse
                                                } else {
                                                    HotkeyError::Other(err.message())
                                                }
                                            });
                                            let _ = reply.send(result);
                                        }
                                        Request::Unregister(id, reply) => {
                                            let _ = UnregisterHotKey(None, id as i32);
                                            let _ = reply.send(());
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            })?;

        let thread_id = id_rx
            .recv()
            .map_err(|_| std::io::Error::other("hotkey thread exited during startup"))?;
        Ok(Self {
            thread_id,
            requests,
            thread: Some(thread),
        })
    }

    /// Registers (or replaces) the hotkey with `id`.
    pub fn register(&self, id: u32, hotkey: Hotkey) -> Result<(), HotkeyError> {
        let (reply, rx) = mpsc::channel();
        self.send(Request::Register(id, hotkey, reply))?;
        rx.recv().map_err(|_| HotkeyError::ServiceStopped)?
    }

    pub fn unregister(&self, id: u32) {
        let (reply, rx) = mpsc::channel();
        if self.send(Request::Unregister(id, reply)).is_ok() {
            let _ = rx.recv();
        }
    }

    fn send(&self, request: Request) -> Result<(), HotkeyError> {
        self.requests
            .send(request)
            .map_err(|_| HotkeyError::ServiceStopped)?;
        // SAFETY: posting a message to our own live thread.
        unsafe { PostThreadMessageW(self.thread_id, WM_WAKE, WPARAM(0), LPARAM(0)) }
            .map_err(|_| HotkeyError::ServiceStopped)
    }
}

impl Drop for HotkeyService {
    fn drop(&mut self) {
        // SAFETY: posting WM_QUIT to our own thread ends its message loop;
        // hotkeys are released when the thread exits.
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

const WM_WAKE: u32 = WM_APP + 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats_round_trip() {
        for s in [
            "Ctrl+Shift+2",
            "Ctrl+Shift+G",
            "Ctrl+X",
            "Ctrl+C",
            "Ctrl+V",
            "Ctrl+Z",
            "Alt+F4",
            "Shift+Win+S",
            "Ctrl+PrintScreen",
            "Ctrl+Minus",
            "Ctrl+NumpadMultiply",
            "Ctrl+NumpadAdd",
            "Backtick",
            "F9",
        ] {
            let hotkey: Hotkey = s.parse().unwrap();
            assert_eq!(hotkey.to_string(), s);
        }
    }

    #[test]
    fn parsing_is_case_insensitive() {
        let hotkey: Hotkey = "ctrl + shift + g".parse().unwrap();
        assert_eq!(hotkey, "Ctrl+Shift+G".parse().unwrap());
        let ctrl_x: Hotkey = "control + x".parse().unwrap();
        assert_eq!(ctrl_x.to_string(), "Ctrl+X");
    }

    #[test]
    fn rejects_bad_input() {
        assert!("Ctrl+Shift".parse::<Hotkey>().is_err());
        assert!("Ctrl+A+B".parse::<Hotkey>().is_err());
        assert!("Ctrl+Nope".parse::<Hotkey>().is_err());
    }
}
