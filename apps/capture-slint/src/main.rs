#![allow(clippy::type_complexity)]

mod desktop;
mod settings;

use arboard::{Clipboard, ImageData};
use capture_actions::{ActionOutcome, CaptureAction, CopyAction, PinAction, SaveAction};
use capture_annotation::{
    Annotation, AnnotationTool, CaptureCommand, CaptureEvent, CaptureSessionState,
};
use capture_core::capture::{CapturedFrame, PixelFormat};
use capture_core::geometry::{PhysicalPoint, PhysicalRect, PhysicalSize};
use capture_core::selection::{ResizeHandle, SelectionInteraction, SelectionSession};
use capture_core::{
    place_toolbar, ActionId, MonitorInfo, SnapCandidate, SnapExclusionToken, SnapKind,
    ToolbarPlacementReason,
};
use capture_platform_api::{CaptureBackend, SnapBackend};
use capture_render::flatten;
use capture_runtime::{
    ActionCompletion, ActionRequestId, CaptureRuntime, CaptureSessionId, RuntimeCommand,
    RuntimeError, RuntimeEvent,
};
use desktop::{DesktopCommand, DesktopIntegration, ShortcutApply};
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use settings::{AppSettings, SettingsStore, Shortcut, ShortcutParseError};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::cell::RefCell;
use std::fmt::Display;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};
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

#[derive(Debug, Clone)]
struct AppConfig {
    settings: AppSettings,
    settings_warning: Option<String>,
    capture_monitor: Option<usize>,
    ui_scale_factor: Option<f64>,
    capture_on_startup: bool,
}

impl AppConfig {
    fn new(settings: AppSettings, settings_warning: Option<String>) -> Self {
        Self {
            settings,
            settings_warning,
            capture_monitor: std::env::var("CAPTURE_MONITOR")
                .ok()
                .and_then(|value| value.parse::<usize>().ok()),
            ui_scale_factor: std::env::var("SLINT_SCALE_FACTOR")
                .ok()
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| *value > 0.1),
            capture_on_startup: std::env::var("CAPTURE_ON_STARTUP")
                .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true")),
        }
    }
}

struct CapturedSnapshot {
    frame: Arc<CapturedFrame>,
    capture_scale_factor: f64,
    monitors: Vec<MonitorInfo>,
}

