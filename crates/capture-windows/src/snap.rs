//! Windows snap backend: visible top-level window enumeration + point hit-test.

use capture_core::{
    rank_candidates, PhysicalPoint, PhysicalRect, SnapCandidate, SnapCandidateId, SnapCapabilities,
    SnapError, SnapExclusionToken, SnapKind,
};
use capture_platform_api::SnapBackend;
use std::sync::Mutex;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, GetWindowTextW, IsIconic,
    IsWindowVisible, GWL_EXSTYLE, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, WS_EX_TOOLWINDOW,
};

/// The Windows top-level-window snap backend.
pub struct WindowsSnapBackend {
    /// HWND values of overlay/pin windows to exclude.
    excluded: Mutex<Vec<u64>>,
}

impl WindowsSnapBackend {
    pub fn new() -> Self {
        Self {
            excluded: Mutex::new(Vec::new()),
        }
    }
}

impl Default for WindowsSnapBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapBackend for WindowsSnapBackend {
    fn capabilities(&self) -> SnapCapabilities {
        SnapCapabilities {
            window_level: true,
            element_level: true,
            expose_label: true,
        }
    }

    fn set_excluded_window(&self, token: Option<SnapExclusionToken>) {
        let tokens = token.into_iter().collect::<Vec<_>>();
        self.set_excluded_windows(&tokens);
    }

    fn set_excluded_windows(&self, tokens: &[SnapExclusionToken]) {
        if let Ok(mut excluded) = self.excluded.lock() {
            *excluded = tokens.iter().map(|token| token.get()).collect();
        }
    }

    fn candidates_at(&self, point: PhysicalPoint) -> Result<Vec<SnapCandidate>, SnapError> {
        let excluded = self
            .excluded
            .lock()
            .map_err(|_| SnapError::SnapFailed("snap exclusion state is poisoned".to_string()))?
            .clone();
        let mut context = SnapContext {
            point,
            excluded,
            candidates: Vec::new(),
            next_z_order: 0,
        };

        if let Some(candidate) = ui_automation_candidate(point, &context.excluded) {
            context.candidates.push(candidate);
        }

        let ok = unsafe {
            EnumWindows(
                Some(enum_window_cb),
                LPARAM(&mut context as *mut _ as isize),
            )
        };
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
            if vw <= 0 || vh <= 0 {
                return Err(SnapError::SnapFailed(
                    "virtual screen has zero size".to_string(),
                ));
            }
            context.candidates.push(desktop_candidate(vx, vy, vw, vh));
        }

        Ok(rank_candidates(point, context.candidates))
    }
}

/// Query the deepest UI Automation element under the pointer. This is best
/// effort: applications are allowed to expose no UIA provider, in which case
/// the window-level candidates remain available.
fn ui_automation_candidate(point: PhysicalPoint, excluded: &[u64]) -> Option<SnapCandidate> {
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()? };
    let element = unsafe {
        automation
            .ElementFromPoint(POINT {
                x: point.x,
                y: point.y,
            })
            .ok()?
    };
    let bounds = unsafe { element.CurrentBoundingRectangle().ok()? };
    let rect = rect_to_physical(bounds);
    if rect.is_empty() || !rect.contains_exclusive(point) {
        return None;
    }

    let hwnd = unsafe { element.CurrentNativeWindowHandle().ok()? };
    let hwnd_value = hwnd.0 as u64;
    if hwnd_value == 0 || excluded.contains(&hwnd_value) {
        return None;
    }

    // Do not duplicate a top-level window when UI Automation returns its root
    // element rather than a child control.
    let mut window_rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut window_rect) }.is_ok()
        && rect == rect_to_physical(window_rect)
    {
        return None;
    }

    let label = unsafe { element.CurrentName().ok() }
        .map(|name| name.to_string())
        .filter(|name| !name.is_empty());
    let id = SnapCandidateId::new(
        hwnd_value
            .rotate_left(17)
            .wrapping_add(rect.origin.x as u32 as u64)
            .rotate_left(17)
            .wrapping_add(rect.origin.y as u32 as u64)
            .wrapping_add(rect.size.width as u64)
            .wrapping_add((rect.size.height as u64) << 32),
    );
    Some(SnapCandidate {
        id,
        bounds: rect,
        kind: SnapKind::Element,
        label,
        z_order: 0,
    })
}

struct SnapContext {
    point: PhysicalPoint,
    excluded: Vec<u64>,
    candidates: Vec<SnapCandidate>,
    next_z_order: u32,
}

unsafe extern "system" fn enum_window_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut SnapContext);
    let z_order = ctx.next_z_order;
    ctx.next_z_order = ctx.next_z_order.saturating_add(1);

    // Self-exclusion.
    if ctx.excluded.contains(&(hwnd.0 as u64)) {
        return BOOL(1);
    }

    let visible = unsafe { IsWindowVisible(hwnd) };
    if !visible.as_bool() || unsafe { IsIconic(hwnd) }.as_bool() || is_cloaked(hwnd) {
        return BOOL(1);
    }

    let mut rect: RECT = std::mem::zeroed();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return BOOL(1);
    }
    let bounds = visual_bounds(hwnd, rect);
    if bounds.is_empty() {
        return BOOL(1);
    }

    // Exclude tool windows (taskbar, tray) for a cleaner candidate set.
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    if (ex_style as u32) & WS_EX_TOOLWINDOW.0 != 0 {
        return BOOL(1);
    }

    // Only keep windows under the point.
    if !bounds.contains_exclusive(ctx.point) {
        return BOOL(1);
    }

    let label = window_text(hwnd);
    ctx.candidates.push(SnapCandidate {
        id: SnapCandidateId::new(hwnd.0 as u64),
        bounds,
        kind: SnapKind::Window,
        label: if label.is_empty() { None } else { Some(label) },
        z_order,
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

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
    }
}

fn visual_bounds(hwnd: HWND, fallback: RECT) -> PhysicalRect {
    let mut visual = RECT::default();
    let result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut visual as *mut RECT as *mut core::ffi::c_void,
            std::mem::size_of::<RECT>() as u32,
        )
    };
    let rect = if result.is_ok() { visual } else { fallback };
    PhysicalRect::new(
        PhysicalPoint::new(rect.left, rect.top),
        capture_core::PhysicalSize::new(
            rect.right.saturating_sub(rect.left) as u32,
            rect.bottom.saturating_sub(rect.top) as u32,
        ),
    )
}

fn rect_to_physical(rect: RECT) -> PhysicalRect {
    PhysicalRect::new(
        PhysicalPoint::new(rect.left, rect.top),
        capture_core::PhysicalSize::new(
            rect.right.saturating_sub(rect.left) as u32,
            rect.bottom.saturating_sub(rect.top) as u32,
        ),
    )
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
        z_order: u32::MAX,
    }
}
