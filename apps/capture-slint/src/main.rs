#![allow(clippy::type_complexity)]

use arboard::{Clipboard, ImageData};
use capture_actions::{ActionOutcome, CaptureAction, CopyAction, PinAction, SaveAction};
use capture_annotation::{
    Annotation, AnnotationTool, CaptureCommand, CaptureEvent, CaptureSession, CaptureSessionState,
};
use capture_core::capture::CapturedFrame;
use capture_core::geometry::{PhysicalPoint, PhysicalRect, PhysicalSize};
use capture_core::selection::{ResizeHandle, SelectionInteraction, SelectionSession};
use capture_core::{
    place_toolbar, MonitorInfo, SnapCandidate, SnapExclusionToken, SnapKind, ToolbarPlacementReason,
};
use capture_platform_api::{CaptureBackend, SnapBackend};
use capture_render::flatten;
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::cell::RefCell;
use std::fmt::Display;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

slint::include_modules!();

#[derive(Clone)]
struct TraceLog {
    origin: Instant,
    file: Option<Arc<Mutex<BufWriter<File>>>>,
}

impl TraceLog {
    fn new() -> Self {
        let file = std::env::var_os("CAPTURE_SLINT_LOG").and_then(|path| {
            let path_display = PathBuf::from(path.clone()).display().to_string();
            match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(file) => Some(Arc::new(Mutex::new(BufWriter::new(file)))),
                Err(error) => {
                    eprintln!(
                        "[capture-slint] logger.open.error path={path_display} error={error}"
                    );
                    None
                }
            }
        });
        let logger = Self {
            origin: Instant::now(),
            file,
        };
        logger.event(
            "logger.ready",
            format!(
                "file={}",
                if std::env::var_os("CAPTURE_SLINT_LOG").is_some() {
                    "enabled"
                } else {
                    "stderr"
                }
            ),
        );
        logger
    }

    fn event(&self, stage: &str, detail: impl Display) {
        let line = format!(
            "[{:.3} ms] {stage} {detail}",
            self.origin.elapsed().as_secs_f64() * 1000.0
        );
        eprintln!("{line}");
        if let Some(file) = &self.file {
            if let Ok(mut file) = file.lock() {
                let _ = writeln!(file, "{line}");
            }
        }
    }

    fn flush(&self) {
        if let Some(file) = &self.file {
            if let Ok(mut file) = file.lock() {
                let _ = file.flush();
            }
        }
    }

    fn duration(&self, stage: &str, started: Instant) {
        self.event(
            stage,
            format!(
                "duration_ms={:.3}",
                started.elapsed().as_secs_f64() * 1000.0
            ),
        );
    }
}

#[cfg(target_os = "linux")]
use capture_linux::LinuxPlatform;
#[cfg(windows)]
use capture_windows::WindowsPlatform;

enum HostPlatform {
    #[cfg(windows)]
    Windows(WindowsPlatform),
    #[cfg(target_os = "linux")]
    Linux(LinuxPlatform),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointerGesture {
    Select,
    Annotate,
    Move,
    Resize(ResizeHandle),
}

#[derive(Clone, Copy, Debug)]
struct EditorLayout {
    window_bounds: PhysicalRect,
    toolbar_rect: PhysicalRect,
    work_area: PhysicalRect,
    toolbar_reason: ToolbarPlacementReason,
}

impl HostPlatform {
    fn capture(&self) -> &dyn CaptureBackend {
        match self {
            #[cfg(windows)]
            Self::Windows(platform) => platform.capture_backend(),
            #[cfg(target_os = "linux")]
            Self::Linux(platform) => platform.capture_backend(),
        }
    }