struct CaptureFinished {
    session_id: CaptureSessionId,
    result: Result<CapturedSnapshot, String>,
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
    config: AppConfig,
    runtime: CaptureRuntime,
    frame: Arc<CapturedFrame>,
    log: TraceLog,
    scale_factor: f64,
    status: String,
    status_revision: i32,
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
    let ui_started = Instant::now();
    let ui = CaptureWindow::new()?;
    log.duration("startup.ui_created", ui_started);
    let settings_store = Rc::new(SettingsStore::discover()?);
    let loaded_settings = settings_store.load_or_default();
    if let Some(warning) = &loaded_settings.warning {
        log.event(
            "settings.load.warning",
            format!("path={} error={warning}", settings_store.path().display()),
        );
    }
    let config = AppConfig::new(loaded_settings.settings, loaded_settings.warning);
    if let Some(ui_scale_factor) = config.ui_scale_factor {
        log.event(
            "startup.ui_scale_hint",
            format!("scale_factor={ui_scale_factor:.3}"),
        );
    }
    let frame = placeholder_frame();
    let state = Rc::new(RefCell::new(Controller {
        host,
        runtime: CaptureRuntime::new(config.settings.runtime_policy()),
        config,
        frame: frame.clone(),
        log: log.clone(),
        scale_factor: 1.0,
        status: String::new(),
        status_revision: 0,
        pin_window: None,
        last_snap_at: None,
        last_visual_at: None,
        pointer_moves: 0,
        last_move_log_at: None,
        monitors: Vec::new(),
        pointer_gesture: None,
        last_pointer: None,
        toolbar_override: None,
        toolbar_drag_offset: None,
        overlay_token: None,
        last_toolbar_log_at: std::cell::Cell::new(None),
    }));
    ui.set_canvas_cursor_kind("default".into());
    ui.set_toolbar_cursor_kind("grab".into());

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
                    state_label(controller.runtime.state())
                ),
            );

            controller.last_pointer = Some(point);
            ui.set_canvas_cursor_kind(cursor_kind_for_point(&controller, point).into());
            match controller.runtime.state() {
                CaptureSessionState::Selecting(selection)
                    if selection.interaction == SelectionInteraction::Hovering
                        && controller
                            .runtime
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
                controller.runtime.state(),
                CaptureSessionState::Selecting(selection)
                    if selection.interaction == SelectionInteraction::Dragging
            );
            if matches!(
                controller.runtime.state(),
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
                            controller.set_status(format!("无法吸附：{error}"));
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
            } else if matches!(controller.runtime.state(), CaptureSessionState::Editing(_)) {
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
                        state_label(controller.runtime.state()),
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
            match controller.runtime.state() {
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

    let (desktop_sender, desktop_receiver) = mpsc::channel();
    let (capture_sender, capture_receiver) = mpsc::channel();
    let desktop_integration = Rc::new(RefCell::new(None::<DesktopIntegration>));
    let settings_window = Rc::new(RefCell::new(None::<SettingsWindow>));

    {
        let integration = desktop_integration.clone();
        let desktop_sender = desktop_sender.clone();
        let log = log.clone();
        let (capture_on_startup, shortcut) = {
            let controller = state.borrow();
            (
                controller.config.capture_on_startup,
                controller.config.settings.shortcut.clone(),
            )
        };
        let native_wayland = uses_native_wayland_capture();
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let settings_store = settings_store.clone();
        let settings_window = settings_window.clone();
        slint::Timer::single_shot(Duration::ZERO, move || {
            let startup = desktop::initialize(desktop_sender.clone(), native_wayland, &shortcut);
            for message in startup.messages {
                log.event("desktop.initialize.warning", message);
            }
            let shortcut_error = startup.shortcut_error;
            *integration.borrow_mut() = Some(startup.integration);
            record_shortcut_status(
                &state,
                &settings_store,
                &settings_window,
                shortcut.to_string(),
                shortcut_error,
            );
            if let Some(ui) = ui_weak.upgrade() {
                let warmup_started = Instant::now();
                ui.set_warmup(true);
                ui.window().set_size(slint::PhysicalSize::new(1, 1));
                ui.window()
                    .set_position(slint::PhysicalPosition::new(-32_000, -32_000));
                match ui.show().and_then(|()| ui.hide()) {
                    Ok(()) => log.duration("startup.renderer_warmup", warmup_started),
                    Err(error) => {
                        log.event("startup.renderer_warmup.error", format!("error={error}"))
                    }
                }
                ui.set_warmup(false);
            }
            log.event(
                "desktop.initialize.ready",
                format!("shortcut={shortcut} tray=true"),
            );
            if capture_on_startup {
                let _ = desktop_sender.send(DesktopCommand::Capture);
            }
        });
    }

    let event_timer = slint::Timer::default();
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        let capture_sender = capture_sender.clone();
        let desktop_integration = desktop_integration.clone();
        let settings_store = settings_store.clone();
        let settings_window = settings_window.clone();
        event_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(8),
            move || {
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                while let Ok(command) = desktop_receiver.try_recv() {
                    match command {
                        DesktopCommand::Capture => {
                            if let Some(settings) = settings_window.borrow().as_ref() {
                                let _ = settings.hide();
                            }
                            request_capture(&ui, &state, capture_sender.clone())
                        }
                        DesktopCommand::ShortcutCapture => {
                            let settings_visible = settings_window
                                .borrow()
                                .as_ref()
                                .is_some_and(|settings| settings.window().is_visible());
                            if settings_visible {
                                state
                                    .borrow()
                                    .log
                                    .event("desktop.hotkey.ignored", "reason=settings_visible");
                            } else {
                                request_capture(&ui, &state, capture_sender.clone());
                            }
                        }
                        DesktopCommand::Settings => show_settings_window(
                            &ui,
                            &state,
                            &desktop_integration,
                            &settings_store,
                            &settings_window,
                        ),
                        DesktopCommand::Quit => {
                            let _ = ui.hide();
                            if let Some(settings) = settings_window.borrow().as_ref() {
                                let _ = settings.hide();
                            }
                            let _ = slint::quit_event_loop();
                        }
                        #[cfg(target_os = "linux")]
                        DesktopCommand::ShortcutStatus { shortcut, error } => {
                            record_shortcut_status(
                                &state,
                                &settings_store,
                                &settings_window,
                                shortcut,
                                error,
                            );
                        }
                    }
                }
                while let Ok(finished) = capture_receiver.try_recv() {
                    finish_capture(&ui, &state, finished);
                }
            },
        );
    }

    log.event("startup.resident_ready", "overlay=hidden");
    log.event("startup.event_loop.begin", "true");
    let event_loop_started = Instant::now();
    slint::run_event_loop_until_quit()?;
    log.event("shutdown.event_loop.end", "true");
    log.duration("shutdown.event_loop.total", event_loop_started);
    let cleanup_started = Instant::now();
    drop(event_timer);
    drop(settings_window);
    drop(desktop_integration);
    drop(ui);
    drop(state);
    log.duration("shutdown.resources_drop", cleanup_started);
    log.flush();
    Ok(())
}

