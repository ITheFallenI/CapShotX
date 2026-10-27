//! Clipboard image output.

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFOHEADER};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{CF_DIB, CF_UNICODETEXT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::core::{Result, w};

/// Puts an opaque image on the clipboard as both `CF_DIB` (for apps like
/// Paint and Office) and the registered `PNG` format (for browsers, chat apps).
///
/// `bgra` is top-down, 4 bytes per pixel; `png` is the same image encoded.
/// Safe to call from any thread.
pub fn set_image(width: u32, height: u32, bgra: &[u8], png: &[u8]) -> Result<()> {
    let dib = dib_bytes(width, height, bgra);

    // SAFETY: global memory is handed to the clipboard on success and freed
    // by `set_bytes` on failure.
    unsafe {
        with_clipboard(|| {
            set_bytes(CF_DIB.0 as u32, &dib)?;
            let png_format = RegisterClipboardFormatW(w!("PNG"));
            if png_format != 0 {
                set_bytes(png_format, png)?;
            }
            Ok(())
        })
    }
}

/// Puts plain text on the clipboard (e.g. a sampled colour).
pub fn set_text(text: &str) -> Result<()> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    // SAFETY: as in `set_image`.
    unsafe {
        let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
        with_clipboard(|| set_bytes(CF_UNICODETEXT.0 as u32, bytes))
    }
}

/// Runs `f` with the clipboard opened and emptied, owned by a temporary
/// message-only window.
unsafe fn with_clipboard(f: impl FnOnce() -> Result<()>) -> Result<()> {
    // SAFETY: the owner window lives until after CloseClipboard.
    unsafe {
        // SetClipboardData fails if the clipboard was opened without an owner.
        let owner = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            None,
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )?;
        let result = (|| {
            open_with_retry(owner)?;
            let result = EmptyClipboard().and_then(|()| f());
            let _ = CloseClipboard();
            result
        })();
        let _ = DestroyWindow(owner);
        result
    }
}

/// Another app may hold the clipboard briefly; retry for ~0.5 s.
unsafe fn open_with_retry(owner: windows::Win32::Foundation::HWND) -> Result<()> {
    let mut last = Ok(());
    for _ in 0..10 {
        // SAFETY: owner is a live window created by the caller.
        last = unsafe { OpenClipboard(Some(owner)) };
        if last.is_ok() {
            return last;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    last
}

unsafe fn set_bytes(format: u32, bytes: &[u8]) -> Result<()> {
    // SAFETY: we allocate, fill and unlock the block; ownership passes to the
    // system only if SetClipboardData succeeds.
    unsafe {
        let mem: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
        let ptr = GlobalLock(mem) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(Some(mem));
            return Err(windows::core::Error::from_thread());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let _ = GlobalUnlock(mem);
        if let Err(err) = SetClipboardData(format, Some(HANDLE(mem.0))) {
            let _ = GlobalFree(Some(mem));
            return Err(err);
        }
        Ok(())
    }
}

/// A bottom-up 32-bpp DIB with opaque alpha.
fn dib_bytes(width: u32, height: u32, bgra: &[u8]) -> Vec<u8> {
    let header = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width as i32,
        biHeight: height as i32,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: width * height * 4,
        ..Default::default()
    };
    let stride = width as usize * 4;
    let mut out = Vec::with_capacity(size_of::<BITMAPINFOHEADER>() + bgra.len());
    // SAFETY: BITMAPINFOHEADER is plain old data.
    out.extend_from_slice(unsafe {
        std::slice::from_raw_parts(
            &header as *const _ as *const u8,
            size_of::<BITMAPINFOHEADER>(),
        )
    });
    for row in bgra.chunks_exact(stride).rev() {
        for px in row.chunks_exact(4) {
            out.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
    }
    out
}
