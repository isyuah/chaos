//! Windows capture backend implemented with GDI `BitBlt`.
//!
//! The whole virtual desktop is captured once and then cropped to the requested
//! monitor, so monitor bounds (including negative origins) match exactly.

use capture_core::{
    CaptureCapabilities, CaptureError, CapturedFrame, MonitorId, MonitorInfo, PixelFormat,
    ScaleFactor,
};
use capture_platform_api::CaptureBackend;
use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, EnumDisplayMonitors,
    GetDC, GetDIBits, GetMonitorInfoW, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    DIB_RGB_COLORS, HDC, HGDIOBJ, HMONITOR, MONITORINFOEXW, SRCCOPY, BI_RGB,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// The Windows GDI capture backend.
pub struct WindowsCaptureBackend;

impl WindowsCaptureBackend {
    pub fn new() -> Self {
        Self
    }
}

impl CaptureBackend for WindowsCaptureBackend {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            multi_monitor: true,
            per_monitor_dpi: true,
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
        capture_monitor_rect(&monitor.bounds, monitor.scale_factor)
    }
}

/// GDI callback for `EnumDisplayMonitors`.
unsafe extern "system" fn enum_monitor_cb(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let list = &mut *(lparam.0 as *mut Vec<MonitorInfo>);
    if let Ok(info) = monitor_from_hmonitor(hmonitor) {
        list.push(info);
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

    let scale = {
        let mut dpi_x: u32 = 0;
        let mut dpi_y: u32 = 0;
        if unsafe { GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
            .is_ok()
            && dpi_x > 0
        {
            ScaleFactor::new(dpi_x as f64 / 96.0)
        } else {
            ScaleFactor::new(1.0)
        }
    };

    let name = device_name(&mi.szDevice);
    // Windows always places the primary monitor's top-left at (0,0).
    let is_primary = bounds.origin.x == 0 && bounds.origin.y == 0;

    Ok(MonitorInfo {
        id: MonitorId::new(0), // filled in by the enumerator
        name,
        bounds,
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
    let mut list: Vec<MonitorInfo> = Vec::new();
    // First get raw monitor descriptions (ids are all 0 here).
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor_cb),
            LPARAM(&mut list as *mut Vec<MonitorInfo> as isize),
        )
    };
    if !ok.as_bool() {
        return Err(CaptureError::CaptureFailed(
            "EnumDisplayMonitors failed".to_string(),
        ));
    }
    // Assign index-based ids in enum order.
    for (i, m) in list.iter_mut().enumerate() {
        m.id = MonitorId::new(i as u32);
    }
    Ok(list)
}

/// Capture the physical pixels of a single monitor rect from the virtual desktop.
fn capture_monitor_rect(
    bounds: &capture_core::PhysicalRect,
    _scale: ScaleFactor,
) -> Result<CapturedFrame, CaptureError> {
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

    // Locate the monitor within the captured virtual screen.
    let crop_x0 = (bounds.origin.x - vx) as i64;
    let crop_y0 = (bounds.origin.y - vy) as i64;
    let crop_w = bounds.size.width as i64;
    let crop_h = bounds.size.height as i64;
    if crop_x0 < 0 || crop_y0 < 0 || crop_x0 + crop_w > vw as i64 || crop_y0 + crop_h > vh as i64 {
        return Err(CaptureError::CaptureFailed(
            "monitor rect outside virtual screen".to_string(),
        ));
    }

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
            0,
            0,
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

    // --- Crop the monitor region and convert BGRA -> RGBA ---
    let mut out = vec![0u8; (crop_w as usize) * (crop_h as usize) * 4];
    let full_stride = vw as usize * 4;
    let out_stride = crop_w as usize * 4;
    for y in 0..crop_h as usize {
        let src_row = (crop_y0 as usize + y) * full_stride + (crop_x0 as usize) * 4;
        let dst_row = y * out_stride;
        for x in 0..crop_w as usize {
            let s = src_row + x * 4;
            let d = dst_row + x * 4;
            out[d] = buf[s + 2];
            out[d + 1] = buf[s + 1];
            out[d + 2] = buf[s];
            out[d + 3] = buf[s + 3];
        }
    }

    Ok(CapturedFrame::new(
        out.into(),
        crop_w as u32,
        crop_h as u32,
        out_stride as u32,
        capture_core::PhysicalPoint::new(bounds.origin.x, bounds.origin.y),
        PixelFormat::Rgba8,
    ))
}
