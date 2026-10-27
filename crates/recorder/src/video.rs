//! Windows Graphics Capture video source.
//!
//! WGC only delivers a frame when something changes, so this source just
//! keeps the *latest* frame (cropped, CPU-side BGRA). The encoder samples it
//! at a fixed rate, which repeats frames across still periods instead of
//! letting them shorten the file (REC-03).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use platform_windows::monitors::PixelRect;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HMODULE, HWND, POINT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{IInspectable, Interface, Result};

#[derive(Clone, Copy, Debug)]
pub enum Target {
    /// Virtual-desktop pixels. Recorded from the monitor holding most of it
    /// (WGC captures one monitor at a time), clipped to that monitor.
    Area(PixelRect),
    /// A top-level window; follows it when moved.
    Window(isize),
}

/// The most recent captured image, top-down BGRA, `width * 4` stride.
pub struct Latest {
    pub data: Vec<u8>,
    /// Increments per captured frame; 0 = nothing captured yet.
    pub seq: u64,
}

struct Shared {
    latest: Mutex<Latest>,
    frames: AtomicU64,
    /// The window was closed (or the monitor removed).
    closed: AtomicBool,
    failed: Mutex<Option<String>>,
}

pub struct VideoSource {
    session: GraphicsCaptureSession,
    pool: Direct3D11CaptureFramePool,
    shared: Arc<Shared>,
    pub width: u32,
    pub height: u32,
}

/// D3D objects used only by the frame handler, which WGC calls serially on
/// its own worker thread.
struct HandlerState {
    context: ID3D11DeviceContext,
    winrt_device: IDirect3DDevice,
    staging: ID3D11Texture2D,
    pool_size: SizeInt32,
    /// Source region inside the captured surface (area targets).
    crop: Option<(i32, i32)>,
    width: u32,
    height: u32,
}

// SAFETY: the state is moved into the handler and only ever touched there;
// WGC never runs one pool's FrameArrived handler concurrently.
unsafe impl Send for HandlerState {}

impl VideoSource {
    pub fn start(target: Target, cursor: bool) -> Result<Self> {
        let (device, context) = d3d_device()?;
        let winrt_device: IDirect3DDevice =
            // SAFETY: the D3D device supports BGRA; conversion is a plain COM call.
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&device.cast::<IDXGIDevice>()?)?.cast()? };

        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let (item, crop, width, height): (GraphicsCaptureItem, _, _, _) = match target {
            Target::Window(hwnd) => {
                // SAFETY: an invalid HWND just fails the call.
                let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(HWND(hwnd as _))? };
                let size = item.Size()?;
                (item, None, even(size.Width), even(size.Height))
            }
            Target::Area(rect) => {
                let monitor = best_monitor(&rect);
                let area = rect.intersect(&monitor.bounds).unwrap_or(monitor.bounds);
                let centre = POINT { x: (area.left + area.right) / 2, y: (area.top + area.bottom) / 2 };
                // SAFETY: plain lookups.
                let item: GraphicsCaptureItem =
                    unsafe { interop.CreateForMonitor(MonitorFromPoint(centre, MONITOR_DEFAULTTONEAREST))? };
                let crop = (area.left - monitor.bounds.left, area.top - monitor.bounds.top);
                (item, Some(crop), even(area.width()), even(area.height()))
            }
        };
        if width == 0 || height == 0 {
            return Err(windows::core::Error::new(windows::Win32::Foundation::E_INVALIDARG, "Area is too small to record"));
        }

        let pool_size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            pool_size,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsCursorCaptureEnabled(cursor);
        // Windows 11: drop the yellow border (unsupported earlier; harmless).
        let _ = session.SetIsBorderRequired(false);

        let shared = Arc::new(Shared {
            latest: Mutex::new(Latest { data: vec![0; (width * height * 4) as usize], seq: 0 }),
            frames: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            failed: Mutex::new(None),
        });

        let staging = staging_texture(&device, width, height)?;
        let state = Mutex::new(HandlerState { context, winrt_device, staging, pool_size, crop, width, height });
        let handler_shared = shared.clone();
        pool.FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(
            move |pool, _| {
                let Some(pool) = pool.as_ref() else { return Ok(()) };
                if let Err(e) = state.lock().unwrap().on_frame(pool, &handler_shared) {
                    tracing::warn!("capture frame failed: {e}");
                    *handler_shared.failed.lock().unwrap() = Some(e.message());
                }
                Ok(())
            },
        ))?;
        let closed_shared = shared.clone();
        item.Closed(&TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
            closed_shared.closed.store(true, Ordering::Release);
            Ok(())
        }))?;

        session.StartCapture()?;
        tracing::info!(width, height, ?target, "video capture started");
        Ok(Self { session, pool, shared, width, height })
    }

    /// Copies the latest frame into `out` if it's newer than `seen`.
    /// Returns the frame's sequence number.
    pub fn latest_into(&self, out: &mut Vec<u8>, seen: u64) -> u64 {
        let latest = self.shared.latest.lock().unwrap();
        if latest.seq != seen {
            out.clear();
            out.extend_from_slice(&latest.data);
        }
        latest.seq
    }

    pub fn frames_captured(&self) -> u64 {
        self.shared.frames.load(Ordering::Relaxed)
    }

    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
    }

    pub fn take_error(&self) -> Option<String> {
        self.shared.failed.lock().unwrap().take()
    }
}

