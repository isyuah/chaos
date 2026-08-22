//! Windows capture backend implemented with GDI `BitBlt`.
//!
//! The whole virtual desktop is captured once and then cropped to the requested
//! monitor, so monitor bounds (including negative origins) match exactly.

use capture_core::{
    CaptureCapabilities, CaptureError, CapturedFrame, MonitorId, MonitorInfo, PhysicalPoint,
    PixelFormat, ScaleFactor,
};
use capture_platform_api::CaptureBackend;
use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    EnumDisplayMonitors, GetDC, GetDIBits, GetMonitorInfoW, ReleaseDC, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC, HGDIOBJ, HMONITOR, MONITORINFOEXW, SRCCOPY,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, MONITORINFOF_PRIMARY, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// The Windows GDI capture backend.
pub struct WindowsCaptureBackend;

impl WindowsCaptureBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindowsCaptureBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureBackend for WindowsCaptureBackend {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            multi_monitor: true,
            per_monitor_dpi: true,
            capture_virtual_desktop: true,
            capture_window: false,
            live_preview: false,
        }
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        enumerate_monitors()
    }

    fn capture_monitor(&self, id: MonitorId) -> Result<CapturedFrame, CaptureError> {
        let monitors = enumerate_monitors()?;
        let monitor = monitors
            .iter()
            .find(|m| m.id == id)
            .ok_or(CaptureError::MonitorNotFound(id))?;
        self.capture_virtual_desktop()?.crop(monitor.bounds)
    }

    fn capture_virtual_desktop(&self) -> Result<CapturedFrame, CaptureError> {
        capture_virtual_screen()
    }
}

struct MonitorEnumContext {
    monitors: Vec<MonitorInfo>,
    error: Option<CaptureError>,
}

/// GDI callback for `EnumDisplayMonitors`.
unsafe extern "system" fn enum_monitor_cb(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let context = &mut *(lparam.0 as *mut MonitorEnumContext);
    match monitor_from_hmonitor(hmonitor) {
        Ok(info) => context.monitors.push(info),
        Err(error) => {
            context.error = Some(error);
            return BOOL(0);
        }
    }
    BOOL(1)
}

fn monitor_from_hmonitor(hmonitor: HMONITOR) -> Result<MonitorInfo, CaptureError> {
    let mut mi: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe { GetMonitorInfoW(hmonitor, &mut mi.monitorInfo) };
    if !ok.as_bool() {
        return Err(CaptureError::CaptureFailed(
            "GetMonitorInfoW failed".to_string(),
        ));
    }
    let rc = mi.monitorInfo.rcMonitor;
    let bounds = rect_to_physical(rc);
    let work_area = rect_to_physical(mi.monitorInfo.rcWork);

    let scale = {
        let mut dpi_x: u32 = 0;
        let mut dpi_y: u32 = 0;
        if unsafe { GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_ok()
            && dpi_x > 0
        {
            ScaleFactor::new(dpi_x as f64 / 96.0)
        } else {
            ScaleFactor::new(1.0)
        }
    };

    let name = device_name(&mi.szDevice);
    let is_primary = (mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;

    Ok(MonitorInfo {
        id: MonitorId::from_stable_key(&name),
        name,
        bounds,
        work_area,
        scale_factor: scale,
        is_primary,
    })
}

fn device_name(sz: &[u16; 32]) -> String {
    let len = sz.iter().position(|&c| c == 0).unwrap_or(sz.len());
    String::from_utf16_lossy(&sz[..len])
}

fn rect_to_physical(r: RECT) -> capture_core::PhysicalRect {
    capture_core::PhysicalRect::new(
        capture_core::PhysicalPoint::new(r.left, r.top),
        capture_core::PhysicalSize::new((r.right - r.left) as u32, (r.bottom - r.top) as u32),
    )
}

/// Enumerate monitors and assign stable 0-based ids.
fn enumerate_monitors() -> Result<Vec<MonitorInfo>, CaptureError> {
    let mut context = MonitorEnumContext {
        monitors: Vec::new(),
        error: None,
    };
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor_cb),
            LPARAM(&mut context as *mut MonitorEnumContext as isize),
        )
    };
    if !ok.as_bool() {
        if let Some(error) = context.error {
            return Err(error);
        }
        return Err(CaptureError::CaptureFailed(
            "EnumDisplayMonitors failed".to_string(),
        ));
    }
    if let Some(error) = context.error {
        return Err(error);
    }
    if context.monitors.is_empty() {
        return Err(CaptureError::CaptureFailed(
            "no monitors were returned by EnumDisplayMonitors".to_string(),
        ));
    }
    context.monitors.sort_by_key(|monitor| {
        (
            monitor.bounds.origin.x,
            monitor.bounds.origin.y,
            monitor.name.clone(),
        )
    });
    Ok(context.monitors)
}