    fn snap(&self) -> &dyn SnapBackend {
        match self {
            #[cfg(windows)]
            Self::Windows(platform) => platform.snap_backend(),
            #[cfg(target_os = "linux")]
            Self::Linux(platform) => platform.snap_backend(),
        }
    }
}

struct Controller {
    host: HostPlatform,
    session: CaptureSession,
    frame: Arc<CapturedFrame>,
    log: TraceLog,
    scale_factor: f64,
    status: String,
    pin_window: Option<PinWindow>,
    last_snap_at: Option<Instant>,
    last_visual_at: Option<Instant>,
    pointer_moves: u64,
    last_move_log_at: Option<Instant>,
    monitors: Vec<MonitorInfo>,
    pointer_gesture: Option<PointerGesture>,
    last_pointer: Option<PhysicalPoint>,
    toolbar_override: Option<PhysicalPoint>,
    toolbar_drag_offset: Option<PhysicalPoint>,
    overlay_token: Option<SnapExclusionToken>,
    last_toolbar_log_at: std::cell::Cell<Option<Instant>>,
}

fn make_host() -> Result<HostPlatform, String> {
    #[cfg(windows)]
    {
        return WindowsPlatform::new()
            .map(HostPlatform::Windows)
            .map_err(|error| error.to_string());
    }
    #[cfg(target_os = "linux")]
    {
        return Ok(HostPlatform::Linux(LinuxPlatform::new()));
    }
    #[allow(unreachable_code)]
    Err("capture-slint has no backend for this target".to_string())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log = TraceLog::new();
    log.event(
        "startup.begin",
        format!(
            "capture_monitor={} slint_backend={} display={} wayland_display={}",
            std::env::var("CAPTURE_MONITOR").unwrap_or_else(|_| "<unset>".to_string()),
            std::env::var("SLINT_BACKEND").unwrap_or_else(|_| "<unset>".to_string()),
            std::env::var("DISPLAY").unwrap_or_else(|_| "<unset>".to_string()),
            std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "<unset>".to_string()),
        ),
    );
    let host_started = Instant::now();
    let host = make_host()?;
    log.duration("startup.host_ready", host_started);
    let ordinal = std::env::var("CAPTURE_MONITOR")
        .ok()
        .and_then(|value| value.parse::<usize>().ok());
    let (frame, _capture_scale_factor, status) = if let Some(ordinal) = ordinal {
        let monitors_started = Instant::now();
        let monitors = host.capture().monitors()?;
        log.duration("startup.monitors", monitors_started);
        let monitor = monitors
            .get(ordinal)
            .ok_or_else(|| format!("monitor ordinal {ordinal} is unavailable"))?;
        log.event(
            "startup.monitor.selected",
            format!(
                "ordinal={ordinal} id={} name={} bounds={}x{}+{}+{} scale={:.3}",
                monitor.id.0,
                monitor.name,
                monitor.bounds.width(),
                monitor.bounds.height(),
                monitor.bounds.origin.x,
                monitor.bounds.origin.y,
                monitor.scale_factor.get(),
            ),
        );
        let capture_started = Instant::now();
        let captured = host.capture().capture_monitor(monitor.id)?;
        log.duration("startup.capture_monitor", capture_started);
        let rgba_started = Instant::now();
        let frame = Arc::new(captured.to_rgba8()?);
        log.duration("startup.frame_to_rgba8", rgba_started);
        (
            frame,
            monitor.scale_factor.get().max(0.1),
            format!(
                "{}  {}x{}",
                monitor.name,
                monitor.bounds.width(),
                monitor.bounds.height()
            ),
        )
    } else {
        let capture_started = Instant::now();
        let captured = host.capture().capture_virtual_desktop()?;
        log.duration("startup.capture_virtual_desktop", capture_started);
        let rgba_started = Instant::now();
        let frame = Arc::new(captured.to_rgba8()?);
        log.duration("startup.frame_to_rgba8", rgba_started);
        (
            frame.clone(),
            1.0,
            format!("Virtual desktop  {}x{}", frame.width, frame.height),
        )
    };
    log.event(
        "startup.frame_ready",
        format!(
            "width={} height={} stride={} origin=({}, {}) bytes={}",
            frame.width,
            frame.height,
            frame.stride,
            frame.origin.x,
            frame.origin.y,
            frame.pixels.len(),
        ),
    );

    let monitors = match host.capture().monitors() {
        Ok(monitors) => monitors,
        Err(error) => {
            log.event("startup.monitors.error", format!("error={error}"));
            Vec::new()
        }
    };
    for monitor in &monitors {
        log.event(
            "startup.monitor",
            format!(
                "name={} bounds={}x{}+{}+{} work_area={}x{}+{}+{} scale={:.3}",
                monitor.name,
                monitor.bounds.width(),
                monitor.bounds.height(),
                monitor.bounds.origin.x,
                monitor.bounds.origin.y,
                monitor.work_area.width(),
                monitor.work_area.height(),
                monitor.work_area.origin.x,
                monitor.work_area.origin.y,
                monitor.scale_factor.get(),
            ),
        );
    }

