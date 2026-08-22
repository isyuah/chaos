//! Linux X11 capture and EWMH window snapping.
//!
//! The backend connects lazily, so a frontend can construct its platform before
//! the display is ready. X11 uses RandR 1.5 monitor descriptions, root-window
//! `GetImage` capture, and EWMH client-list/window geometry queries. Wayland is
//! detected explicitly and returns an actionable error because a portal capture
//! requires user permission and a frontend-owned PipeWire consumer.

use capture_core::geometry::{PhysicalPoint, PhysicalRect, PhysicalSize};
use capture_core::{
    CaptureCapabilities, CaptureError, CapturedFrame, MonitorId, MonitorInfo, PixelFormat,
    ScaleFactor, SnapCandidate, SnapCandidateId, SnapCapabilities, SnapError, SnapExclusionToken,
    SnapKind,
};
use capture_platform_api::{CaptureBackend, SnapBackend};
use std::sync::Mutex;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as RandrConnectionExt;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as XprotoConnectionExt, ImageFormat, Window,
};
use x11rb::rust_connection::RustConnection;

const NET_CLIENT_LIST: &[u8] = b"_NET_CLIENT_LIST";
const NET_CLIENT_LIST_STACKING: &[u8] = b"_NET_CLIENT_LIST_STACKING";
const NET_FRAME_EXTENTS: &[u8] = b"_NET_FRAME_EXTENTS";
const NET_WM_NAME: &[u8] = b"_NET_WM_NAME";
const NET_WM_STATE: &[u8] = b"_NET_WM_STATE";
const NET_WM_STATE_HIDDEN: &[u8] = b"_NET_WM_STATE_HIDDEN";
const NET_WM_WINDOW_TYPE: &[u8] = b"_NET_WM_WINDOW_TYPE";
const NET_WM_WINDOW_TYPE_DESKTOP: &[u8] = b"_NET_WM_WINDOW_TYPE_DESKTOP";
const NET_WM_WINDOW_TYPE_DOCK: &[u8] = b"_NET_WM_WINDOW_TYPE_DOCK";
const NET_WM_WINDOW_TYPE_SPLASH: &[u8] = b"_NET_WM_WINDOW_TYPE_SPLASH";
const NET_WM_WINDOW_TYPE_TOOLBAR: &[u8] = b"_NET_WM_WINDOW_TYPE_TOOLBAR";
const NET_WM_WINDOW_TYPE_UTILITY: &[u8] = b"_NET_WM_WINDOW_TYPE_UTILITY";
const NET_WORKAREA: &[u8] = b"_NET_WORKAREA";
const UTF8_STRING: &[u8] = b"UTF8_STRING";

/// Linux capture backend. X11 is supported; Wayland reports a deterministic
/// unsupported error until the portal/PipeWire frontend bridge is supplied.
pub struct LinuxCaptureBackend;

impl CaptureBackend for LinuxCaptureBackend {
    fn capabilities(&self) -> CaptureCapabilities {
        let x11_available = std::env::var_os("DISPLAY").is_some();
        CaptureCapabilities {
            multi_monitor: x11_available,
            per_monitor_dpi: false,
            capture_virtual_desktop: x11_available,
            capture_window: false,
            live_preview: false,
        }
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        let display = connect_x11()?;
        enumerate_monitors(&display)
    }

    fn capture_monitor(&self, id: MonitorId) -> Result<CapturedFrame, CaptureError> {
        let display = connect_x11()?;
        let monitors = enumerate_monitors(&display)?;
        let monitor = monitors
            .iter()
            .find(|monitor| monitor.id == id)
            .ok_or(CaptureError::MonitorNotFound(id))?;
        capture_virtual_desktop_x11(&display)?.crop(monitor.bounds)
    }

    fn capture_virtual_desktop(&self) -> Result<CapturedFrame, CaptureError> {
        let display = connect_x11()?;
        capture_virtual_desktop_x11(&display)
    }
}

/// Linux window snap backend using EWMH client stacking and geometry.
pub struct LinuxSnapBackend {
    excluded: Mutex<Vec<u64>>,
}

impl LinuxSnapBackend {
    pub fn new() -> Self {
        Self {
            excluded: Mutex::new(Vec::new()),
        }
    }
}

