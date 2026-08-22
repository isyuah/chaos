//! Windows snap backend: visible top-level window enumeration + point hit-test.

use capture_core::{
    rank_candidates, PhysicalPoint, PhysicalRect, SnapCandidate, SnapCandidateId,
    SnapCapabilities, SnapError, SnapExclusionToken, SnapKind,
};
use capture_platform_api::SnapBackend;
use std::sync::Mutex;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, GetWindowTextW, IsWindowVisible,
    GWL_EXSTYLE, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    WS_EX_TOOLWINDOW,
};

/// The Windows top-level-window snap backend.
pub struct WindowsSnapBackend {
    /// HWND value of the window to exclude (usually the overlay itself).
    excluded: Mutex<Option<u64>>,
}

impl WindowsSnapBackend {
    pub fn new() -> Self {
        Self {
            excluded: Mutex::new(None),
        }
    }
}

impl SnapBackend for WindowsSnapBackend {
    fn capabilities(&self) -> SnapCapabilities {
        SnapCapabilities {
            window_level: true,
            element_level: false,
            expose_label: true,
        }
    }

    fn set_excluded_window(&self, token: Option<SnapExclusionToken>) {
        *self.excluded.lock().unwrap() = token.map(|t| t.get());
    }

    fn candidates_at(&self, point: PhysicalPoint) -> Result<Vec<SnapCandidate>, SnapError> {
        let excluded = *self.excluded.lock().unwrap();
        let mut context = SnapContext {
            point,
            excluded,
            candidates: Vec::new(),
        };

        let ok = unsafe { EnumWindows(Some(enum_window_cb), LPARAM(&mut context as *mut _ as isize)) };
        if ok.is_err() {
            return Err(SnapError::SnapFailed("EnumWindows failed".to_string()));
        }

        // Fallback: virtual-desktop candidate so the user can always select an area.
        let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        if !context
            .candidates
            .iter()
            .any(|c| c.kind == SnapKind::Desktop)
        {
            context.candidates.push(desktop_candidate(vx, vy, vw, vh));
        }

        Ok(rank_candidates(point, context.candidates))
    }
}

struct SnapContext {
    point: PhysicalPoint,
    excluded: Option<u64>,
    candidates: Vec<SnapCandidate>,
}

unsafe extern "system" fn enum_window_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut SnapContext);

    // Self-exclusion.
    if let Some(ex) = ctx.excluded {
        if hwnd.0 as u64 == ex {
            return BOOL(1);
        }
    }

    let visible = unsafe { IsWindowVisible(hwnd) };
    if !visible.as_bool() {
        return BOOL(1);
    }

    let mut rect: RECT = std::mem::zeroed();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return BOOL(1);
    }
    let bounds = PhysicalRect::new(
        PhysicalPoint::new(rect.left, rect.top),
        capture_core::PhysicalSize::new(
            (rect.right - rect.left) as u32,
            (rect.bottom - rect.top) as u32,
        ),
    );
    if bounds.is_empty() {
        return BOOL(1);
    }

    // Exclude tool windows (taskbar, tray) for a cleaner candidate set.
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    if (ex_style as u32) & WS_EX_TOOLWINDOW.0 != 0 {
        return BOOL(1);
    }

    // Only keep windows under the point.
    if !bounds.contains(ctx.point) {
        return BOOL(1);
    }

    let label = window_text(hwnd);
    let kind = if bounds.contains(ctx.point) {
        SnapKind::Window
    } else {
        SnapKind::Desktop
    };
    ctx.candidates.push(SnapCandidate {
        id: SnapCandidateId::new(hwnd.0 as u64),
        bounds,
        kind,
        label: if label.is_empty() { None } else { Some(label) },
    });
    BOOL(1)
}

fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if n == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn desktop_candidate(vx: i32, vy: i32, vw: i32, vh: i32) -> SnapCandidate {
    SnapCandidate {
        id: SnapCandidateId::new(0),
        bounds: PhysicalRect::new(
            PhysicalPoint::new(vx, vy),
            capture_core::PhysicalSize::new(vw as u32, vh as u32),
        ),
        kind: SnapKind::Desktop,
        label: None,
    }
}
