use crate::inspector::{InspectorColorFormat, InspectorCoordinateMode};
use crate::presentation::{refresh_ui, sync_window_geometry};
use crate::trace::TraceLog;
use crate::{make_host, CaptureWindow, Controller};
use capture_core::capture::CapturedFrame;
use capture_core::MonitorInfo;
use capture_runtime::{CaptureSessionId, RuntimeCommand, RuntimeEvent};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

struct CapturedSnapshot {
    frame: Arc<CapturedFrame>,
    capture_scale_factor: f64,
    monitors: Vec<MonitorInfo>,
}

pub(super) struct CaptureFinished {
    session_id: CaptureSessionId,
    result: Result<CapturedSnapshot, String>,
}

pub(super) fn request_capture(
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
        controller.inspector_color_format = InspectorColorFormat::default();
        controller.inspector_coordinate_mode = InspectorCoordinateMode::default();
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

pub(super) fn uses_native_wayland_capture() -> bool {
    #[cfg(target_os = "linux")]
    {
        capture_linux::native_wayland_selected()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

pub(super) fn finish_capture(
    ui: &CaptureWindow,
    state: &Rc<RefCell<Controller>>,
    finished: CaptureFinished,
) {
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
