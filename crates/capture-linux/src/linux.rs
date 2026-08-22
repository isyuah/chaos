//! Linux X11 and native Wayland capture with EWMH window snapping.
//!
//! The backend connects lazily, so a frontend can construct its platform before
//! the display is ready. X11 uses RandR 1.5 monitor descriptions, root-window
//! `GetImage` capture, and EWMH client-list/window geometry queries. Wayland is
//! native Wayland uses the XDG ScreenCast portal and a short-lived PipeWire
//! consumer to obtain an authorized frame. Window enumeration remains an X11
//! capability because Wayland intentionally does not expose a global window
//! list.

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType, Stream as PortalStream};
use ashpd::desktop::PersistMode;
use capture_core::geometry::{PhysicalPoint, PhysicalRect, PhysicalSize};
use capture_core::{
    CaptureCapabilities, CaptureError, CapturedFrame, MonitorId, MonitorInfo, PixelFormat,
    ScaleFactor, SnapCandidate, SnapCandidateId, SnapCapabilities, SnapError, SnapExclusionToken,
    SnapKind,
};
use capture_platform_api::{CaptureBackend, SnapBackend};
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use pw::spa::param::video::{VideoFormat, VideoInfoRaw};
use std::os::fd::OwnedFd;
use std::sync::Mutex;
use std::time::{Duration, Instant};
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

/// Linux capture backend.
///
/// X11 is used when `DISPLAY` is available. Pure Wayland uses the ScreenCast
/// portal and PipeWire. The portal asks the user to choose a monitor or virtual
/// desktop for each synchronous capture request, which keeps permission and
/// session ownership explicit at the Core boundary.
pub struct LinuxCaptureBackend;

impl CaptureBackend for LinuxCaptureBackend {
    fn capabilities(&self) -> CaptureCapabilities {
        let x11_available = std::env::var_os("DISPLAY").is_some();
        let wayland_available = std::env::var_os("WAYLAND_DISPLAY").is_some();
        CaptureCapabilities {
            multi_monitor: x11_available || wayland_available,
            per_monitor_dpi: false,
            capture_virtual_desktop: x11_available || wayland_available,
            capture_window: false,
            live_preview: false,
        }
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        if std::env::var_os("DISPLAY").is_some() {
            return enumerate_monitors(&connect_x11()?);
        }
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            return query_wayland_monitors();
        }
        enumerate_monitors(&connect_x11()?)
    }

    fn capture_monitor(&self, id: MonitorId) -> Result<CapturedFrame, CaptureError> {
        if std::env::var_os("DISPLAY").is_some() {
            let display = connect_x11()?;
            let monitors = enumerate_monitors(&display)?;
            let monitor = monitors
                .iter()
                .find(|monitor| monitor.id == id)
                .ok_or(CaptureError::MonitorNotFound(id))?;
            return capture_virtual_desktop_x11(&display)?.crop(monitor.bounds);
        }
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            let (frame, monitor) = capture_wayland(SourceType::Monitor)?;
            if monitor.id != id {
                return Err(CaptureError::MonitorNotFound(id));
            }
            return Ok(frame);
        }
        let display = connect_x11()?;
        let monitors = enumerate_monitors(&display)?;
        let monitor = monitors
            .iter()
            .find(|monitor| monitor.id == id)
            .ok_or(CaptureError::MonitorNotFound(id))?;
        capture_virtual_desktop_x11(&display)?.crop(monitor.bounds)
    }

    fn capture_virtual_desktop(&self) -> Result<CapturedFrame, CaptureError> {
        if std::env::var_os("DISPLAY").is_some() {
            return capture_virtual_desktop_x11(&connect_x11()?);
        }
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            return capture_wayland(SourceType::Virtual).map(|(frame, _)| frame);
        }
        capture_virtual_desktop_x11(&connect_x11()?)
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

#[derive(Debug)]
struct PortalCaptureContext {
    stream: PortalStream,
    pipewire_fd: OwnedFd,
}

fn query_wayland_monitors() -> Result<Vec<MonitorInfo>, CaptureError> {
    let context = open_wayland_stream(SourceType::Monitor)?;
    Ok(vec![wayland_monitor_info(&context.stream, None)])
}

