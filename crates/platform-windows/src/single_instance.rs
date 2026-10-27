use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
    WaitForSingleObject,
};
use windows::core::HSTRING;

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
    /// Tries to become the only running instance for `app_id`.
    ///
    /// If another instance is already running, signals it to activate and
    /// returns [`Acquire::AlreadyRunning`].
    pub fn acquire(app_id: &str) -> windows::core::Result<Acquire> {
        let mutex_name = HSTRING::from(format!(r"Local\{app_id}.SingleInstance"));
        let event_name = HSTRING::from(format!(r"Local\{app_id}.Activate"));

        // SAFETY: names are valid null-terminated wide strings; handles are
        // owned and closed by OwnedHandle.
        unsafe {
            let mutex = OwnedHandle(CreateMutexW(None, true, &mutex_name)?);
            if GetLastError() == ERROR_ALREADY_EXISTS {
                if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, &event_name) {
                    let event = OwnedHandle(event);
                    let _ = SetEvent(event.0);
                }
                drop(mutex);
                return Ok(Acquire::AlreadyRunning);
            }
            let activate_event = OwnedHandle(CreateEventW(None, false, false, &event_name)?);
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