    let ui_started = Instant::now();
    let ui = CaptureWindow::new()?;
    log.duration("startup.ui_created", ui_started);
    let ui_scale_hint = std::env::var("SLINT_SCALE_FACTOR")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| *value > 0.1)
        .unwrap_or(1.0);
    if ui_scale_hint > 1.01 {
        log.event(
            "startup.ui_scale_hint",
            format!("scale_factor={ui_scale_hint:.3}"),
        );
    }
    let window_size_started = Instant::now();
    ui.window()
        .set_size(slint::PhysicalSize::new(frame.width, frame.height));
    log.duration("startup.window_set_size", window_size_started);
    let window_position_started = Instant::now();
    ui.window()
        .set_position(slint::PhysicalPosition::new(frame.origin.x, frame.origin.y));
    log.duration("startup.window_set_position", window_position_started);
    let state = Rc::new(RefCell::new(Controller {
        host,
        session: CaptureSession::new(),
        frame: frame.clone(),
        log: log.clone(),
        scale_factor: ui_scale_hint,
        status,
        pin_window: None,
        last_snap_at: None,
        last_visual_at: None,
        pointer_moves: 0,
        last_move_log_at: None,
        monitors,
        pointer_gesture: None,
        last_pointer: None,
        toolbar_override: None,
        toolbar_drag_offset: None,
        overlay_token: None,
        last_toolbar_log_at: std::cell::Cell::new(None),
    }));

    {
        let mut controller = state.borrow_mut();
        let initial_refresh_started = Instant::now();
        let session_begin_started = Instant::now();
        controller.session.apply(CaptureCommand::Begin);
        controller
            .log
            .duration("startup.session_begin", session_begin_started);
        let session_frame_started = Instant::now();
        controller
            .session
            .apply(CaptureCommand::FrameReady((*frame).clone()));
        controller
            .log
            .duration("startup.session_frame_ready", session_frame_started);
        let image_started = Instant::now();
        ui.set_frame_image(image_from_frame(&controller.frame));
        ui.set_canvas_cursor_kind("default".into());
        ui.set_toolbar_cursor_kind("grab".into());
        controller.log.duration("render.frame_image", image_started);
        refresh_selection_geometry(&ui, &controller);
        refresh_editor_overlay(&ui, &controller);
        controller
            .log
            .duration("startup.initial_ui_refresh", initial_refresh_started);
    }
    log.event("startup.initial_ui_ready", "true");

    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_pointer_down(move |x, y| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let mut controller = state.borrow_mut();
            let point = controller.to_physical(x, y);
            let started = Instant::now();
            controller.log.event(
                "input.down.begin",
                format!(
                    "x={x:.1} y={y:.1} physical=({}, {}) state={}",
                    point.x,
                    point.y,
                    state_label(controller.session.state())
                ),
            );

            controller.last_pointer = Some(point);
            ui.set_canvas_cursor_kind(cursor_kind_for_point(&controller, point).into());
            match controller.session.state() {
                CaptureSessionState::Selecting(selection)
                    if selection.interaction == SelectionInteraction::Hovering
                        && controller
                            .session
                            .hover_candidate()
                            .is_some_and(is_committable_snap) =>
                {
                    // Defer the commit until pointer-up so a drag over a
                    // window still creates a free selection.
                    controller.pointer_gesture = Some(PointerGesture::Select);
                }
                CaptureSessionState::Selecting(_) => {
                    controller.pointer_gesture = Some(PointerGesture::Select);
                    controller.apply(CaptureCommand::BeginFreeSelection(point));
                }
                CaptureSessionState::Editing(editor) => match editor.selected_tool {
                    AnnotationTool::Pointer => {
                        let crop = editor.document.crop;
                        let mut selection = SelectionSession::new();
                        selection.rect = crop;
                        let handle =
                            selection.hit_resize_handle(point, handle_tolerance(&controller));
                        if let Some(handle) = handle {
                            controller.pointer_gesture = Some(PointerGesture::Resize(handle));
                        } else if editor.document.crop.contains_exclusive(point) {
                            controller.pointer_gesture = Some(PointerGesture::Move);
                        }
                    }
                    AnnotationTool::Pen | AnnotationTool::Rectangle => {
                        controller.pointer_gesture = Some(PointerGesture::Annotate);
                        controller.apply(CaptureCommand::BeginAnnotation(point));
                    }
                },
                _ => return,
            }
            controller.log.duration("input.down.apply", started);
            let refresh_started = Instant::now();
            refresh_pointer_visuals(&ui, &controller);
            controller
                .log
                .duration("input.down.refresh", refresh_started);
            controller.log.duration("input.down.total", started);
        });
    }

    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_pointer_move(move |x, y| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let mut controller = state.borrow_mut();
            let point = controller.to_physical(x, y);
            ui.set_canvas_cursor_kind(cursor_kind_for_point(&controller, point).into());
            controller.pointer_moves = controller.pointer_moves.saturating_add(1);
            let gesture = controller.pointer_gesture;
            let dragging = matches!(
                controller.session.state(),
                CaptureSessionState::Selecting(selection)
                    if selection.interaction == SelectionInteraction::Dragging
            );
            if matches!(
                controller.session.state(),
                CaptureSessionState::Selecting(_)
            ) {
                if !dragging && controller.should_query_snap() {
                    controller.sync_snap_exclusions(&ui);
                    let snap_started = Instant::now();
                    match controller.host.snap().candidates_at(point) {
                        Ok(candidates) => {
                            let count = candidates.len();
                            controller.apply(CaptureCommand::SnapCandidate(
                                candidates.into_iter().next(),
                            ));
                            controller.log.event(
                                "snap.query",
                                format!(
                                    "point=({}, {}) candidates={count} duration_ms={:.3}",
                                    point.x,
                                    point.y,
                                    snap_started.elapsed().as_secs_f64() * 1000.0
                                ),
                            );
                        }
                        Err(error) => {
                            controller.log.event(
                                "snap.query.error",
                                format!(
                                    "point=({}, {}) duration_ms={:.3} error={error}",
                                    point.x,
                                    point.y,
                                    snap_started.elapsed().as_secs_f64() * 1000.0
                                ),
                            );
                            controller.status = format!("Snap unavailable: {error}");
                        }
                    }
                }

                if gesture == Some(PointerGesture::Select) && !dragging {
                    if let Some(start) = controller.last_pointer {
                        if moved_enough(start, point) {
                            controller.apply(CaptureCommand::BeginFreeSelection(start));
                            controller.apply(CaptureCommand::UpdateFreeSelection(point));
                        }
                    }
                } else if dragging {
                    controller.apply(CaptureCommand::UpdateFreeSelection(point));
                }
            } else if matches!(controller.session.state(), CaptureSessionState::Editing(_)) {
                match gesture {
                    Some(PointerGesture::Move) => {
                        if let Some(last) = controller.last_pointer {
                            controller.apply(CaptureCommand::MoveSelection(point - last));
                        }
                    }
                    Some(PointerGesture::Resize(handle)) => {
                        controller.apply(CaptureCommand::ResizeSelection(handle, point));
                    }
                    Some(PointerGesture::Annotate) => {
                        controller.apply(CaptureCommand::UpdateAnnotation(point));
                    }
                    _ => {}
                }
            }
            controller.last_pointer = Some(point);
            ui.set_canvas_cursor_kind(cursor_kind_for_point(&controller, point).into());
            if controller.should_render_visuals() {
                let refresh_started = Instant::now();
                refresh_pointer_visuals(&ui, &controller);
                controller.log.duration("visual.pointer", refresh_started);
            }
            if controller
                .last_move_log_at
                .is_none_or(|last| last.elapsed() >= Duration::from_millis(500))
            {
                controller.last_move_log_at = Some(Instant::now());
                controller.log.event(
                    "input.move.sample",
                    format!(
                        "events={} point=({}, {}) state={}",
                        controller.pointer_moves,
                        point.x,
                        point.y,
                        state_label(controller.session.state()),
                    ),
                );
            }
        });
    }

    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_pointer_up(move |x, y| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let mut controller = state.borrow_mut();
            let started = Instant::now();
            let point = controller.to_physical(x, y);
            match controller.session.state() {
                CaptureSessionState::Selecting(_) => {
                    controller.apply(CaptureCommand::CommitSelection)
                }
                CaptureSessionState::Editing(_) => {
                    if controller.pointer_gesture == Some(PointerGesture::Annotate) {
                        controller.apply(CaptureCommand::EndAnnotation);
                    }
                }
                _ => return,
            };
            controller.last_pointer = Some(point);
            controller.pointer_gesture = None;
            controller.last_pointer = None;
            refresh_pointer_visuals(&ui, &controller);
            controller.log.duration("input.up.total", started);
        });
    }

    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_choose_tool(move |tool| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let mut controller = state.borrow_mut();
            let tool = match tool.as_str() {
                "pointer" => AnnotationTool::Pointer,
                "rectangle" => AnnotationTool::Rectangle,
                "pen" => AnnotationTool::Pen,
                _ => return,
            };
            controller.apply(CaptureCommand::SelectTool(tool));
            refresh_editor_ui(&ui, &controller, false);
        });
    }

    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_toolbar_pointer_down(move |x, y| {
            let Some(_ui) = ui_weak.upgrade() else {
                return;
            };
            let mut controller = state.borrow_mut();
            let point = controller.to_physical(x, y);
            let Some(layout) = editor_layout(&controller) else {
                return;
            };
            if layout.toolbar_rect.contains_exclusive(point) {
                controller.toolbar_drag_offset = Some(point - layout.toolbar_rect.origin);
                _ui.set_toolbar_cursor_kind("grabbing".into());
            }
            controller.pointer_gesture = None;
        });
    }

    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_toolbar_pointer_move(move |x, y| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let mut controller = state.borrow_mut();
            let point = controller.to_physical(x, y);
            if let Some(offset) = controller.toolbar_drag_offset {
                let next = point - offset;
                controller.toolbar_override = Some(next);
                refresh_selection_geometry(&ui, &controller);
            }
        });
    }

    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_toolbar_pointer_up(move || {
            let Some(_ui) = ui_weak.upgrade() else {
                return;
            };
            state.borrow_mut().toolbar_drag_offset = None;
            _ui.set_toolbar_cursor_kind("grab".into());
        });
    }

    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_run_action(move |action| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let mut controller = state.borrow_mut();
            controller.run_action(action.as_str(), &ui);
        });
    }

    let show_started = Instant::now();
    ui.show()?;
    log.duration("startup.ui_show", show_started);
    {
        let mut controller = state.borrow_mut();
        let actual_scale = ui.window().scale_factor() as f64;
        let capture_scale = controller.scale_factor;
        let scale_changed = (controller.scale_factor - actual_scale).abs() > 0.01;
        if ui_scale_hint <= 1.01 {
            controller.scale_factor = actual_scale;
        }
        let bounds = controller.frame.bounds();
        sync_window_geometry(&ui, &controller, bounds);
        if scale_changed {
            refresh_selection_geometry(&ui, &controller);
            refresh_editor_overlay(&ui, &controller);
            controller.log.event(
                "startup.scale_reconciled",
                format!("capture_scale={capture_scale:.3} ui_scale={actual_scale:.3}"),
            );
        }
        controller.sync_snap_exclusions(&ui);
        controller.log.event(
            "startup.window_ready",
            format!(
                "scale_factor={:.3} position=({}, {}) size={}x{}",
                controller.scale_factor,
                controller.frame.origin.x,
                controller.frame.origin.y,
                controller.frame.width,
                controller.frame.height,
            ),
        );
    }
    log.event("startup.event_loop.begin", "true");
    // Winit applies the native position/scale during the first event-loop
    // turn. Refresh once after that turn so the initial selecting overlay uses
    // the same geometry as subsequent tool changes.
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(1), move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let mut controller = state.borrow_mut();
            if ui_scale_hint <= 1.01 {
                controller.scale_factor = ui.window().scale_factor() as f64;
            }
            let bounds = controller.frame.bounds();
            sync_window_geometry(&ui, &controller, bounds);
            refresh_ui(&ui, &controller);
            controller.log.event("startup.deferred_ui_refresh", "true");
        });
    }
    let event_loop_started = Instant::now();
    slint::run_event_loop()?;
    log.event("shutdown.event_loop.end", "true");
    log.duration("shutdown.event_loop.total", event_loop_started);
    let cleanup_started = Instant::now();
    drop(ui);
    drop(state);
    log.duration("shutdown.resources_drop", cleanup_started);
    log.flush();
    Ok(())
}