/// Capture one physical-pixel frame of the complete virtual desktop.
fn capture_virtual_screen() -> Result<CapturedFrame, CaptureError> {
    let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    if vw <= 0 || vh <= 0 {
        return Err(CaptureError::CaptureFailed(
            "virtual screen has zero size".to_string(),
        ));
    }
    let (vw, vh) = (vw as u32, vh as u32);

    // --- GDI capture of the full virtual screen ---
    let screen_dc = unsafe { GetDC(None) };
    if screen_dc.is_invalid() {
        return Err(CaptureError::CaptureFailed("GetDC failed".to_string()));
    }
    let mem_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
    if mem_dc.is_invalid() {
        let _ = unsafe { ReleaseDC(None, screen_dc) };
        return Err(CaptureError::CaptureFailed(
            "CreateCompatibleDC failed".to_string(),
        ));
    }
    let bmp = unsafe { CreateCompatibleBitmap(screen_dc, vw as i32, vh as i32) };
    if bmp.is_invalid() {
        let _ = unsafe { DeleteDC(mem_dc) };
        let _ = unsafe { ReleaseDC(None, screen_dc) };
        return Err(CaptureError::CaptureFailed(
            "CreateCompatibleBitmap failed".to_string(),
        ));
    }
    let old = unsafe { SelectObject(mem_dc, HGDIOBJ(bmp.0)) };

    let blt = unsafe {
        BitBlt(
            mem_dc,
            0,
            0,
            vw as i32,
            vh as i32,
            Some(screen_dc),
            vx,
            vy,
            SRCCOPY,
        )
    };
    if blt.is_err() {
        unsafe { SelectObject(mem_dc, old) };
        let _ = unsafe { DeleteObject(HGDIOBJ(bmp.0)) };
        let _ = unsafe { DeleteDC(mem_dc) };
        let _ = unsafe { ReleaseDC(None, screen_dc) };
        return Err(CaptureError::CaptureFailed(format!(
            "BitBlt failed: {:?}",
            blt.err()
        )));
    }

    // --- Read bits (top-down by giving a negative biHeight) ---
    let mut buf = vec![0u8; (vw as usize) * (vh as usize) * 4];
    let mut bmi: BITMAPINFO = unsafe { std::mem::zeroed() };
    bmi.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: vw as i32,
        biHeight: -(vh as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: 0,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };
    let scanlines = unsafe {
        GetDIBits(
            mem_dc,
            bmp,
            0,
            vh,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        )
    };
    unsafe {
        SelectObject(mem_dc, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(None, screen_dc);
    }
    if scanlines <= 0 {
        return Err(CaptureError::CaptureFailed("GetDIBits failed".to_string()));
    }

    let mut out = vec![0u8; buf.len()];
    for (src, dst) in buf.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
        dst[3] = 255;
    }

    Ok(CapturedFrame::new(
        out.into(),
        vw,
        vh,
        vw.saturating_mul(4),
        PhysicalPoint::new(vx, vy),
        PixelFormat::Rgba8,
    ))
}