fn show_settings_window(
    overlay: &CaptureWindow,
    state: &Rc<RefCell<Controller>>,
    desktop_integration: &Rc<RefCell<Option<DesktopIntegration>>>,
    settings_store: &Rc<SettingsStore>,
    settings_window: &Rc<RefCell<Option<SettingsWindow>>>,
) {
    let _ = overlay.hide();
    {
        let mut controller = state.borrow_mut();
        if !matches!(controller.runtime.state(), CaptureSessionState::Idle) {
            controller.apply(CaptureCommand::Cancel);
            controller.host.snap().set_excluded_windows(&[]);
            controller.overlay_token = None;
            controller.frame = placeholder_frame();
            overlay.set_frame_image(image_from_frame(&controller.frame));
        }
    }

    {
        if let Some(previous) = settings_window.borrow_mut().take() {
            let _ = previous.hide();
        }
        let settings = match SettingsWindow::new() {
            Ok(settings) => settings,
            Err(error) => {
                state
                    .borrow()
                    .log
                    .event("settings.window.error", format!("error={error}"));
                return;
            }
        };

        {
            let settings_weak = settings.as_weak();
            settings.on_cancel(move || {
                if let Some(settings) = settings_weak.upgrade() {
                    let _ = settings.hide();
                }
            });
        }

        {
            let settings_weak = settings.as_weak();
            settings.on_choose_directory(move || {
                let Some(settings) = settings_weak.upgrade() else {
                    return;
                };
                let current = PathBuf::from(settings.get_save_directory().as_str());
                let mut dialog = rfd::FileDialog::new().set_title("选择截图保存目录");
                if current.is_dir() {
                    dialog = dialog.set_directory(current);
                }
                if let Some(directory) = dialog.pick_folder() {
                    settings.set_save_directory(directory.to_string_lossy().into_owned().into());
                    settings.set_status("".into());
                }
            });
        }

        {
            let settings_weak = settings.as_weak();
            settings.on_record_shortcut(move |key, ctrl, alt, shift, meta| {
                let Some(settings) = settings_weak.upgrade() else {
                    return false;
                };
                match shortcut_from_key_event(key.as_str(), ctrl, alt, shift, meta) {
                    Ok(Some(shortcut)) => {
                        settings.set_shortcut(shortcut.to_string().into());
                        settings.set_status("".into());
                        settings.set_status_is_error(false);
                        true
                    }
                    Ok(None) => false,
                    Err(error) => {
                        settings.set_status(error.to_string().into());
                        settings.set_status_is_error(true);
                        false
                    }
                }
            });
        }

        {
            let settings_weak = settings.as_weak();
            let state = state.clone();
            let desktop_integration = desktop_integration.clone();
            let settings_store = settings_store.clone();
            settings.on_save_settings(move |shortcut, save_directory, close_after_copy| {
                let Some(settings_window) = settings_weak.upgrade() else {
                    return;
                };
                let shortcut = match shortcut.as_str().parse::<Shortcut>() {
                    Ok(shortcut) => shortcut,
                    Err(error) => {
                        settings_window.set_status(error.to_string().into());
                        settings_window.set_status_is_error(true);
                        return;
                    }
                };
                let save_directory = match prepare_save_directory(save_directory.as_str()) {
                    Ok(directory) => directory,
                    Err(error) => {
                        settings_window.set_status(error.into());
                        settings_window.set_status_is_error(true);
                        return;
                    }
                };

                let previous = state.borrow().config.settings.clone();
                let mut candidate = previous.clone();
                candidate.shortcut = shortcut;
                candidate.save_directory = save_directory;
                candidate.copy_disposition = if close_after_copy {
                    capture_runtime::CopyDisposition::CloseOverlay
                } else {
                    capture_runtime::CopyDisposition::KeepEditorOpen
                };
                let should_apply_shortcut =
                    candidate.shortcut != previous.shortcut || previous.shortcut_error.is_some();
                if should_apply_shortcut {
                    candidate.shortcut_error = None;
                }

                if let Err(error) = settings_store.save(&candidate) {
                    settings_window.set_status(error.to_string().into());
                    settings_window.set_status_is_error(true);
                    return;
                }

                let shortcut_apply = if should_apply_shortcut {
                    match desktop_integration.borrow_mut().as_mut() {
                        Some(integration) => integration.set_shortcut(&candidate.shortcut),
                        None => Err("桌面集成尚未初始化".to_string()),
                    }
                } else {
                    Ok(ShortcutApply::Active)
                };
                let shortcut_apply = match shortcut_apply {
                    Ok(result) => result,
                    Err(error) => {
                        let rollback = settings_store.save(&previous).err();
                        let message = match rollback {
                            Some(rollback) => {
                                format!("快捷键未生效：{error}；同时无法恢复原设置文件：{rollback}")
                            }
                            None => format!("快捷键未生效：{error}"),
                        };
                        settings_window.set_status(message.into());
                        settings_window.set_status_is_error(true);
                        return;
                    }
                };

                {
                    let mut controller = state.borrow_mut();
                    controller.config.settings = candidate.clone();
                    controller.config.settings_warning = None;
                    controller
                        .dispatch_runtime(RuntimeCommand::SetPolicy(candidate.runtime_policy()));
                    controller.log.event(
                        "settings.saved",
                        format!(
                            "path={} shortcut={} save_directory={} copy_disposition={:?}",
                            settings_store.path().display(),
                            candidate.shortcut,
                            candidate.save_directory.display(),
                            candidate.copy_disposition
                        ),
                    );
                }
                settings_window.set_shortcut(candidate.shortcut.to_string().into());
                settings_window.set_save_directory(
                    candidate
                        .save_directory
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                );
                settings_window.set_status_is_error(false);
                settings_window.set_status(
                    match shortcut_apply {
                        ShortcutApply::Active => "设置已保存",
                        #[cfg(target_os = "linux")]
                        ShortcutApply::AwaitingPortal => "设置已保存，等待桌面授权快捷键",
                    }
                    .into(),
                );
            });
        }

        *settings_window.borrow_mut() = Some(settings);
    }

    let (current, settings_warning) = {
        let controller = state.borrow();
        (
            controller.config.settings.clone(),
            controller.config.settings_warning.clone(),
        )
    };
    if let Some(settings) = settings_window.borrow().as_ref() {
        settings.set_shortcut(current.shortcut.to_string().into());
        settings.set_save_directory(current.save_directory.to_string_lossy().into_owned().into());
        settings.set_close_after_copy(
            current.copy_disposition == capture_runtime::CopyDisposition::CloseOverlay,
        );
        let diagnostic = current.shortcut_error.or(settings_warning);
        settings.set_status_is_error(diagnostic.is_some());
        settings.set_status(diagnostic.unwrap_or_default().into());
        if let Err(error) = settings.show() {
            state
                .borrow()
                .log
                .event("settings.window.show.error", format!("error={error}"));
        }
    }
}