fn state_label(state: &CaptureSessionState) -> &'static str {
    match state {
        CaptureSessionState::Idle => "idle",
        CaptureSessionState::Preparing => "preparing",
        CaptureSessionState::Selecting(_) => "selecting",
        CaptureSessionState::Editing(_) => "editing",
    }
}

fn cursor_kind_for_point(controller: &Controller, point: PhysicalPoint) -> &'static str {
    let CaptureSessionState::Editing(editor) = controller.session.state() else {
        return "default";
    };
    if editor.selected_tool != AnnotationTool::Pointer {
        return "crosshair";
    }
    let mut selection = SelectionSession::new();
    selection.rect = editor.document.crop;
    if let Some(handle) = selection.hit_resize_handle(point, handle_tolerance(controller)) {
        return match handle {
            ResizeHandle::TopLeft | ResizeHandle::BottomRight => "nwse-resize",
            ResizeHandle::TopRight | ResizeHandle::BottomLeft => "nesw-resize",
            ResizeHandle::Top | ResizeHandle::Bottom => "ns-resize",
            ResizeHandle::Left | ResizeHandle::Right => "ew-resize",
        };
    }
    if editor.document.crop.contains_exclusive(point) {
        "move"
    } else {
        "default"
    }
}

fn input_origin(_state: &CaptureSessionState, frame_origin: PhysicalPoint) -> PhysicalPoint {
    // The overlay remains the full virtual desktop in every state. Selection
    // and editor coordinates are therefore always translated from frame.origin.
    frame_origin
}

