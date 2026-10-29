//! Clipboard image input and output.

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFOHEADER};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
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

/// Image data read from the Windows clipboard.
#[derive(Clone, Debug)]
pub enum ClipboardImage {
    /// Registered `"PNG"` clipboard format bytes.
    Png(Vec<u8>),
    /// Decoded top-down BGRA8 pixels from `CF_DIB`.
    Bgra {
        width: u32,
        height: u32,
        bgra: Vec<u8>,
    },
}

/// Reads an image from the Windows clipboard (`PNG` format preferred, falling
/// back to 24-bpp / 32-bpp `CF_DIB`).
pub fn get_image() -> Option<ClipboardImage> {
    unsafe {
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
        )
        .ok()?;
        let out = (|| {
            open_with_retry(owner).ok()?;
            let res = read_clipboard_image();
            let _ = CloseClipboard();
            res
        })();
        let _ = DestroyWindow(owner);
        out
    }
}

unsafe fn read_clipboard_image() -> Option<ClipboardImage> {
    unsafe {
        let png_format = RegisterClipboardFormatW(w!("PNG"));
        if png_format != 0 && IsClipboardFormatAvailable(png_format).is_ok() {
            if let Some(bytes) = get_clipboard_bytes(png_format) {
                if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
                    return Some(ClipboardImage::Png(bytes));
                }
            }
        }
        if IsClipboardFormatAvailable(CF_DIB.0 as u32).is_ok() {
            if let Some(bytes) = get_clipboard_bytes(CF_DIB.0 as u32) {
                if let Some((width, height, bgra)) = parse_dib_to_bgra(&bytes) {
                    return Some(ClipboardImage::Bgra {
                        width,
                        height,
                        bgra,
                    });
                }
            }
        }
        None
    }
}

unsafe fn get_clipboard_bytes(format: u32) -> Option<Vec<u8>> {
    unsafe {
        let handle = GetClipboardData(format).ok()?;
        let mem = HGLOBAL(handle.0);
        let size = GlobalSize(mem);
        if size == 0 {
            return None;
        }
        let ptr = GlobalLock(mem) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let slice = std::slice::from_raw_parts(ptr, size);
        let out = slice.to_vec();
        let _ = GlobalUnlock(mem);
        Some(out)
    }
}

fn parse_dib_to_bgra(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if bytes.len() < size_of::<BITMAPINFOHEADER>() {
        return None;
    }
    let header: BITMAPINFOHEADER =
        unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const BITMAPINFOHEADER) };
    let header_size = header.biSize as usize;
    if header_size < size_of::<BITMAPINFOHEADER>() || bytes.len() < header_size {
        return None;
    }
    let width = header.biWidth.unsigned_abs();
    let height = header.biHeight.unsigned_abs();
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return None;
    }
    let top_down = header.biHeight < 0;
    let bpp = header.biBitCount;
    if bpp != 32 && bpp != 24 {
        return None;
    }
    let extra_masks = if header.biCompression == 3 && header_size == size_of::<BITMAPINFOHEADER>() {
        12
    } else {
        0
    };
    let pixel_offset = header_size + extra_masks;
    if bytes.len() <= pixel_offset {
        return None;
    }
    let pixels = &bytes[pixel_offset..];
    let row_bytes = (width as usize * bpp as usize).div_ceil(32) * 4;
    if pixels.len() < row_bytes * height as usize {
        return None;
    }

    let mut bgra = vec![0u8; width as usize * height as usize * 4];
    for y in 0..height as usize {
        let src_y = if top_down {
            y
        } else {
            height as usize - 1 - y
        };
        let src_row = &pixels[src_y * row_bytes..src_y * row_bytes + row_bytes];
        let dst_row = &mut bgra[y * width as usize * 4..(y + 1) * width as usize * 4];
        if bpp == 32 {
            for (dst, src) in dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)) {
                let a = if src[3] == 0 { 255 } else { src[3] };
                dst.copy_from_slice(&[src[0], src[1], src[2], a]);
            }
        } else {
            for (dst, src) in dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(3)) {
                dst.copy_from_slice(&[src[0], src[1], src[2], 255]);
            }
        }
    }
    Some((width, height, bgra))
}