fn shortcut_from_key_event(
    key_text: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
) -> Result<Option<Shortcut>, ShortcutParseError> {
    use slint::platform::Key;

    let mut characters = key_text.chars();
    let Some(key) = characters.next() else {
        return Err(ShortcutParseError::UnsupportedKey(key_text.to_string()));
    };
    if characters.next().is_some() {
        return Err(ShortcutParseError::UnsupportedKey(key_text.to_string()));
    }

    if [
        Key::Control,
        Key::ControlR,
        Key::Alt,
        Key::AltGr,
        Key::Shift,
        Key::ShiftR,
        Key::Meta,
        Key::MetaR,
    ]
    .into_iter()
    .any(|modifier| char::from(modifier) == key)
    {
        return Ok(None);
    }

    let key_name = if key.is_ascii_alphanumeric() {
        key.to_ascii_uppercase().to_string()
    } else {
        let first_function_key = char::from(Key::F1) as u32;
        let key_code = key as u32;
        if (first_function_key..=char::from(Key::F24) as u32).contains(&key_code) {
            format!("F{}", key_code - first_function_key + 1)
        } else if key == char::from(Key::SysReq) {
            "PrintScreen".to_string()
        } else {
            return Err(ShortcutParseError::UnsupportedKey(key_text.to_string()));
        }
    };

    let mut parts = Vec::with_capacity(5);
    if ctrl {
        parts.push("Ctrl");
    }
    if alt {
        parts.push("Alt");
    }
    if shift {
        parts.push("Shift");
    }
    if meta {
        parts.push("Win");
    }
    parts.push(&key_name);
    parts.join("+").parse().map(Some)
}

fn record_shortcut_status(
    state: &Rc<RefCell<Controller>>,
    settings_store: &SettingsStore,
    settings_window: &Rc<RefCell<Option<SettingsWindow>>>,
    shortcut: String,
    error: Option<String>,
) {
    let (updated, log) = {
        let mut controller = state.borrow_mut();
        if controller.config.settings.shortcut.to_string() != shortcut {
            controller.log.event(
                "desktop.hotkey.status.ignored",
                format!("shortcut={shortcut} reason=stale"),
            );
            return;
        }
        let changed = controller.config.settings.shortcut_error != error;
        controller.config.settings.shortcut_error = error.clone();
        (
            changed.then(|| controller.config.settings.clone()),
            controller.log.clone(),
        )
    };

    if let Some(updated) = updated {
        if let Err(save_error) = settings_store.save(&updated) {
            log.event(
                "settings.shortcut_status.persist.error",
                format!("error={save_error}"),
            );
        }
    }
    log.event(
        "desktop.hotkey.status",
        format!(
            "shortcut={shortcut} active={} error={}",
            error.is_none(),
            error.as_deref().unwrap_or("<none>")
        ),
    );
    if let Some(settings) = settings_window.borrow().as_ref() {
        settings.set_status_is_error(error.is_some());
        settings.set_status(error.unwrap_or_else(|| "快捷键已生效".to_string()).into());
    }
}

