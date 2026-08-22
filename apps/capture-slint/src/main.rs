#![allow(clippy::type_complexity)]

use arboard::{Clipboard, ImageData};
use capture_actions::{ActionOutcome, CaptureAction, CopyAction, PinAction, SaveAction};
use capture_annotation::{
    Annotation, AnnotationTool, CaptureCommand, CaptureEvent, CaptureSession, CaptureSessionState,
};
use capture_core::capture::CapturedFrame;
use capture_core::geometry::{PhysicalPoint, PhysicalRect};
use capture_core::selection::SelectionInteraction;
use capture_platform_api::{CaptureBackend, SnapBackend};
use capture_render::flatten;
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
    let (frame, scale_factor, status) = if let Some(ordinal) = ordinal {
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

    let ui_started = Instant::now();
    let ui = CaptureWindow::new()?;
    log.duration("startup.ui_created", ui_started);
    ui.window()
        .set_size(slint::PhysicalSize::new(frame.width, frame.height));
    let state = Rc::new(RefCell::new(Controller {
        host,
        session: CaptureSession::new(),
        frame: frame.clone(),
        log: log.clone(),
        scale_factor,
        status,
        pin_window: None,
        last_snap_at: None,
        last_visual_at: None,
        pointer_moves: 0,
        last_move_log_at: None,
    }));

    {
        let mut controller = state.borrow_mut();
        let initial_refresh_started = Instant::now();
        controller.session.apply(CaptureCommand::Begin);
        controller
            .session
            .apply(CaptureCommand::FrameReady((*frame).clone()));
        refresh_ui(&ui, &controller);
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
            let command = match controller.session.state() {
                CaptureSessionState::Selecting(selection)
                    if selection.interaction == SelectionInteraction::Hovering
                        && controller.session.hover_candidate().is_some() =>
                {
                    CaptureCommand::CommitSelection
                }
                CaptureSessionState::Selecting(_) => CaptureCommand::BeginFreeSelection(point),
                CaptureSessionState::Editing(_) => CaptureCommand::BeginAnnotation(point),
                _ => return,
            };
            let command_label = format!("{command:?}");
            let refresh_kind = match &command {
                CaptureCommand::BeginFreeSelection(_) => "selection",
                CaptureCommand::BeginAnnotation(_) => "annotation",
                _ => "full",
            };
            let started = Instant::now();
            controller.log.event(
                "input.down.begin",
                format!(
                    "x={x:.1} y={y:.1} physical=({}, {}) command={command_label}",
                    point.x, point.y
                ),
            );
            controller.apply(command);
            controller.log.duration("input.down.apply", started);
            let refresh_started = Instant::now();
            match refresh_kind {
                "selection" => refresh_selection_geometry(&ui, &controller),
                "annotation" => {
                    refresh_selection_geometry(&ui, &controller);
                    refresh_editor_overlay(&ui, &controller);
                }
                _ => refresh_ui(&ui, &controller),
            }
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
            controller.pointer_moves = controller.pointer_moves.saturating_add(1);
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
                controller.apply(CaptureCommand::UpdateFreeSelection(point));
            } else if matches!(controller.session.state(), CaptureSessionState::Editing(_)) {
                controller.apply(CaptureCommand::UpdateAnnotation(point));
                if controller.should_render_visuals() {
                    let refresh_started = Instant::now();
                    refresh_editor_overlay(&ui, &controller);
                    controller
                        .log
                        .duration("visual.annotation", refresh_started);
                }
            }
            if matches!(
                controller.session.state(),
                CaptureSessionState::Selecting(_)
            ) && controller.should_render_visuals()
            {
                let refresh_started = Instant::now();
                refresh_selection_geometry(&ui, &controller);
                controller.log.duration("visual.selection", refresh_started);
            }
            if controller
                .last_move_log_at
                .map_or(true, |last| last.elapsed() >= Duration::from_millis(500))
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
        ui.on_pointer_up(move |_x, _y| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let mut controller = state.borrow_mut();
            let started = Instant::now();
            let was_selecting = matches!(
                controller.session.state(),
                CaptureSessionState::Selecting(_)
            );
            match controller.session.state() {
                CaptureSessionState::Selecting(_) => {
                    controller.apply(CaptureCommand::CommitSelection)
                }
                CaptureSessionState::Editing(_) => controller.apply(CaptureCommand::EndAnnotation),
                _ => return,
            };
            if was_selecting {
                refresh_editor_ui(&ui, &controller, true);
            } else {
                refresh_editor_ui(&ui, &controller, false);
            }
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
                "rectangle" => AnnotationTool::Rectangle,
                _ => AnnotationTool::Pen,
            };
            controller.apply(CaptureCommand::SelectTool(tool));
            refresh_editor_ui(&ui, &controller, false);
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
        controller.scale_factor = ui.window().scale_factor() as f64;
        sync_window_geometry(&ui, &controller, controller.frame.bounds());
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
    slint::run_event_loop()?;
    log.event("shutdown.event_loop.end", "true");
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

fn input_origin(state: &CaptureSessionState, frame_origin: PhysicalPoint) -> PhysicalPoint {
    match state {
        CaptureSessionState::Editing(editor) => editor.document.crop.origin,
        CaptureSessionState::Idle
        | CaptureSessionState::Preparing
        | CaptureSessionState::Selecting(_) => frame_origin,
    }
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
        match action {
            "undo" => self.apply(CaptureCommand::Undo),
            "cancel" => {
                self.apply(CaptureCommand::Cancel);
                let _ = ui.hide();
                let _ = slint::quit_event_loop();
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
            refresh_editor_ui(ui, controller, true);
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

fn refresh_editor_ui(ui: &CaptureWindow, controller: &Controller, refresh_base: bool) {
    let started = Instant::now();
    let CaptureSessionState::Editing(editor) = controller.session.state() else {
        refresh_ui(ui, controller);
        return;
    };
    if refresh_base {
        refresh_editor_base(ui, controller);
    }
    sync_window_geometry(ui, controller, editor.document.crop);
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
    let CaptureSessionState::Editing(editor) = controller.session.state() else {
        return;
    };
    let flatten_started = Instant::now();
    let image = match flatten(&editor.document) {
        Ok(rendered) => {
            controller.log.duration("render.flatten", flatten_started);
            let image_started = Instant::now();
            let image = image_from_rgba(rendered.width, rendered.height, &rendered.pixels);
            controller
                .log
                .duration("render.editor_image", image_started);
            image
        }
        Err(error) => {
            controller.log.event("render.flatten.error", error);
            let image_started = Instant::now();
            let image = image_from_frame(&controller.frame);
            controller.log.duration("render.frame_image", image_started);
            image
        }
    };
    ui.set_frame_image(image);
}

fn refresh_editor_overlay(ui: &CaptureWindow, controller: &Controller) {
    let (path, width, visible) = match controller.session.state() {
        CaptureSessionState::Editing(editor) => {
            let mut path = String::new();
            let mut width = 0.0;
            let scale = controller.scale_factor as f32;
            for annotation in &editor.document.annotations {
                let (annotation_path, annotation_width) =
                    annotation_path(annotation, editor.document.crop, scale);
                path.push_str(&annotation_path);
                width = annotation_width;
            }
            if let Some(annotation) = editor.active_preview() {
                let (annotation_path, annotation_width) =
                    annotation_path(&annotation, editor.document.crop, scale);
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
    let (rect, editing, tool) = match controller.session.state() {
        CaptureSessionState::Selecting(selection) => (selection.rect, false, "pen"),
        CaptureSessionState::Editing(editor) => (
            PhysicalRect::new(PhysicalPoint::ZERO, editor.document.crop.size),
            true,
            editor.selected_tool.id(),
        ),
        CaptureSessionState::Idle | CaptureSessionState::Preparing => {
            (PhysicalRect::default(), false, "pen")
        }
    };
    let origin = if editing {
        PhysicalPoint::ZERO
    } else {
        controller.frame.origin
    };
    let scale = controller.scale_factor as f32;
    ui.set_selection_x((rect.origin.x - origin.x) as f32 / scale);
    ui.set_selection_y((rect.origin.y - origin.y) as f32 / scale);
    ui.set_selection_width(rect.size.width as f32 / scale);
    ui.set_selection_height(rect.size.height as f32 / scale);
    ui.set_selecting(!editing);
    ui.set_editing(editing);
    ui.set_active_tool(tool.into());
    ui.set_status(controller.status.clone().into());
}

fn annotation_path(annotation: &Annotation, crop: PhysicalRect, scale: f32) -> (String, f32) {
    let local = |point: PhysicalPoint| {
        (
            (point.x - crop.origin.x) as f32 / scale,
            (point.y - crop.origin.y) as f32 / scale,
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

fn sync_window_geometry(ui: &CaptureWindow, _controller: &Controller, bounds: PhysicalRect) {
    let size = slint::PhysicalSize::new(bounds.size.width, bounds.size.height);
    if ui.window().size() != size {
        ui.window().set_size(size);
    }
    let position = slint::PhysicalPosition::new(bounds.origin.x, bounds.origin.y);
    if ui.window().position() != position {
        ui.window().set_position(position);
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
    fn editing_input_origin_follows_crop_origin() {
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
            PhysicalPoint::new(-2360, 100)
        );
    }
}