fn capture_wayland(source_type: SourceType) -> Result<(CapturedFrame, MonitorInfo), CaptureError> {
    let context = open_wayland_stream(source_type)?;
    let origin = context
        .stream
        .position()
        .map(|(x, y)| PhysicalPoint::new(x, y))
        .unwrap_or(PhysicalPoint::ZERO);
    let frame = capture_pipewire_frame(
        context.stream.pipe_wire_node_id(),
        context.pipewire_fd,
        origin,
    )?;
    let monitor = wayland_monitor_info(&context.stream, Some((frame.width, frame.height)));
    Ok((frame, monitor))
}

fn open_wayland_stream(source_type: SourceType) -> Result<PortalCaptureContext, CaptureError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            CaptureError::BackendUnavailable(format!(
                "cannot create Wayland portal runtime: {error}"
            ))
        })?;
    runtime.block_on(async move {
        let proxy = Screencast::new().await.map_err(portal_error)?;
        let session = proxy.create_session().await.map_err(portal_error)?;
        proxy
            .select_sources(
                &session,
                CursorMode::Hidden,
                source_type.into(),
                false,
                None,
                PersistMode::DoNot,
            )
            .await
            .map_err(portal_error)?;
        let response = proxy
            .start(&session, None)
            .await
            .map_err(portal_error)?
            .response()
            .map_err(portal_error)?;
        let stream = response.streams().first().cloned().ok_or_else(|| {
            CaptureError::CaptureFailed(
                "Wayland portal returned no selected PipeWire stream".to_string(),
            )
        })?;
        let pipewire_fd = proxy
            .open_pipe_wire_remote(&session)
            .await
            .map_err(portal_error)?;
        Ok(PortalCaptureContext {
            stream,
            pipewire_fd,
        })
    })
}

fn portal_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::Unsupported(format!("Wayland ScreenCast portal request failed: {error}"))
}

fn wayland_monitor_info(stream: &PortalStream, captured_size: Option<(u32, u32)>) -> MonitorInfo {
    let (logical_width, logical_height) = stream
        .size()
        .filter(|(width, height)| *width > 0 && *height > 0)
        .map(|(width, height)| (width as u32, height as u32))
        .unwrap_or((1, 1));
    let (width, height) = captured_size.unwrap_or((logical_width, logical_height));
    let origin = stream
        .position()
        .map(|(x, y)| PhysicalPoint::new(x, y))
        .unwrap_or(PhysicalPoint::ZERO);
    let bounds = PhysicalRect::new(origin, PhysicalSize::new(width.max(1), height.max(1)));
    let key = stream
        .id()
        .map(|id| format!("wayland-portal-{id}"))
        .unwrap_or_else(|| format!("wayland-portal-node-{}", stream.pipe_wire_node_id()));
    let scale = if logical_width > 0 {
        ScaleFactor::new(width as f64 / logical_width as f64)
    } else {
        ScaleFactor::new(1.0)
    };
    MonitorInfo {
        id: MonitorId::from_stable_key(&key),
        name: stream
            .id()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "Wayland portal monitor".to_string()),
        bounds,
        work_area: bounds,
        scale_factor: scale,
        is_primary: true,
    }
}

struct PipewireCaptureData {
    format: VideoInfoRaw,
    frame_sent: bool,
    sender: std::sync::mpsc::SyncSender<Result<RawPipewireFrame, String>>,
}

struct RawPipewireFrame {
    width: u32,
    height: u32,
    stride: i32,
    format: VideoFormat,
    pixels: Vec<u8>,
}