impl Default for LinuxSnapBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapBackend for LinuxSnapBackend {
    fn capabilities(&self) -> SnapCapabilities {
        SnapCapabilities {
            window_level: std::env::var_os("DISPLAY").is_some(),
            element_level: false,
            expose_label: std::env::var_os("DISPLAY").is_some(),
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
        let display = connect_x11().map_err(|error| SnapError::Unsupported(error.to_string()))?;
        let excluded = self
            .excluded
            .lock()
            .map_err(|_| SnapError::SnapFailed("snap exclusion state is poisoned".to_string()))?
            .clone();
        enumerate_candidates(&display, point, &excluded)
    }
}

/// A ready-to-use Linux platform exposing both X11 backends.
pub struct LinuxPlatform {
    capture: LinuxCaptureBackend,
    snap: LinuxSnapBackend,
}

impl LinuxPlatform {
    pub fn new() -> Self {
        Self {
            capture: LinuxCaptureBackend,
            snap: LinuxSnapBackend::new(),
        }
    }

    pub fn capture_backend(&self) -> &dyn CaptureBackend {
        &self.capture
    }

    pub fn snap_backend(&self) -> &dyn SnapBackend {
        &self.snap
    }
}

impl Default for LinuxPlatform {
    fn default() -> Self {
        Self::new()
    }
}

struct X11Display {
    connection: RustConnection,
    screen_num: usize,
}

fn connect_x11() -> Result<X11Display, CaptureError> {
    match x11rb::connect(None) {
        Ok((connection, screen_num)) => Ok(X11Display {
            connection,
            screen_num,
        }),
        Err(error) if std::env::var_os("WAYLAND_DISPLAY").is_some() => {
            Err(CaptureError::Unsupported(format!(
                "Wayland display detected; use the XDG ScreenCast portal and PipeWire frontend bridge ({error})"
            )))
        }
        Err(error) => Err(CaptureError::BackendUnavailable(format!(
            "cannot connect to X11 display: {error}"
        ))),
    }
}

fn root(display: &X11Display) -> &x11rb::protocol::xproto::Screen {
    &display.connection.setup().roots[display.screen_num]
}

fn enumerate_monitors(display: &X11Display) -> Result<Vec<MonitorInfo>, CaptureError> {
    let root = root(display);
    let reply = display
        .connection
        .randr_get_monitors(root.root, true)
        .map_err(|error| CaptureError::Unsupported(format!("RandR monitor query failed: {error}")))?
        .reply()
        .map_err(|error| {
            CaptureError::Unsupported(format!("RandR monitor reply failed: {error}"))
        })?;

    let mut monitors = Vec::new();
    for (index, monitor) in reply.monitors.iter().enumerate() {
        if monitor.width == 0 || monitor.height == 0 {
            continue;
        }
        let base_name = atom_name(&display.connection, monitor.name)
            .unwrap_or_else(|| format!("X11-Monitor-{index}"));
        let name = if monitors
            .iter()
            .any(|item: &MonitorInfo| item.name == base_name)
        {
            format!("{base_name}-{index}")
        } else {
            base_name
        };
        let bounds = PhysicalRect::new(
            PhysicalPoint::new(i32::from(monitor.x), i32::from(monitor.y)),
            PhysicalSize::new(u32::from(monitor.width), u32::from(monitor.height)),
        );
        let work_area = query_work_area(&display.connection, root.root, bounds);
        monitors.push(MonitorInfo {
            id: MonitorId::from_stable_key(&name),
            name,
            bounds,
            work_area,
            scale_factor: ScaleFactor::new(1.0),
            is_primary: monitor.primary,
        });
    }

    if monitors.is_empty() {
        let bounds = PhysicalRect::new(
            PhysicalPoint::ZERO,
            PhysicalSize::new(
                u32::from(root.width_in_pixels),
                u32::from(root.height_in_pixels),
            ),
        );
        monitors.push(MonitorInfo {
            id: MonitorId::from_stable_key("X11-root"),
            name: "X11-root".to_string(),
            bounds,
            work_area: query_work_area(&display.connection, root.root, bounds),
            scale_factor: ScaleFactor::new(1.0),
            is_primary: true,
        });
    }
    monitors.sort_by_key(|monitor| (monitor.bounds.origin.x, monitor.bounds.origin.y));
    Ok(monitors)
}

fn capture_virtual_desktop_x11(display: &X11Display) -> Result<CapturedFrame, CaptureError> {
    let root = root(display);
    let monitors = enumerate_monitors(display)?;
    let virtual_bounds = monitors
        .iter()
        .map(|monitor| monitor.bounds)
        .reduce(PhysicalRect::union)
        .unwrap_or(PhysicalRect::new(
            PhysicalPoint::ZERO,
            PhysicalSize::new(
                u32::from(root.width_in_pixels),
                u32::from(root.height_in_pixels),
            ),
        ));
    let width = root.width_in_pixels;
    let height = root.height_in_pixels;
    let image = display
        .connection
        .get_image(
            ImageFormat::Z_PIXMAP,
            root.root,
            0,
            0,
            width,
            height,
            u32::MAX,
        )
        .map_err(|error| CaptureError::CaptureFailed(format!("X11 GetImage failed: {error}")))?
        .reply()
        .map_err(|error| {
            CaptureError::CaptureFailed(format!("X11 GetImage reply failed: {error}"))
        })?;
    let byte_order = x11rb::image::ImageOrder::try_from(
        display.connection.setup().image_byte_order,
    )
    .map_err(|_| CaptureError::CaptureFailed("unsupported X11 image byte order".to_string()))?;
    let pixels = image_to_rgba(
        &image.data,
        width,
        height,
        byte_order,
        visual_masks(display, image.visual).ok_or_else(|| {
            CaptureError::CaptureFailed("X11 root visual masks are unavailable".to_string())
        })?,
    )?;
    Ok(CapturedFrame::new(
        pixels.into(),
        u32::from(width),
        u32::from(height),
        u32::from(width).saturating_mul(4),
        virtual_bounds.origin,
        PixelFormat::Rgba8,
    ))
}

fn visual_masks(display: &X11Display, visual_id: u32) -> Option<(u32, u32, u32)> {
    display
        .connection
        .setup()
        .roots
        .iter()
        .flat_map(|screen| screen.allowed_depths.iter())
        .flat_map(|depth| depth.visuals.iter())
        .find(|visual| visual.visual_id == visual_id)
        .map(|visual| (visual.red_mask, visual.green_mask, visual.blue_mask))
}

fn image_to_rgba(
    data: &[u8],
    width: u16,
    height: u16,
    byte_order: x11rb::image::ImageOrder,
    masks: (u32, u32, u32),
) -> Result<Vec<u8>, CaptureError> {
    let pixels = usize::from(width)
        .checked_mul(usize::from(height))
        .ok_or_else(|| CaptureError::InvalidFrame("X11 image dimensions overflow".to_string()))?;
    if pixels == 0 || data.len() % pixels != 0 {
        return Err(CaptureError::InvalidFrame(
            "X11 image buffer does not contain whole pixels".to_string(),
        ));
    }
    let bytes_per_pixel = data.len() / pixels;
    if !(3..=4).contains(&bytes_per_pixel) {
        return Err(CaptureError::FormatUnsupported(PixelFormat::Rgb24));
    }
    let mut rgba = vec![0u8; pixels * 4];
    for (index, output) in rgba.chunks_exact_mut(4).enumerate() {
        let source = &data[index * bytes_per_pixel..(index + 1) * bytes_per_pixel];
        let value = match byte_order {
            x11rb::image::ImageOrder::LsbFirst => source
                .iter()
                .enumerate()
                .fold(0u32, |value, (shift, byte)| {
                    value | u32::from(*byte) << (shift * 8)
                }),
            x11rb::image::ImageOrder::MsbFirst => source
                .iter()
                .fold(0u32, |value, byte| (value << 8) | u32::from(*byte)),
        };
        output[0] = mask_to_u8(value, masks.0);
        output[1] = mask_to_u8(value, masks.1);
        output[2] = mask_to_u8(value, masks.2);
        output[3] = 255;
    }
    Ok(rgba)
}

fn mask_to_u8(value: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let max = mask >> shift;
    let channel = (value & mask) >> shift;
    ((u64::from(channel) * 255 + u64::from(max) / 2) / u64::from(max)).min(255) as u8
}

fn query_work_area(
    connection: &RustConnection,
    root: Window,
    bounds: PhysicalRect,
) -> PhysicalRect {
    let Ok(workarea_atom) = intern_atom(connection, NET_WORKAREA) else {
        return bounds;
    };
    let Ok(cookie) = connection.get_property(false, root, workarea_atom, AtomEnum::CARDINAL, 0, 4)
    else {
        return bounds;
    };
    let Ok(reply) = cookie.reply() else {
        return bounds;
    };
    let values = reply
        .value32()
        .map(|values| values.map(|value| value as i32).collect::<Vec<_>>())
        .unwrap_or_default();
    if values.len() < 4 {
        return bounds;
    }
    let desktop = PhysicalRect::new(
        PhysicalPoint::new(values[0], values[1]),
        PhysicalSize::new(values[2].max(0) as u32, values[3].max(0) as u32),
    );
    bounds.intersection(desktop).unwrap_or(bounds)
}

fn enumerate_candidates(
    display: &X11Display,
    point: PhysicalPoint,
    excluded: &[u64],
) -> Result<Vec<SnapCandidate>, SnapError> {
    let root = root(display);
    let atom_stacking = intern_atom(&display.connection, NET_CLIENT_LIST_STACKING)
        .map_err(|error| SnapError::SnapFailed(error.to_string()))?;
    let atom_clients = intern_atom(&display.connection, NET_CLIENT_LIST)
        .map_err(|error| SnapError::SnapFailed(error.to_string()))?;
    let windows = read_window_list(&display.connection, root.root, atom_stacking)
        .or_else(|_| read_window_list(&display.connection, root.root, atom_clients))
        .map_err(|error| SnapError::SnapFailed(error.to_string()))?;
    let mut candidates = Vec::new();
    for (z_order, window) in windows.into_iter().rev().enumerate() {
        if excluded.contains(&u64::from(window)) {
            continue;
        }
        let Some(bounds) = window_bounds(&display.connection, root.root, window) else {
            continue;
        };
        if !bounds.contains_exclusive(point)
            || window_is_hidden_or_tool(&display.connection, window)
        {
            continue;
        }
        candidates.push(SnapCandidate {
            id: SnapCandidateId::new(u64::from(window)),
            bounds,
            kind: SnapKind::Window,
            label: window_label(&display.connection, window),
            z_order: z_order as u32,
        });
    }

    let desktop = enumerate_monitors(display)
        .map_err(|error| SnapError::SnapFailed(error.to_string()))?
        .into_iter()
        .map(|monitor| monitor.bounds)
        .reduce(PhysicalRect::union)
        .unwrap_or(PhysicalRect::new(
            PhysicalPoint::ZERO,
            PhysicalSize::new(
                u32::from(root.width_in_pixels),
                u32::from(root.height_in_pixels),
            ),
        ));
    candidates.push(SnapCandidate {
        id: SnapCandidateId::new(0),
        bounds: desktop,
        kind: SnapKind::Desktop,
        label: None,
        z_order: u32::MAX,
    });
    Ok(capture_core::rank_candidates(point, candidates))
}

fn read_window_list(
    connection: &RustConnection,
    root: Window,
    atom: Atom,
) -> Result<Vec<Window>, x11rb::errors::ReplyOrIdError> {
    let reply = connection
        .get_property(false, root, atom, AtomEnum::WINDOW, 0, u32::MAX)?
        .reply()?;
    Ok(reply
        .value32()
        .map(|windows| windows.map(|window| window as Window).collect())
        .unwrap_or_default())
}

fn window_bounds(
    connection: &RustConnection,
    root: Window,
    window: Window,
) -> Option<PhysicalRect> {
    let geometry = connection.get_geometry(window).ok()?.reply().ok()?;
    let translated = connection
        .translate_coordinates(window, root, 0, 0)
        .ok()?
        .reply()
        .ok()?;
    let extents = intern_atom(connection, NET_FRAME_EXTENTS)
        .ok()
        .and_then(|atom| {
            connection
                .get_property(false, window, atom, AtomEnum::CARDINAL, 0, 4)
                .ok()?
                .reply()
                .ok()
        })
        .map(|reply| {
            reply
                .value32()
                .map(|values| values.collect::<Vec<_>>())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let left = extents.first().copied().unwrap_or(0) as i32;
    let right = extents.get(2).copied().unwrap_or(0) as i32;
    let top = extents.get(1).copied().unwrap_or(0) as i32;
    let bottom = extents.get(3).copied().unwrap_or(0) as i32;
    let x = i32::from(translated.dst_x).saturating_sub(left);
    let y = i32::from(translated.dst_y).saturating_sub(top);
    let width = u32::from(geometry.width)
        .saturating_add(left.max(0) as u32)
        .saturating_add(right.max(0) as u32);
    let height = u32::from(geometry.height)
        .saturating_add(top.max(0) as u32)
        .saturating_add(bottom.max(0) as u32);
    Some(PhysicalRect::new(
        PhysicalPoint::new(x, y),
        PhysicalSize::new(width, height),
    ))
}

fn window_is_hidden_or_tool(connection: &RustConnection, window: Window) -> bool {
    let Ok(state_atom) = intern_atom(connection, NET_WM_STATE) else {
        return false;
    };
    let Ok(hidden_atom) = intern_atom(connection, NET_WM_STATE_HIDDEN) else {
        return false;
    };
    let Ok(type_atom) = intern_atom(connection, NET_WM_WINDOW_TYPE) else {
        return false;
    };
    let hidden = connection
        .get_property(false, window, state_atom, AtomEnum::ATOM, 0, u32::MAX)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| {
            reply
                .value32()
                .map(|mut atoms| atoms.any(|atom| atom == hidden_atom))
                .unwrap_or(false)
        })
        .unwrap_or(false);
    if hidden {
        return true;
    }
    let excluded_types = [
        NET_WM_WINDOW_TYPE_DESKTOP,
        NET_WM_WINDOW_TYPE_DOCK,
        NET_WM_WINDOW_TYPE_SPLASH,
        NET_WM_WINDOW_TYPE_TOOLBAR,
        NET_WM_WINDOW_TYPE_UTILITY,
    ];
    let excluded_atoms = excluded_types
        .iter()
        .filter_map(|name| intern_atom(connection, name).ok())
        .collect::<Vec<_>>();
    connection
        .get_property(false, window, type_atom, AtomEnum::ATOM, 0, u32::MAX)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| {
            reply
                .value32()
                .map(|mut atoms| atoms.any(|atom| excluded_atoms.contains(&atom)))
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn window_label(connection: &RustConnection, window: Window) -> Option<String> {
    let utf8_atom = intern_atom(connection, UTF8_STRING).ok()?;
    let name_atom = intern_atom(connection, NET_WM_NAME).ok()?;
    if let Ok(cookie) = connection.get_property(false, window, name_atom, utf8_atom, 0, 1024) {
        if let Ok(reply) = cookie.reply() {
            let label = String::from_utf8_lossy(&reply.value).trim().to_string();
            if !label.is_empty() {
                return Some(label);
            }
        }
    }
    let reply = connection
        .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 1024)
        .ok()?
        .reply()
        .ok()?;
    let label = String::from_utf8_lossy(&reply.value).trim().to_string();
    (!label.is_empty()).then_some(label)
}

fn atom_name(connection: &RustConnection, atom: Atom) -> Option<String> {
    let reply = connection.get_atom_name(atom).ok()?.reply().ok()?;
    let name = String::from_utf8_lossy(&reply.name).trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn intern_atom(
    connection: &RustConnection,
    name: &[u8],
) -> Result<Atom, x11rb::errors::ReplyOrIdError> {
    Ok(connection.intern_atom(false, name)?.reply()?.atom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_little_endian_rgb_masks() {
        let data = [0x00, 0x00, 0xFF, 0x00];
        let rgba = image_to_rgba(
            &data,
            1,
            1,
            x11rb::image::ImageOrder::LsbFirst,
            (0x00FF_0000, 0x0000_FF00, 0x0000_00FF),
        )
        .expect("one pixel should convert");
        assert_eq!(rgba, vec![255, 0, 0, 255]);
    }

    #[test]
    fn mask_scaling_handles_opaque_channel() {
        assert_eq!(mask_to_u8(0xFF, 0xFF), 255);
        assert_eq!(mask_to_u8(0, 0xFF), 0);
    }
}