impl Drop for VideoSource {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

impl HandlerState {
    fn on_frame(&mut self, pool: &Direct3D11CaptureFramePool, shared: &Shared) -> Result<()> {
        let frame = pool.TryGetNextFrame()?;
        let content = frame.ContentSize()?;
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        // SAFETY: the surface is a D3D11 texture on our device.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };

        let (x, y) = self.crop.unwrap_or((0, 0));
        let w = (content.Width - x).clamp(0, self.width as i32) as u32;
        let h = (content.Height - y).clamp(0, self.height as i32) as u32;
        if w > 0 && h > 0 {
            let src = D3D11_BOX { left: x as u32, top: y as u32, front: 0, right: x as u32 + w, bottom: y as u32 + h, back: 1 };
            let mut latest = shared.latest.lock().unwrap();
            // SAFETY: copy within both textures' bounds, then a read-only map
            // of the staging texture for exactly the row copies.
            unsafe {
                self.context.CopySubresourceRegion(&self.staging, 0, 0, 0, 0, &texture, 0, Some(&src));
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                let stride = self.width as usize * 4;
                let row = w as usize * 4;
                for r in 0..self.height as usize {
                    let dst = &mut latest.data[r * stride..(r + 1) * stride];
                    if r < h as usize {
                        let p = (mapped.pData as *const u8).add(r * mapped.RowPitch as usize);
                        dst[..row].copy_from_slice(std::slice::from_raw_parts(p, row));
                        // A window that shrank: black beyond its content.
                        dst[row..].fill(0);
                    } else {
                        dst.fill(0);
                    }
                }
                self.context.Unmap(&self.staging, 0);
            }
            latest.seq += 1;
            shared.frames.fetch_add(1, Ordering::Relaxed);
        }
        drop(frame);

        // Resized window: the pool must match, or frames are clipped/stale.
        if content.Width != self.pool_size.Width || content.Height != self.pool_size.Height {
            self.pool_size = content;
            pool.Recreate(&self.winrt_device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, content)?;
        }
        Ok(())
    }
}

fn even(v: i32) -> u32 {
    (v.max(0) as u32) & !1
}

fn best_monitor(rect: &PixelRect) -> platform_windows::monitors::Monitor {
    let monitors = platform_windows::monitors::enumerate();
    let area = |m: &platform_windows::monitors::Monitor| {
        rect.intersect(&m.bounds).map_or(0i64, |r| r.width() as i64 * r.height() as i64)
    };
    monitors
        .iter()
        .max_by_key(|m| (area(m), m.primary))
        .cloned()
        .expect("at least one monitor")
}

fn d3d_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut context = None;
    // SAFETY: standard device creation with out-pointers.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    Ok((device.unwrap(), context.unwrap()))
}

fn staging_texture(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut texture = None;
    // SAFETY: valid descriptor and out-pointer.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
    Ok(texture.unwrap())
}