fn prepare_save_directory(value: &str) -> Result<PathBuf, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("保存目录不能为空".to_string());
    }
    let mut directory = PathBuf::from(value);
    if directory.is_relative() {
        directory = std::env::current_dir()
            .map_err(|error| format!("无法解析相对保存目录：{error}"))?
            .join(directory);
    }
    fs::create_dir_all(&directory)
        .map_err(|error| format!("无法创建保存目录 {}：{error}", directory.display()))?;
    let metadata = fs::metadata(&directory)
        .map_err(|error| format!("无法访问保存目录 {}：{error}", directory.display()))?;
    if !metadata.is_dir() {
        return Err(format!("保存路径不是目录：{}", directory.display()));
    }
    directory
        .canonicalize()
        .map_err(|error| format!("无法规范化保存目录 {}：{error}", directory.display()))
}

fn next_capture_path(directory: &std::path::Path) -> Result<PathBuf, String> {
    fs::create_dir_all(directory)
        .map_err(|error| format!("无法创建保存目录 {}：{error}", directory.display()))?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("系统时间无效：{error}"))?
        .as_millis();
    for sequence in 0..10_000u32 {
        let name = if sequence == 0 {
            format!("capture-{timestamp}.png")
        } else {
            format!("capture-{timestamp}-{sequence}.png")
        };
        let path = directory.join(name);
        if !path.exists() {
            return Ok(path);
        }
    }
    Err("无法生成不重复的截图文件名".to_string())
}

fn request_capture(
    ui: &CaptureWindow,
    state: &Rc<RefCell<Controller>>,
    sender: mpsc::Sender<CaptureFinished>,
) {
    let request_started = Instant::now();
    let _ = ui.hide();
    let (session_id, monitor_ordinal, log) = {
        let mut controller = state.borrow_mut();
        controller.host.snap().set_excluded_windows(&[]);
        controller.overlay_token = None;
        controller.pointer_gesture = None;
        controller.last_pointer = None;
        controller.toolbar_override = None;
        controller.toolbar_drag_offset = None;
        controller.last_snap_at = None;
        controller.last_visual_at = None;
        controller.set_status("正在截图…");
        let events = controller.dispatch_runtime(RuntimeCommand::BeginCapture);
        let Some(session_id) = events.into_iter().find_map(|event| match event {
            RuntimeEvent::CaptureRequested { session_id } => Some(session_id),
            _ => None,
        }) else {
            controller.set_status("无法创建截图会话");
            return;
        };
        controller.log.event(
            "capture.requested",
            format!(
                "session={} monitor={}",
                session_id.get(),
                controller
                    .config
                    .capture_monitor
                    .map_or_else(|| "virtual".into(), |ordinal| ordinal.to_string())
            ),
        );
        (
            session_id,
            controller.config.capture_monitor,
            controller.log.clone(),
        )
    };

    let worker_sender = sender.clone();
    let spawn_result = std::thread::Builder::new()
        .name(format!("capture-session-{}", session_id.get()))
        .spawn(move || {
            let result = capture_snapshot(monitor_ordinal, &log);
            let _ = worker_sender.send(CaptureFinished { session_id, result });
        });
    if let Err(error) = spawn_result {
        let _ = sender.send(CaptureFinished {
            session_id,
            result: Err(format!("无法启动截图任务：{error}")),
        });
    }
    state
        .borrow()
        .log
        .duration("capture.request.dispatch", request_started);
}