impl Controller {
    fn to_physical(&self, x: f32, y: f32) -> capture_core::PhysicalPoint {
        let frame = self.frame.as_ref();
        let origin = input_origin(self.session.state(), frame.origin);
        PhysicalPoint::new(
            origin
                .x
                .saturating_add((x as f64 * self.scale_factor).round() as i32),
            origin
                .y
                .saturating_add((y as f64 * self.scale_factor).round() as i32),
        )
    }

    fn sync_snap_exclusions(&mut self, ui: &CaptureWindow) {
        #[cfg(windows)]
        {
            let token = ui
                .window()
                .window_handle()
                .window_handle()
                .ok()
                .and_then(|handle| match handle.as_raw() {
                    RawWindowHandle::Win32(handle) => {
                        Some(SnapExclusionToken::new(handle.hwnd.get() as u64))
                    }
                    _ => None,
                });
            if token != self.overlay_token {
                let tokens = token.into_iter().collect::<Vec<_>>();
                self.host.snap().set_excluded_windows(&tokens);
                self.overlay_token = token;
                self.log.event(
                    "snap.exclusions",
                    format!("overlay_window={}", self.overlay_token.is_some()),
                );
            }
        }
        #[cfg(not(windows))]
        {
            let _ = ui;
        }
    }

    fn apply(&mut self, command: CaptureCommand) {
        for event in self.session.apply(command) {
            if let CaptureEvent::Error(error) = event {
                self.status = error.to_string();
            }
        }
    }

