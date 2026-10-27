use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HLOCAL, LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
    WaitForSingleObject,
};
use windows::core::{HSTRING, w};

/// Owns the process-wide single-instance mutex for as long as it lives.
pub struct SingleInstance {
    // Held only to keep the named mutex alive.
    _mutex: OwnedHandle,
    activate_event: OwnedHandle,
}

/// Result of trying to become the primary instance.
pub enum Acquire {
    /// This process is the primary instance.
    Primary(SingleInstance),
    /// Another instance already runs; it has been asked to show itself.
    AlreadyRunning,
}

impl SingleInstance {
    /// If another instance for `app_id` is already running (even elevated),
    /// signals its activation event and returns `true` without taking ownership.
    pub fn try_activate_existing(app_id: &str) -> bool {
        let event_name = HSTRING::from(format!(r"Local\{app_id}.Activate"));
        // SAFETY: valid wide string; handle is closed immediately after signalling.
        unsafe {
            if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, &event_name) {
                let event = OwnedHandle(event);
                let _ = SetEvent(event.0);
                return true;
            }
        }
        false
    }

    /// Tries to become the only running instance for `app_id`.
    ///
    /// If another instance is already running, signals it to activate and
    /// returns [`Acquire::AlreadyRunning`]. Uses a Medium-IL permissive DACL so
    /// a non-elevated second launch in Explorer can wake an elevated primary.
    pub fn acquire(app_id: &str) -> windows::core::Result<Acquire> {
        let mutex_name = HSTRING::from(format!(r"Local\{app_id}.SingleInstance"));
        let event_name = HSTRING::from(format!(r"Local\{app_id}.Activate"));

        // SAFETY: names and optional security descriptor live across the calls;
        // handles and SDDL buffer are freed via RAII wrappers.
        unsafe {
            let sd = MediumIntegritySd::new();
            let attrs = sd.as_attributes();
            let attrs_ptr = attrs.as_ref().map(|a| a as *const SECURITY_ATTRIBUTES);

            let mutex = match CreateMutexW(attrs_ptr, true, &mutex_name) {
                Ok(h) => {
                    if GetLastError() == ERROR_ALREADY_EXISTS {
                        drop(OwnedHandle(h));
                        let _ = Self::try_activate_existing(app_id);
                        return Ok(Acquire::AlreadyRunning);
                    }
                    OwnedHandle(h)
                }
                Err(err) if err.code() == ERROR_ACCESS_DENIED.to_hresult() => {
                    let _ = Self::try_activate_existing(app_id);
                    return Ok(Acquire::AlreadyRunning);
                }
                Err(err) => return Err(err),
            };

            let activate_event = OwnedHandle(CreateEventW(attrs_ptr, false, false, &event_name)?);
            Ok(Acquire::Primary(Self {
                _mutex: mutex,
                activate_event,
            }))
        }
    }

    /// Calls `on_activate` on a background thread whenever a second instance
    /// is launched.
    pub fn listen_for_activation(&self, on_activate: impl Fn() + Send + 'static) {
        let event = self.activate_event.0.0 as isize;
        std::thread::Builder::new()
            .name("single-instance".into())
            .spawn(move || {
                let event = HANDLE(event as *mut _);
                loop {
                    // SAFETY: the event handle outlives the process's UI and is
                    // only closed when SingleInstance drops at exit.
                    let result = unsafe { WaitForSingleObject(event, INFINITE) };
                    if result.0 != 0 {
                        break;
                    }
                    on_activate();
                }
            })
            .expect("spawn single-instance listener");
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own this handle and close it exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Grants access to SYSTEM, Administrators and Interactive Users at Medium
/// Integrity or higher so a normal Explorer launch can signal an elevated tray
/// instance.
struct MediumIntegritySd(PSECURITY_DESCRIPTOR);

impl MediumIntegritySd {
    unsafe fn new() -> Self {
        let mut psd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: valid SDDL constant; allocated buffer is freed in Drop.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;IU)S:(ML;;NW;;;ME)"),
                SDDL_REVISION_1,
                &mut psd,
                None,
            )
            .is_ok()
        };
        Self(if ok { psd } else { PSECURITY_DESCRIPTOR::default() })
    }

    fn as_attributes(&self) -> Option<SECURITY_ATTRIBUTES> {
        (!self.0.0.is_null()).then_some(SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: false.into(),
        })
    }
}

impl Drop for MediumIntegritySd {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0.0)));
            }
        }
    }
}
