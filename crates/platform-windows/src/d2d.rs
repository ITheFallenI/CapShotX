//! Shared Direct2D/DirectWrite helpers for CapShotX's owner-drawn windows.

use std::ffi::c_void;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE, D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED,
    D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS, D2D1_PRESENT_OPTIONS_IMMEDIATELY,
    D2D1_PRESENT_OPTIONS_NONE,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1RenderTarget,
    ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::{Interface, Result, w};

pub use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_PARAGRAPH_ALIGNMENT_FAR, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING,
};

pub struct Factories {
    pub d2d: ID2D1Factory,
    pub dwrite: IDWriteFactory,
}

impl Factories {
    pub fn new() -> Result<Self> {
        // SAFETY: factory creation has no preconditions.
        unsafe {
            Ok(Self {
                d2d: D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?,
                dwrite: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?,
            })
        }
    }

    /// A render target for `hwnd`. With `dpi = 96` one unit is one pixel.
    pub fn hwnd_target(&self, hwnd: HWND, size: (u32, u32), dpi: f32) -> Result<ID2D1HwndRenderTarget> {
        self.hwnd_target_with(hwnd, size, dpi, D2D1_PRESENT_OPTIONS_NONE)
    }

    /// Like [`Self::hwnd_target`], but presents without waiting for vsync.
    /// For interactive surfaces (several windows repainting per mouse move,
    /// where waiting for each vblank in turn makes tracking lag).
    pub fn hwnd_target_immediate(&self, hwnd: HWND, size: (u32, u32), dpi: f32) -> Result<ID2D1HwndRenderTarget> {
        self.hwnd_target_with(hwnd, size, dpi, D2D1_PRESENT_OPTIONS_IMMEDIATELY)
    }

    fn hwnd_target_with(
        &self,
        hwnd: HWND,
        size: (u32, u32),
        dpi: f32,
        present: D2D1_PRESENT_OPTIONS,
    ) -> Result<ID2D1HwndRenderTarget> {
        // SAFETY: descriptors are fully initialised.
        unsafe {
            self.d2d.CreateHwndRenderTarget(
                &D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                    pixelFormat: pixel_format(D2D1_ALPHA_MODE_PREMULTIPLIED),
                    dpiX: dpi,
                    dpiY: dpi,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                },
                &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd,
                    pixelSize: D2D_SIZE_U {
                        width: size.0,
                        height: size.1,
                    },
                    presentOptions: present,
                },
            )
        }
    }

    /// Segoe UI text format, single line, trimmed with an ellipsis.
    pub fn text_format(
        &self,
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
        paragraph: DWRITE_PARAGRAPH_ALIGNMENT,
        text: DWRITE_TEXT_ALIGNMENT,
    ) -> Result<IDWriteTextFormat> {
        // SAFETY: plain DirectWrite object creation.
        unsafe {
            let format = self.dwrite.CreateTextFormat(
                w!("Segoe UI"),
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )?;
            format.SetParagraphAlignment(paragraph)?;
            format.SetTextAlignment(text)?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let sign = self.dwrite.CreateEllipsisTrimmingSign(&format)?;
            format.SetTrimming(
                &DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                },
                &sign,
            )?;
            Ok(format)
        }
    }

    /// Width of `text` laid out in `format`, in the target's units.
    pub fn text_width(&self, text: &str, format: &IDWriteTextFormat) -> f32 {
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: plain DirectWrite layout.
        unsafe {
            self.dwrite
                .CreateTextLayout(&wide, format, 10_000.0, 1_000.0)
                .and_then(|layout| {
                    let mut metrics = Default::default();
                    layout.GetMetrics(&mut metrics).map(|()| metrics)
                })
                .map_or(0.0, |m: windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS| {
                    m.widthIncludingTrailingWhitespace
                })
        }
    }
}

fn pixel_format(alpha: D2D1_ALPHA_MODE) -> D2D1_PIXEL_FORMAT {
    D2D1_PIXEL_FORMAT {
        format: DXGI_FORMAT_B8G8R8A8_UNORM,
        alphaMode: alpha,
    }
}

/// Creates an opaque bitmap from BGRX pixels (alpha ignored). `stride` is
/// the byte distance between rows, so a sub-rectangle of a larger buffer can
/// be uploaded directly.
pub fn opaque_bitmap(
    target: &impl Interface,
    bgra: &[u8],
    width: u32,
    height: u32,
    stride: u32,
) -> Result<ID2D1Bitmap> {
    let target: ID2D1RenderTarget = target.cast()?;
    assert!(bgra.len() >= (stride as usize) * (height.saturating_sub(1) as usize) + width as usize * 4);
    // SAFETY: the buffer covers `height` rows of `stride` bytes (asserted).
    unsafe {
        target.CreateBitmap(
            D2D_SIZE_U { width, height },
            Some(bgra.as_ptr() as *const c_void),
            stride,
            &D2D1_BITMAP_PROPERTIES {
                pixelFormat: pixel_format(D2D1_ALPHA_MODE_IGNORE),
                dpiX: 96.0,
                dpiY: 96.0,
            },
        )
    }
}

pub fn rgb(r: u8, g: u8, b: u8) -> D2D1_COLOR_F {
    rgba(r, g, b, 1.0)
}

pub fn rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

pub fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

pub fn brush(target: &ID2D1RenderTarget, color: D2D1_COLOR_F) -> Result<ID2D1SolidColorBrush> {
    // SAFETY: plain resource creation.
    unsafe { target.CreateSolidColorBrush(&color, None) }
}

pub fn draw_text(
    target: &ID2D1RenderTarget,
    text: &str,
    format: &IDWriteTextFormat,
    rect: D2D_RECT_F,
    brush: &ID2D1SolidColorBrush,
) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: called between BeginDraw/EndDraw with live resources.
    unsafe {
        target.DrawText(
            &wide,
            format,
            &rect,
            brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}