    fn should_query_snap(&mut self) -> bool {
        let now = Instant::now();
        if self
            .last_snap_at
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(32))
        {
            return false;
        }
        self.last_snap_at = Some(now);
        true
    }

    fn should_render_visuals(&mut self) -> bool {
        let now = Instant::now();
        if self
            .last_visual_at
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(16))
        {
            return false;
        }
        self.last_visual_at = Some(now);
        true
    }

    fn run_action(&mut self, action: &str, ui: &CaptureWindow) {
        self.log.event(
            "action.begin",
            format!(
                "action={action} state={}",
                state_label(self.session.state())
            ),
        );
        match action {
            "undo" => self.apply(CaptureCommand::Undo),
            "cancel" => {
                let apply_started = Instant::now();
                self.apply(CaptureCommand::Cancel);
                self.log.duration("shutdown.cancel.apply", apply_started);
                let hide_started = Instant::now();
                match ui.hide() {
                    Ok(()) => self.log.event(
                        "shutdown.cancel.hide",
                        format!(
                            "ok=true duration_ms={:.3}",
                            hide_started.elapsed().as_secs_f64() * 1000.0
                        ),
                    ),
                    Err(error) => self.log.event(
                        "shutdown.cancel.hide",
                        format!(
                            "ok=false duration_ms={:.3} error={error}",
                            hide_started.elapsed().as_secs_f64() * 1000.0
                        ),
                    ),
                }
                let quit_started = Instant::now();
                let _ = slint::quit_event_loop();
                self.log.duration("shutdown.cancel.quit", quit_started);
                return;
            }
            "copy" => {
                let Some(document) = self.document() else {
                    return;
                };
                match CopyAction.invoke(&document) {
                    Ok(ActionOutcome::Png(_)) => match copy_document_to_clipboard(&document) {
                        Ok(()) => self.status = "Copied capture to the clipboard".to_string(),
                        Err(error) => self.status = format!("Copy failed: {error}"),
                    },
                    Ok(_) => self.status = "Copy action returned an unexpected result".to_string(),
                    Err(error) => self.status = format!("Copy failed: {error}"),
                }
            }
            "save" => {
                let Some(document) = self.document() else {
                    return;
                };
                let path = PathBuf::from("capture-slint.png");
                match SaveAction::new(&path).invoke(&document) {
                    Ok(ActionOutcome::Saved(path)) => {
                        self.status = format!("Saved {}", path.display())
                    }
                    Ok(_) => self.status = "Save completed".to_string(),
                    Err(error) => self.status = format!("Save failed: {error}"),
                }
            }
            "pin" => {
                let Some(document) = self.document() else {
                    return;
                };
                match PinAction.invoke(&document) {
                    Ok(ActionOutcome::Pin(_payload)) => match PinWindow::new() {
                        Ok(pin) => {
                            let rendered = match flatten(&document) {
                                Ok(rendered) => rendered,
                                Err(error) => {
                                    self.status = format!("Pin render failed: {error}");
                                    return refresh_editor_ui(ui, self, false);
                                }
                            };
                            let image =
                                image_from_rgba(rendered.width, rendered.height, &rendered.pixels);
                            pin.set_pin_image(image);
                            pin.window().set_size(slint::PhysicalSize::new(
                                rendered.width,
                                rendered.height,
                            ));
                            pin.window().set_position(slint::PhysicalPosition::new(
                                document.crop.origin.x.saturating_add(16),
                                document.crop.origin.y.saturating_add(16),
                            ));
                            let pin_weak = pin.as_weak();
                            pin.on_close_requested(move || {
                                if let Some(pin) = pin_weak.upgrade() {
                                    let _ = pin.hide();
                                }
                            });
                            let _ = pin.show();
                            self.pin_window = Some(pin);
                            self.status = "Pinned capture in a floating window".to_string();
                        }
                        Err(error) => self.status = format!("Pin window failed: {error}"),
                    },
                    Ok(_) => self.status = "Pin completed".to_string(),
                    Err(error) => self.status = format!("Pin failed: {error}"),
                }
            }
            "ask-ai" => self.status = "Ask AI payload prepared (stub)".to_string(),
            _ => {}
        }
        if matches!(self.session.state(), CaptureSessionState::Editing(_)) {
            refresh_editor_ui(ui, self, false);
        } else {
            refresh_ui(ui, self);
        }
    }

    fn document(&self) -> Option<capture_annotation::CaptureDocument> {
        match self.session.state() {
            CaptureSessionState::Editing(editor) => Some(editor.document.clone()),
            _ => None,
        }
    }
}

fn refresh_ui(ui: &CaptureWindow, controller: &Controller) {
    let started = Instant::now();
    match controller.session.state() {
        CaptureSessionState::Selecting(_) => {
            let image_started = Instant::now();
            ui.set_frame_image(image_from_frame(&controller.frame));
            controller.log.duration("render.frame_image", image_started);
            sync_window_geometry(ui, controller, controller.frame.bounds());
        }
        CaptureSessionState::Editing(_) => {
            refresh_editor_ui(ui, controller, false);
            controller.log.event(
                "render.refresh_ui",
                format!(
                    "state={} duration_ms={:.3}",
                    state_label(controller.session.state()),
                    started.elapsed().as_secs_f64() * 1000.0
                ),
            );
            return;
        }
        CaptureSessionState::Idle | CaptureSessionState::Preparing => {
            let image_started = Instant::now();
            ui.set_frame_image(image_from_frame(&controller.frame));
            controller.log.duration("render.frame_image", image_started);
            sync_window_geometry(ui, controller, controller.frame.bounds());
        }
    }
    refresh_selection_geometry(ui, controller);
    refresh_editor_overlay(ui, controller);
    controller.log.event(
        "render.refresh_ui",
        format!(
            "state={} duration_ms={:.3}",
            state_label(controller.session.state()),
            started.elapsed().as_secs_f64() * 1000.0
        ),
    );
}

fn refresh_pointer_visuals(ui: &CaptureWindow, controller: &Controller) {
    match controller.session.state() {
        CaptureSessionState::Selecting(_) => refresh_selection_geometry(ui, controller),
        CaptureSessionState::Editing(_) => {
            refresh_selection_geometry(ui, controller);
            refresh_editor_overlay(ui, controller);
        }
        CaptureSessionState::Idle | CaptureSessionState::Preparing => refresh_ui(ui, controller),
    }
}

