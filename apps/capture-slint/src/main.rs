#![allow(clippy::type_complexity)]

mod actions;
mod capture_flow;
mod controller;
mod desktop;
mod image;
mod inspector;
mod pin;
mod presentation;
mod settings;
mod settings_window;
mod trace;

use actions::copy_text_to_clipboard;
use capture_annotation::{AnnotationTool, CaptureCommand, CaptureSessionState};
use capture_core::capture::CapturedFrame;
use capture_core::geometry::PhysicalPoint;
use capture_core::selection::{ResizeHandle, SelectionInteraction, SelectionSession};
use capture_core::{MonitorInfo, SnapExclusionToken};
use capture_flow::{finish_capture, request_capture, uses_native_wayland_capture};
use capture_platform_api::{CaptureBackend, SnapBackend};
use capture_runtime::CaptureRuntime;
use desktop::{DesktopCommand, DesktopIntegration};
use image::placeholder_frame;
use inspector::{
    current_pixel_color_text, refresh_pixel_inspector, InspectorColorFormat,
    InspectorCoordinateMode,
};
use presentation::{
    cursor_kind_for_point, editor_layout, handle_tolerance, is_committable_snap, moved_enough,
    refresh_editor_ui, refresh_pointer_visuals, refresh_selection_geometry, state_label,
};
use settings::{AppSettings, SettingsStore};
use settings_window::{record_shortcut_status, show_settings_window};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use trace::TraceLog;

slint::include_modules!();

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointerGesture {
    Select,
    Annotate,
    Move,
    Resize(ResizeHandle),
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
    inspector_color_format: InspectorColorFormat,
    inspector_coordinate_mode: InspectorCoordinateMode,
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
        inspector_color_format: InspectorColorFormat::default(),
        inspector_coordinate_mode: InspectorCoordinateMode::default(),
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
            refresh_pointer_visuals(&ui, &controller);
            controller.log.duration("input.up.total", started);
        });
    }

    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_inspector_key_pressed(move |key, ctrl, alt, _shift, meta, repeat| {
            let Some(ui) = ui_weak.upgrade() else {
                return false;
            };
            if ctrl || alt || meta {
                return false;
            }

            let mut controller = state.borrow_mut();
            let key = key.as_str();
            let shift_pressed = [slint::platform::Key::Shift, slint::platform::Key::ShiftR]
                .into_iter()
                .any(|candidate| key == char::from(candidate).to_string());
            if shift_pressed {
                if !repeat {
                    controller.inspector_color_format = match controller.inspector_color_format {
                        InspectorColorFormat::Hex => InspectorColorFormat::Rgb,
                        InspectorColorFormat::Rgb => InspectorColorFormat::Hex,
                    };
                    refresh_pixel_inspector(&ui, &controller);
                }
                return true;
            }
            if key.eq_ignore_ascii_case("p") {
                if !repeat {
                    controller.inspector_coordinate_mode =
                        match controller.inspector_coordinate_mode {
                            InspectorCoordinateMode::Relative => InspectorCoordinateMode::Absolute,
                            InspectorCoordinateMode::Absolute => InspectorCoordinateMode::Relative,
                        };
                    refresh_pixel_inspector(&ui, &controller);
                }
                return true;
            }
            if key.eq_ignore_ascii_case("c") {
                if !repeat {
                    match current_pixel_color_text(&controller) {
                        Some(color) => match copy_text_to_clipboard(&color) {
                            Ok(()) => controller.set_status(format!("已复制颜色 {color}")),
                            Err(error) => controller.set_status(format!("复制颜色失败：{error}")),
                        },
                        None => return false,
                    }
                    refresh_selection_geometry(&ui, &controller);
                }
                return true;
            }
            false
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::next_capture_path;
    use crate::inspector::{place_selection_info, sample_frame_pixel};
    use crate::pin::{next_pin_zoom, pin_size_for_zoom};
    use crate::presentation::input_origin;
    use crate::settings_window::shortcut_from_key_event;
    use capture_core::capture::PixelFormat;
    use capture_core::geometry::{PhysicalRect, PhysicalSize};
    use capture_runtime::{RuntimeCommand, RuntimeEvent};
    use std::fs;

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

    #[test]
    fn pixel_sampling_respects_virtual_origin_and_row_stride() {
        let frame = CapturedFrame::new(
            Arc::<[u8]>::from([
                1, 2, 3, 255, 4, 5, 6, 255, 90, 91, 92, 93, 7, 8, 9, 255, 10, 11, 12, 255, 94, 95,
                96, 97,
            ]),
            2,
            2,
            12,
            PhysicalPoint::new(-4, 7),
            PixelFormat::Rgba8,
        );

        assert_eq!(
            sample_frame_pixel(&frame, PhysicalPoint::new(-3, 8)),
            Some([10, 11, 12, 255])
        );
        assert_eq!(sample_frame_pixel(&frame, PhysicalPoint::new(-2, 8)), None);
    }

    #[test]
    fn pin_zoom_preserves_aspect_ratio_and_caps_large_images() {
        let base = PhysicalSize::new(4_000, 2_000);
        let zoomed = next_pin_zoom(1.0, 120.0, base);
        let size = pin_size_for_zoom(base, zoomed);

        assert!(zoomed > 1.0);
        assert_eq!(size.width, size.height * 2);

        let capped = next_pin_zoom(8.0, 120.0, PhysicalSize::new(8_000, 4_000));
        let capped_size = pin_size_for_zoom(PhysicalSize::new(8_000, 4_000), capped);
        assert!(capped_size.width <= 16_384);
        assert!(capped_size.height <= 16_384);
    }

    #[test]
    fn selection_info_prefers_outside_and_avoids_screen_edges() {
        let frame = PhysicalRect::new(PhysicalPoint::ZERO, PhysicalSize::new(1_000, 800));
        let near_right = PhysicalRect::new(PhysicalPoint::new(950, 100), PhysicalSize::new(40, 40));
        let fills_height =
            PhysicalRect::new(PhysicalPoint::new(20, 0), PhysicalSize::new(300, 800));

        assert_eq!(
            place_selection_info(near_right, frame, 1.0),
            PhysicalPoint::new(770, 62)
        );
        assert_eq!(
            place_selection_info(fills_height, frame, 1.0),
            PhysicalPoint::new(28, 8)
        );
    }
}