fn capture_snapshot(
    monitor_ordinal: Option<usize>,
    log: &TraceLog,
) -> Result<CapturedSnapshot, String> {
    let host_started = Instant::now();
    let host = make_host()?;
    log.duration("capture.host_ready", host_started);

    let monitors_started = Instant::now();
    let monitors = if monitor_ordinal.is_none() && uses_native_wayland_capture() {
        Vec::new()
    } else {
        host.capture()
            .monitors()
            .map_err(|error| error.to_string())?
    };
    log.duration("capture.monitors", monitors_started);
    for monitor in &monitors {
        log.event(
            "capture.monitor",
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

    let capture_started = Instant::now();
    let (captured, capture_scale_factor) = if let Some(ordinal) = monitor_ordinal {
        let monitor = monitors
            .get(ordinal)
            .ok_or_else(|| format!("monitor ordinal {ordinal} is unavailable"))?;
        log.event(
            "capture.monitor.selected",
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
        (
            host.capture()
                .capture_monitor(monitor.id)
                .map_err(|error| error.to_string())?,
            monitor.scale_factor.get().max(0.1),
        )
    } else {
        (
            host.capture()
                .capture_virtual_desktop()
                .map_err(|error| error.to_string())?,
            1.0,
        )
    };
    log.duration("capture.frame", capture_started);
    let rgba_started = Instant::now();
    let frame = Arc::new(captured.to_rgba8().map_err(|error| error.to_string())?);
    log.duration("capture.frame_to_rgba8", rgba_started);
    log.event(
        "capture.frame_ready",
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
    Ok(CapturedSnapshot {
        frame,
        capture_scale_factor,
        monitors,
    })
}

fn uses_native_wayland_capture() -> bool {
    #[cfg(target_os = "linux")]
    {
        capture_linux::native_wayland_selected()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn finish_capture(ui: &CaptureWindow, state: &Rc<RefCell<Controller>>, finished: CaptureFinished) {
    let finished_started = Instant::now();
    let CaptureFinished { session_id, result } = finished;
    let snapshot = match result {
        Ok(snapshot) => snapshot,
        Err(message) => {
            let mut controller = state.borrow_mut();
            let events = controller.dispatch_runtime(RuntimeCommand::FrameFailed {
                session_id,
                message: message.clone(),
            });
            if events.iter().any(|event| {
                matches!(event, RuntimeEvent::CaptureFailed { session_id: failed, .. } if *failed == session_id)
            }) {
                controller.set_status(format!("截图失败：{message}"));
                controller.log.event(
                    "capture.failed",
                    format!("session={} error={message}", session_id.get()),
                );
            }
            return;
        }
    };

    {
        let mut controller = state.borrow_mut();
        let events = controller.dispatch_runtime(RuntimeCommand::FrameReady {
            session_id,
            frame: (*snapshot.frame).clone(),
        });
        let accepted = events.iter().any(|event| {
            matches!(event, RuntimeEvent::CaptureReady { session_id: ready } if *ready == session_id)
        });
        if !accepted {
            if let Some(message) = events.iter().find_map(|event| match event {
                RuntimeEvent::CaptureFailed { message, .. } => Some(message),
                _ => None,
            }) {
                controller.log.event(
                    "capture.failed",
                    format!("session={} error={message}", session_id.get()),
                );
            } else {
                controller.log.event(
                    "capture.stale",
                    format!("session={} ignored=true", session_id.get()),
                );
            }
            return;
        }
        controller.frame = snapshot.frame;
        controller.monitors = snapshot.monitors;
        controller.scale_factor = controller
            .config
            .ui_scale_factor
            .unwrap_or(snapshot.capture_scale_factor);
        controller.status.clear();
        let bounds = controller.frame.bounds();
        sync_window_geometry(ui, &controller, bounds);
        refresh_ui(ui, &controller);
    }

    let show_started = Instant::now();
    if let Err(error) = ui.show() {
        let mut controller = state.borrow_mut();
        controller.set_status(format!("无法显示截图窗口：{error}"));
        controller.log.event(
            "capture.overlay.show.error",
            format!("session={} error={error}", session_id.get()),
        );
        return;
    }
    {
        let mut controller = state.borrow_mut();
        controller
            .log
            .duration("capture.overlay.show", show_started);
        reconcile_window_scale(ui, &mut controller, session_id, false);
        controller.sync_snap_exclusions(ui);
        controller.log.event(
            "capture.overlay.ready",
            format!(
                "session={} scale_factor={:.3} position=({}, {}) size={}x{} total_ms={:.3}",
                session_id.get(),
                controller.scale_factor,
                controller.frame.origin.x,
                controller.frame.origin.y,
                controller.frame.width,
                controller.frame.height,
                finished_started.elapsed().as_secs_f64() * 1000.0,
            ),
        );
    }

    let state = state.clone();
    let ui_weak = ui.as_weak();
    slint::Timer::single_shot(Duration::from_millis(1), move || {
        let Some(ui) = ui_weak.upgrade() else {
            return;
        };
        let mut controller = state.borrow_mut();
        if controller.runtime.active_session_id() == Some(session_id) {
            reconcile_window_scale(&ui, &mut controller, session_id, true);
        }
    });
}

fn reconcile_window_scale(
    ui: &CaptureWindow,
    controller: &mut Controller,
    session_id: CaptureSessionId,
    deferred: bool,
) {
    let actual_scale = ui.window().scale_factor() as f64;
    let previous_scale = controller.scale_factor;
    if controller.config.ui_scale_factor.is_none() {
        controller.scale_factor = actual_scale.max(0.1);
    }
    let bounds = controller.frame.bounds();
    sync_window_geometry(ui, controller, bounds);
    if (previous_scale - controller.scale_factor).abs() > 0.01 || deferred {
        refresh_ui(ui, controller);
        controller.log.event(
            "capture.scale_reconciled",
            format!(
                "session={} deferred={} previous={previous_scale:.3} ui={actual_scale:.3}",
                session_id.get(),
                deferred,
            ),
        );
    }
}

fn placeholder_frame() -> Arc<CapturedFrame> {
    Arc::new(CapturedFrame::new(
        Arc::<[u8]>::from([0, 0, 0, 0]),
        1,
        1,
        4,
        PhysicalPoint::ZERO,
        PixelFormat::Rgba8,
    ))
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
    let CaptureSessionState::Editing(editor) = controller.runtime.state() else {
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
        let origin = input_origin(self.runtime.state(), frame.origin);
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
        self.dispatch_runtime(RuntimeCommand::Capture(command));
    }

    fn dispatch_runtime(&mut self, command: RuntimeCommand) -> Vec<RuntimeEvent> {
        let events = self.runtime.dispatch(command);
        for event in &events {
            match event {
                RuntimeEvent::Session(CaptureEvent::Error(error)) => {
                    self.set_status(error.to_string())
                }
                RuntimeEvent::CaptureFailed { message, .. } => self.set_status(message.clone()),
                RuntimeEvent::StatusChanged(message) => self.set_status(message.clone()),
                RuntimeEvent::Rejected(error) => {
                    self.log.event("runtime.rejected", error);
                    if !matches!(error, RuntimeError::StaleCaptureSession { .. }) {
                        self.set_status(error.to_string());
                    }
                }
                _ => {}
            }
        }
        events
    }

    fn set_status(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_revision = self.status_revision.wrapping_add(1);
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
                state_label(self.runtime.state())
            ),
        );
        match action {
            "undo" => self.apply(CaptureCommand::Undo),
            "cancel" => {
                let apply_started = Instant::now();
                self.apply(CaptureCommand::Cancel);
                self.log.duration("overlay.cancel.apply", apply_started);
                let hide_started = Instant::now();
                match ui.hide() {
                    Ok(()) => self.log.event(
                        "overlay.cancel.hide",
                        format!(
                            "ok=true duration_ms={:.3}",
                            hide_started.elapsed().as_secs_f64() * 1000.0
                        ),
                    ),
                    Err(error) => self.log.event(
                        "overlay.cancel.hide",
                        format!(
                            "ok=false duration_ms={:.3} error={error}",
                            hide_started.elapsed().as_secs_f64() * 1000.0
                        ),
                    ),
                }
                self.host.snap().set_excluded_windows(&[]);
                self.overlay_token = None;
                self.frame = placeholder_frame();
                ui.set_frame_image(image_from_frame(&self.frame));
                return;
            }
            "copy" | "save" | "pin" | "ask-ai" => {
                let action_id = match action {
                    "copy" => ActionId::COPY,
                    "save" => ActionId::SAVE,
                    "pin" => ActionId::PIN,
                    "ask-ai" => ActionId::ASK_AI,
                    _ => unreachable!(),
                };
                let events = self.dispatch_runtime(RuntimeCommand::Capture(
                    CaptureCommand::InvokeAction(action_id),
                ));
                for event in events {
                    if let RuntimeEvent::ActionRequested { request_id, action } = event {
                        self.execute_builtin_action(request_id, action, ui);
                    }
                }
            }
            _ => {}
        }
        if matches!(self.runtime.state(), CaptureSessionState::Editing(_)) {
            refresh_editor_ui(ui, self, false);
        } else {
            refresh_ui(ui, self);
        }
    }

    fn execute_builtin_action(
        &mut self,
        request_id: ActionRequestId,
        action: ActionId,
        ui: &CaptureWindow,
    ) {
        let Some(document) = self.document() else {
            return;
        };
        match action {
            ActionId::COPY => match CopyAction.invoke(&document) {
                Ok(ActionOutcome::Png(_)) => match copy_document_to_clipboard(&document) {
                    Ok(()) => self.complete_action(request_id, true, "已复制到剪贴板", ui),
                    Err(error) => {
                        self.complete_action(request_id, false, format!("复制失败：{error}"), ui)
                    }
                },
                Ok(_) => self.complete_action(request_id, false, "复制失败：返回了未知结果", ui),
                Err(error) => {
                    self.complete_action(request_id, false, format!("复制失败：{error}"), ui)
                }
            },
            ActionId::SAVE => {
                let path = match next_capture_path(&self.config.settings.save_directory) {
                    Ok(path) => path,
                    Err(error) => {
                        self.complete_action(request_id, false, format!("保存失败：{error}"), ui);
                        return;
                    }
                };
                match SaveAction::new(&path).invoke(&document) {
                    Ok(ActionOutcome::Saved(path)) => self.complete_action(
                        request_id,
                        true,
                        format!("已保存到 {}", path.display()),
                        ui,
                    ),
                    Ok(_) => self.complete_action(request_id, true, "保存完成", ui),
                    Err(error) => {
                        self.complete_action(request_id, false, format!("保存失败：{error}"), ui)
                    }
                }
            }
            ActionId::PIN => match PinAction.invoke(&document) {
                Ok(ActionOutcome::Pin(_payload)) => match PinWindow::new() {
                    Ok(pin) => {
                        let rendered = match flatten(&document) {
                            Ok(rendered) => rendered,
                            Err(error) => {
                                self.complete_action(
                                    request_id,
                                    false,
                                    format!("固定截图渲染失败：{error}"),
                                    ui,
                                );
                                return;
                            }
                        };
                        let image =
                            image_from_rgba(rendered.width, rendered.height, &rendered.pixels);
                        pin.set_pin_image(image);
                        pin.window()
                            .set_size(slint::PhysicalSize::new(rendered.width, rendered.height));
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
                        self.complete_action(request_id, true, "截图已固定为浮动窗口", ui);
                    }
                    Err(error) => self.complete_action(
                        request_id,
                        false,
                        format!("固定窗口创建失败：{error}"),
                        ui,
                    ),
                },
                Ok(_) => self.complete_action(request_id, true, "固定完成", ui),
                Err(error) => {
                    self.complete_action(request_id, false, format!("固定失败：{error}"), ui)
                }
            },
            ActionId::ASK_AI => self.complete_action(request_id, false, "AI 功能尚未接入", ui),
            _ => {}
        }
    }

    fn complete_action(
        &mut self,
        request_id: ActionRequestId,
        success: bool,
        message: impl Into<String>,
        ui: &CaptureWindow,
    ) {
        let message = message.into();
        let completion = if success {
            ActionCompletion::succeeded(message)
        } else {
            ActionCompletion::failed(message)
        };
        let events = self.dispatch_runtime(RuntimeCommand::CompleteAction {
            request_id,
            completion,
        });
        if events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::CloseOverlay))
        {
            self.apply(CaptureCommand::Cancel);
            let _ = ui.hide();
            self.host.snap().set_excluded_windows(&[]);
            self.overlay_token = None;
            self.frame = placeholder_frame();
            ui.set_frame_image(image_from_frame(&self.frame));
        }
    }

    fn document(&self) -> Option<capture_annotation::CaptureDocument> {
        self.runtime.document()
    }
}

fn refresh_ui(ui: &CaptureWindow, controller: &Controller) {
    let started = Instant::now();
    match controller.runtime.state() {
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
                    state_label(controller.runtime.state()),
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
            state_label(controller.runtime.state()),
            started.elapsed().as_secs_f64() * 1000.0
        ),
    );
}

fn refresh_pointer_visuals(ui: &CaptureWindow, controller: &Controller) {
    match controller.runtime.state() {
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
    let CaptureSessionState::Editing(editor) = controller.runtime.state() else {
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
    let (path, width, visible) = match controller.runtime.state() {
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
    let (rect, editing, tool, window_origin, toolbar) = match controller.runtime.state() {
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
    ui.set_status_revision(controller.status_revision);
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
    let CaptureSessionState::Editing(editor) = controller.runtime.state() else {
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
        (408.0 * controller.scale_factor).round().max(1.0) as u32,
        (56.0 * controller.scale_factor).round().max(1.0) as u32,
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
    for (pixel, rgba) in target.iter_mut().zip(pixels.as_chunks::<4>().0.iter()) {
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
        let mut runtime = CaptureRuntime::default();
        let session_id = runtime
            .dispatch(RuntimeCommand::BeginCapture)
            .into_iter()
            .find_map(|event| match event {
                RuntimeEvent::CaptureRequested { session_id } => Some(session_id),
                _ => None,
            })
            .expect("capture request");
        runtime.dispatch(RuntimeCommand::FrameReady { session_id, frame });
        for command in [
            CaptureCommand::BeginFreeSelection(PhysicalPoint::new(-2300, 100)),
            CaptureCommand::UpdateFreeSelection(PhysicalPoint::new(-2100, 240)),
            CaptureCommand::CommitSelection,
        ] {
            runtime.dispatch(RuntimeCommand::Capture(command));
        }

        assert_eq!(
            input_origin(runtime.state(), PhysicalPoint::new(-2560, 0)),
            PhysicalPoint::new(-2560, 0)
        );
    }

    #[test]
    fn capture_paths_do_not_reuse_an_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let first = next_capture_path(directory.path()).unwrap();
        fs::write(&first, b"existing screenshot").unwrap();

        let second = next_capture_path(directory.path()).unwrap();

        assert_ne!(first, second);
        assert!(!second.exists());
    }

    #[test]
    fn shortcut_recorder_ignores_modifier_key_events() {
        let control = char::from(slint::platform::Key::Control).to_string();

        assert_eq!(
            shortcut_from_key_event(&control, true, false, false, false).unwrap(),
            None
        );
    }

    #[test]
    fn shortcut_recorder_canonicalizes_supported_key_events() {
        let letter = shortcut_from_key_event("s", true, false, true, false)
            .unwrap()
            .unwrap();
        let function_key = char::from(slint::platform::Key::F12).to_string();
        let function = shortcut_from_key_event(&function_key, false, true, false, false)
            .unwrap()
            .unwrap();
        let print_screen = char::from(slint::platform::Key::SysReq).to_string();
        let print_screen = shortcut_from_key_event(&print_screen, false, false, false, false)
            .unwrap()
            .unwrap();

        assert_eq!(letter.to_string(), "Ctrl+Shift+S");
        assert_eq!(function.to_string(), "Alt+F12");
        assert_eq!(print_screen.to_string(), "PrintScreen");
    }
}