fn refresh_editor_ui(ui: &CaptureWindow, controller: &Controller, refresh_base: bool) {
    let started = Instant::now();
    let CaptureSessionState::Editing(editor) = controller.session.state() else {
        refresh_ui(ui, controller);
        return;
    };
    if refresh_base {
        refresh_editor_base(ui, controller);
    }
    if let Some(layout) = editor_layout(controller) {
        sync_window_geometry(ui, controller, layout.window_bounds);
    }
    refresh_selection_geometry(ui, controller);
    refresh_editor_overlay(ui, controller);
    controller.log.event(
        "render.editor_ui",
        format!(
            "base={} annotations={} duration_ms={:.3}",
            refresh_base,
            editor.document.annotations.len(),
            started.elapsed().as_secs_f64() * 1000.0
        ),
    );
}

fn refresh_editor_base(ui: &CaptureWindow, controller: &Controller) {
    // Keep the editor on the same full-desktop canvas as the selection state.
    // Copy/save/pin flatten the document separately, while the live overlay
    // draws annotations in global coordinates.
    ui.set_frame_image(image_from_frame(&controller.frame));
}

fn refresh_editor_overlay(ui: &CaptureWindow, controller: &Controller) {
    let (path, width, visible) = match controller.session.state() {
        CaptureSessionState::Editing(editor) => {
            let mut path = String::new();
            let mut width = 0.0;
            let scale = controller.scale_factor as f32;
            for annotation in &editor.document.annotations {
                let (annotation_path, annotation_width) =
                    annotation_path(annotation, controller.frame.origin, scale);
                path.push_str(&annotation_path);
                width = annotation_width;
            }
            if let Some(annotation) = editor.active_preview() {
                let (annotation_path, annotation_width) =
                    annotation_path(&annotation, controller.frame.origin, scale);
                path.push_str(&annotation_path);
                width = annotation_width;
            }
            let visible = !path.is_empty();
            (path, width, visible)
        }
        _ => (String::new(), 0.0, false),
    };
    ui.set_annotation_path(path.into());
    ui.set_annotation_width(width);
    ui.set_annotation_visible(visible);
}

fn refresh_selection_geometry(ui: &CaptureWindow, controller: &Controller) {
    let (rect, editing, tool, window_origin, toolbar) = match controller.session.state() {
        CaptureSessionState::Selecting(selection) => (
            selection.rect,
            false,
            "pointer",
            controller.frame.origin,
            None,
        ),
        CaptureSessionState::Editing(editor) => {
            let Some(layout) = editor_layout(controller) else {
                return;
            };
            (
                editor.document.crop,
                true,
                editor.selected_tool.id(),
                controller.frame.origin,
                Some((layout.toolbar_rect, layout.toolbar_reason, layout.work_area)),
            )
        }
        CaptureSessionState::Idle | CaptureSessionState::Preparing => (
            PhysicalRect::default(),
            false,
            "pointer",
            controller.frame.origin,
            None,
        ),
    };
    let scale = controller.scale_factor as f32;
    ui.set_selection_x((rect.origin.x - window_origin.x) as f32 / scale);
    ui.set_selection_y((rect.origin.y - window_origin.y) as f32 / scale);
    ui.set_selection_width(rect.size.width as f32 / scale);
    ui.set_selection_height(rect.size.height as f32 / scale);
    ui.set_selecting(!editing);
    ui.set_editing(editing);
    ui.set_active_tool(tool.into());
    if let Some((toolbar_rect, reason, work_area)) = toolbar {
        ui.set_toolbar_x((toolbar_rect.origin.x - window_origin.x) as f32 / scale);
        ui.set_toolbar_y((toolbar_rect.origin.y - window_origin.y) as f32 / scale);
        ui.set_toolbar_visible(true);
        ui.set_toolbar_inside(reason == ToolbarPlacementReason::InsideBottom);
        if controller
            .last_toolbar_log_at
            .get()
            .is_none_or(|last| last.elapsed() >= Duration::from_millis(250))
        {
            controller.last_toolbar_log_at.set(Some(Instant::now()));
            controller.log.event(
                "toolbar.layout",
                format!(
                    "reason={reason:?} rect={}x{}+{}+{} work_area={}x{}+{}+{}",
                    toolbar_rect.width(),
                    toolbar_rect.height(),
                    toolbar_rect.origin.x,
                    toolbar_rect.origin.y,
                    work_area.width(),
                    work_area.height(),
                    work_area.origin.x,
                    work_area.origin.y,
                ),
            );
        }
    } else {
        ui.set_toolbar_visible(false);
        ui.set_toolbar_inside(false);
    }
    ui.set_status(controller.status.clone().into());
}

fn annotation_path(
    annotation: &Annotation,
    canvas_origin: PhysicalPoint,
    scale: f32,
) -> (String, f32) {
    let local = |point: PhysicalPoint| {
        (
            (point.x - canvas_origin.x) as f32 / scale,
            (point.y - canvas_origin.y) as f32 / scale,
        )
    };
    match annotation {
        Annotation::Pen(stroke) => {
            let mut path = String::new();
            for (index, point) in stroke.points.iter().enumerate() {
                let (x, y) = local(*point);
                if index == 0 {
                    path.push_str(&format!("M {} {} ", x, y));
                } else {
                    path.push_str(&format!("L {} {} ", x, y));
                }
            }
            (path, stroke.thickness as f32 / scale)
        }
        Annotation::Rectangle(rectangle) => {
            let (left, top) = local(rectangle.rect.origin);
            let (right, bottom) = local(PhysicalPoint::new(
                rectangle.rect.right(),
                rectangle.rect.bottom(),
            ));
            (
                format!(
                    "M {} {} L {} {} L {} {} L {} {} Z",
                    left, top, right, top, right, bottom, left, bottom
                ),
                rectangle.thickness as f32 / scale,
            )
        }
    }
}