fn capture_pipewire_frame(
    node_id: u32,
    pipewire_fd: OwnedFd,
    origin: PhysicalPoint,
) -> Result<CapturedFrame, CaptureError> {
    pw::init();
    let main_loop = pw::main_loop::MainLoop::new(None).map_err(|error| {
        CaptureError::BackendUnavailable(format!("PipeWire main loop: {error}"))
    })?;
    let context = pw::context::Context::new(&main_loop)
        .map_err(|error| CaptureError::BackendUnavailable(format!("PipeWire context: {error}")))?;
    let core = context
        .connect_fd(pipewire_fd, None)
        .map_err(|error| CaptureError::BackendUnavailable(format!("PipeWire connect: {error}")))?;
    let stream = pw::stream::Stream::new(
        &core,
        "capture-core-wayland",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|error| CaptureError::BackendUnavailable(format!("PipeWire stream: {error}")))?;

    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let listener = stream
        .add_local_listener_with_user_data(PipewireCaptureData {
            format: VideoInfoRaw::default(),
            frame_sent: false,
            sender,
        })
        .state_changed(|_, data, _, state| {
            if let pw::stream::StreamState::Error(error) = state {
                if !data.frame_sent {
                    let _ = data.sender.try_send(Err(format!("PipeWire stream: {error}")));
                    data.frame_sent = true;
                }
            }
        })
        .param_changed(|_, data, id, param| {
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            if let Some(param) = param {
                let _ = data.format.parse(param);
            }
        })
        .process(|stream, data| {
            if data.frame_sent {
                return;
            }
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data_block) = buffer.datas_mut().first_mut() else {
                let _ = data.sender.try_send(Err("PipeWire buffer has no data plane".to_string()));
                data.frame_sent = true;
                return;
            };
            let offset = data_block.chunk().offset() as usize;
            let size = data_block.chunk().size() as usize;
            let stride = data_block.chunk().stride();
            let Some(bytes) = data_block.data() else {
                let _ = data.sender.try_send(Err(
                    "PipeWire frame uses an unmapped buffer; DMA-BUF import is not available in Core"
                        .to_string(),
                ));
                data.frame_sent = true;
                return;
            };
            match copy_pipewire_frame(&data.format, bytes, offset, size, stride) {
                Ok(frame) => {
                    let _ = data.sender.try_send(Ok(frame));
                }
                Err(error) => {
                    let _ = data.sender.try_send(Err(error));
                }
            }
            data.frame_sent = true;
        })
        .register()
        .map_err(|error| CaptureError::BackendUnavailable(format!("PipeWire listener: {error}")))?;

    let values = build_pipewire_video_params().map_err(CaptureError::CaptureFailed)?;
    let pod = spa::pod::Pod::from_bytes(&values).ok_or_else(|| {
        CaptureError::CaptureFailed("failed to build PipeWire video format pod".to_string())
    })?;
    let mut params = [pod];
    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|error| {
            CaptureError::BackendUnavailable(format!("PipeWire stream connect: {error}"))
        })?;

    let deadline = Instant::now() + Duration::from_secs(10);
    let raw = loop {
        match receiver.try_recv() {
            Ok(result) => break result.map_err(CaptureError::CaptureFailed)?,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(CaptureError::CaptureFailed(
                    "PipeWire capture callback disconnected".to_string(),
                ))
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            return Err(CaptureError::CaptureFailed(
                "timed out waiting for the first Wayland PipeWire frame".to_string(),
            ));
        }
        main_loop.loop_().iterate(Duration::from_millis(100));
    };
    drop(listener);
    raw_to_frame(raw, origin)
}

fn copy_pipewire_frame(
    format: &VideoInfoRaw,
    bytes: &[u8],
    offset: usize,
    size: usize,
    stride: i32,
) -> Result<RawPipewireFrame, String> {
    if !matches!(
        format.format(),
        VideoFormat::RGBA | VideoFormat::BGRA | VideoFormat::RGBx | VideoFormat::BGRx
    ) {
        return Err(format!(
            "PipeWire negotiated unsupported video format: {:?}",
            format.format()
        ));
    }
    let rectangle = format.size();
    let width = rectangle.width;
    let height = rectangle.height;
    if width == 0 || height == 0 {
        return Err("PipeWire negotiated an empty video frame".to_string());
    }
    let source_stride = if stride > 0 {
        usize::try_from(stride).map_err(|_| "PipeWire stride overflows usize".to_string())?
    } else {
        return Err("PipeWire returned a non-positive video stride".to_string());
    };
    let row_bytes = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or_else(|| "PipeWire row size overflows usize".to_string())?;
    let end = offset
        .checked_add(size)
        .ok_or_else(|| "PipeWire chunk bounds overflow".to_string())?;
    if end > bytes.len() {
        return Err("PipeWire chunk lies outside the mapped buffer".to_string());
    }
    let required = usize::try_from(height - 1)
        .ok()
        .and_then(|last| last.checked_mul(source_stride))
        .and_then(|last| last.checked_add(row_bytes))
        .and_then(|last| offset.checked_add(last))
        .ok_or_else(|| "PipeWire frame bounds overflow".to_string())?;
    if required > end {
        return Err("PipeWire frame is shorter than its negotiated geometry".to_string());
    }
    Ok(RawPipewireFrame {
        width,
        height,
        stride,
        format: format.format(),
        pixels: bytes[offset..end].to_vec(),
    })
}