fn is_committable_snap(candidate: &SnapCandidate) -> bool {
    candidate.kind != SnapKind::Desktop && !candidate.bounds.is_empty()
}

fn handle_tolerance(controller: &Controller) -> u32 {
    ((10.0 * controller.scale_factor).round() as u32).max(6)
}

fn moved_enough(start: PhysicalPoint, current: PhysicalPoint) -> bool {
    (current.x as i64 - start.x as i64).unsigned_abs() > 2
        || (current.y as i64 - start.y as i64).unsigned_abs() > 2
}

fn editor_layout(controller: &Controller) -> Option<EditorLayout> {
    let CaptureSessionState::Editing(editor) = controller.session.state() else {
        return None;
    };
    let selection = editor.document.crop;
    let frame_bounds = controller.frame.bounds();
    let work_area = controller
        .monitors
        .iter()
        .find(|monitor| monitor.work_area.contains(selection.center()))
        .map(|monitor| monitor.work_area)
        .or_else(|| {
            controller
                .monitors
                .iter()
                .find(|monitor| monitor.bounds.contains(selection.center()))
                .map(|monitor| monitor.work_area)
        })
        .unwrap_or(frame_bounds);
    let toolbar_size = PhysicalSize::new(
        (548.0 * controller.scale_factor).round().max(1.0) as u32,
        (64.0 * controller.scale_factor).round().max(1.0) as u32,
    );
    let placement = place_toolbar(selection, toolbar_size, work_area, 12);
    let (toolbar_rect, toolbar_reason) = if let Some(origin) = controller.toolbar_override {
        (
            PhysicalRect::new(origin, toolbar_size).clamp(work_area),
            ToolbarPlacementReason::Clamped,
        )
    } else {
        (placement.rect, placement.reason)
    };
    Some(EditorLayout {
        window_bounds: frame_bounds,
        toolbar_rect,
        work_area,
        toolbar_reason,
    })
}

fn sync_window_geometry(ui: &CaptureWindow, controller: &Controller, bounds: PhysicalRect) {
    let size = slint::PhysicalSize::new(bounds.size.width, bounds.size.height);
    if ui.window().size() != size {
        let started = Instant::now();
        ui.window().set_size(size);
        controller.log.event(
            "window.set_size",
            format!(
                "width={} height={} duration_ms={:.3}",
                size.width,
                size.height,
                started.elapsed().as_secs_f64() * 1000.0
            ),
        );
    }
    let position = slint::PhysicalPosition::new(bounds.origin.x, bounds.origin.y);
    if ui.window().position() != position {
        let started = Instant::now();
        ui.window().set_position(position);
        controller.log.event(
            "window.set_position",
            format!(
                "x={} y={} duration_ms={:.3}",
                position.x,
                position.y,
                started.elapsed().as_secs_f64() * 1000.0
            ),
        );
    }
}

fn image_from_frame(frame: &CapturedFrame) -> Image {
    image_from_rgba(frame.width, frame.height, &frame.pixels)
}

fn image_from_rgba(width: u32, height: u32, pixels: &[u8]) -> Image {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let target = buffer.make_mut_slice();
    for (pixel, rgba) in target.iter_mut().zip(pixels.chunks_exact(4)) {
        *pixel = Rgba8Pixel {
            r: rgba[0],
            g: rgba[1],
            b: rgba[2],
            a: rgba[3],
        };
    }
    Image::from_rgba8(buffer)
}

fn copy_document_to_clipboard(
    document: &capture_annotation::CaptureDocument,
) -> Result<(), String> {
    let rendered = flatten(document).map_err(|error| error.to_string())?;
    let mut clipboard = Clipboard::new().map_err(|error| error.to_string())?;
    clipboard
        .set_image(ImageData {
            width: rendered.width as usize,
            height: rendered.height as usize,
            bytes: std::borrow::Cow::Owned(rendered.pixels),
        })
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use capture_core::capture::PixelFormat;

    #[test]
    fn editing_input_origin_stays_on_full_frame_overlay() {
        let frame = CapturedFrame::new(
            Arc::<[u8]>::from(vec![0; 400 * 300 * 4]),
            400,
            300,
            400 * 4,
            PhysicalPoint::new(-2560, 0),
            PixelFormat::Rgba8,
        );
        let mut session = CaptureSession::new();
        session.apply(CaptureCommand::Begin);
        session.apply(CaptureCommand::FrameReady(frame));
        session.apply(CaptureCommand::BeginFreeSelection(PhysicalPoint::new(
            -2300, 100,
        )));
        session.apply(CaptureCommand::UpdateFreeSelection(PhysicalPoint::new(
            -2100, 240,
        )));
        session.apply(CaptureCommand::CommitSelection);

        assert_eq!(
            input_origin(session.state(), PhysicalPoint::new(-2560, 0)),
            PhysicalPoint::new(-2560, 0)
        );
    }
}