fn raw_to_frame(
    raw: RawPipewireFrame,
    origin: PhysicalPoint,
) -> Result<CapturedFrame, CaptureError> {
    let row_bytes = usize::try_from(raw.width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or_else(|| {
            CaptureError::InvalidFrame("Wayland row size overflows usize".to_string())
        })?;
    let stride = usize::try_from(raw.stride)
        .map_err(|_| CaptureError::InvalidFrame("Wayland stride is invalid".to_string()))?;
    let output_len = row_bytes.checked_mul(raw.height as usize).ok_or_else(|| {
        CaptureError::InvalidFrame("Wayland frame size overflows usize".to_string())
    })?;
    let mut rgba = vec![0u8; output_len];
    for y in 0..raw.height as usize {
        let source_start = y.checked_mul(stride).ok_or_else(|| {
            CaptureError::InvalidFrame("Wayland row offset overflows usize".to_string())
        })?;
        let source_end = source_start.checked_add(row_bytes).ok_or_else(|| {
            CaptureError::InvalidFrame("Wayland row end overflows usize".to_string())
        })?;
        let source = raw
            .pixels
            .get(source_start..source_end)
            .ok_or_else(|| CaptureError::InvalidFrame("Wayland row is unavailable".to_string()))?;
        let destination = &mut rgba[y * row_bytes..(y + 1) * row_bytes];
        for (source_pixel, destination_pixel) in
            source.chunks_exact(4).zip(destination.chunks_exact_mut(4))
        {
            match raw.format {
                VideoFormat::RGBA => destination_pixel.copy_from_slice(source_pixel),
                VideoFormat::BGRA => destination_pixel.copy_from_slice(&[
                    source_pixel[2],
                    source_pixel[1],
                    source_pixel[0],
                    source_pixel[3],
                ]),
                VideoFormat::RGBx => destination_pixel.copy_from_slice(&[
                    source_pixel[0],
                    source_pixel[1],
                    source_pixel[2],
                    255,
                ]),
                VideoFormat::BGRx => destination_pixel.copy_from_slice(&[
                    source_pixel[2],
                    source_pixel[1],
                    source_pixel[0],
                    255,
                ]),
                format => {
                    return Err(CaptureError::FormatUnsupported(format_to_pixel_format(
                        format,
                    )))
                }
            }
        }
    }
    Ok(CapturedFrame::new(
        rgba.into(),
        raw.width,
        raw.height,
        raw.width.saturating_mul(4),
        origin,
        PixelFormat::Rgba8,
    ))
}

fn format_to_pixel_format(format: VideoFormat) -> PixelFormat {
    if format == VideoFormat::RGBx || format == VideoFormat::RGBA {
        PixelFormat::Rgba8
    } else if format == VideoFormat::BGRx || format == VideoFormat::BGRA {
        PixelFormat::Bgra8
    } else {
        PixelFormat::Rgb24
    }
}

fn build_pipewire_video_params() -> Result<Vec<u8>, String> {
    let object = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::RGBx,
            VideoFormat::BGRA,
            VideoFormat::RGBA
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle {
                width: 1280,
                height: 720
            },
            spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            spa::utils::Rectangle {
                width: 8192,
                height: 8192
            }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 30, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 120, denom: 1 }
        )
    );
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map(|(bytes, _)| bytes.into_inner())
    .map_err(|error| format!("PipeWire format serialization failed: {error:?}"))
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

    #[test]
    fn pipewire_bgrx_frame_is_converted_to_rgba() {
        let mut format = VideoInfoRaw::default();
        format.set_format(VideoFormat::BGRx);
        format.set_size(spa::utils::Rectangle {
            width: 1,
            height: 1,
        });
        let raw = copy_pipewire_frame(&format, &[10, 20, 30, 0], 0, 4, 4)
            .expect("BGRx frame should be copied");
        let frame = raw_to_frame(raw, PhysicalPoint::new(-4, 8)).expect("frame should convert");
        assert_eq!(frame.origin, PhysicalPoint::new(-4, 8));
        assert_eq!(frame.pixels.as_ref(), &[30, 20, 10, 255]);
    }

    #[test]
    fn pipewire_unsupported_format_is_rejected_before_copy() {
        let mut format = VideoInfoRaw::default();
        format.set_format(VideoFormat::RGB);
        format.set_size(spa::utils::Rectangle {
            width: 1,
            height: 1,
        });
        assert!(copy_pipewire_frame(&format, &[], 0, 0, 3).is_err());
    }
}
