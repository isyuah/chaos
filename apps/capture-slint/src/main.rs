#![allow(clippy::type_complexity)]

use arboard::{Clipboard, ImageData};
use capture_actions::{ActionOutcome, CaptureAction, CopyAction, PinAction, SaveAction};
use capture_annotation::{
    AnnotationTool, CaptureCommand, CaptureEvent, CaptureSession, CaptureSessionState,
};
use capture_core::capture::CapturedFrame;
use capture_core::geometry::{PhysicalPoint, PhysicalRect};
use capture_core::selection::SelectionInteraction;
use capture_platform_api::{CaptureBackend, SnapBackend};
use capture_render::flatten;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    scale_factor: f64,
    status: String,
    pin_window: Option<PinWindow>,
    last_snap_at: Option<Instant>,
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
    let host = make_host()?;
    let ordinal = std::env::var("CAPTURE_MONITOR")
        .ok()
        .and_then(|value| value.parse::<usize>().ok());
    let (frame, scale_factor, status) = if let Some(ordinal) = ordinal {
        let monitors = host.capture().monitors()?;
        let monitor = monitors
            .get(ordinal)
            .ok_or_else(|| format!("monitor ordinal {ordinal} is unavailable"))?;
        (
            Arc::new(host.capture().capture_monitor(monitor.id)?.to_rgba8()?),
            monitor.scale_factor.get().max(0.1),
            format!(
                "{}  {}x{}",
                monitor.name,
                monitor.bounds.width(),
                monitor.bounds.height()
            ),
        )
    } else {
        let frame = Arc::new(host.capture().capture_virtual_desktop()?.to_rgba8()?);
        (
            frame.clone(),
            1.0,
            format!("Virtual desktop  {}x{}", frame.width, frame.height),
        )
    };

    let ui = CaptureWindow::new()?;
    ui.window()
        .set_size(slint::PhysicalSize::new(frame.width, frame.height));
    let state = Rc::new(RefCell::new(Controller {
        host,
        session: CaptureSession::new(),
        frame: frame.clone(),
        scale_factor,
        status,
        pin_window: None,
        last_snap_at: None,
    }));

    {
        let mut controller = state.borrow_mut();
        controller.session.apply(CaptureCommand::Begin);
        controller
            .session
            .apply(CaptureCommand::FrameReady((*frame).clone()));
        refresh_ui(&ui, &controller);
    }

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
            controller.apply(command);
            refresh_ui(&ui, &controller);
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
                    match controller.host.snap().candidates_at(point) {
                        Ok(candidates) => controller
                            .apply(CaptureCommand::SnapCandidate(candidates.into_iter().next())),
                        Err(error) => controller.status = format!("Snap unavailable: {error}"),
                    }
                }
                controller.apply(CaptureCommand::UpdateFreeSelection(point));
            } else if matches!(controller.session.state(), CaptureSessionState::Editing(_)) {
                controller.apply(CaptureCommand::UpdateAnnotation(point));
                refresh_editor_image(&ui, &controller);
            }
            refresh_selection_ui(&ui, &controller);
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
            match controller.session.state() {
                CaptureSessionState::Selecting(_) => {
                    controller.apply(CaptureCommand::CommitSelection)
                }
                CaptureSessionState::Editing(_) => controller.apply(CaptureCommand::EndAnnotation),
                _ => return,
            };
            refresh_ui(&ui, &controller);
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
            refresh_ui(&ui, &controller);
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

    ui.show()?;
    {
        let mut controller = state.borrow_mut();
        controller.scale_factor = ui.window().scale_factor() as f64;
        sync_window_geometry(&ui, &controller, controller.frame.bounds());
    }
    slint::run_event_loop()?;
    Ok(())
}

impl Controller {
    fn to_physical(&self, x: f32, y: f32) -> capture_core::PhysicalPoint {
        let frame = self.frame.as_ref();
        let origin = if matches!(self.session.state(), CaptureSessionState::Editing(_)) {
            PhysicalPoint::ZERO
        } else {
            frame.origin
        };
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

    fn run_action(&mut self, action: &str, ui: &CaptureWindow) {
        match action {
            "undo" => self.apply(CaptureCommand::Undo),
            "cancel" => {
                self.apply(CaptureCommand::Cancel);
                let _ = ui.hide();
                let _ = slint::quit_event_loop();
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
                                    return refresh_ui(ui, self);
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
        refresh_ui(ui, self);
    }

    fn document(&self) -> Option<capture_annotation::CaptureDocument> {
        match self.session.state() {
            CaptureSessionState::Editing(editor) => Some(editor.document.clone()),
            _ => None,
        }
    }
}

fn refresh_ui(ui: &CaptureWindow, controller: &Controller) {
    match controller.session.state() {
        CaptureSessionState::Selecting(_) => {
            ui.set_frame_image(image_from_frame(&controller.frame));
            sync_window_geometry(ui, controller, controller.frame.bounds());
        }
        CaptureSessionState::Editing(editor) => {
            refresh_editor_image(ui, controller);
            sync_window_geometry(ui, controller, editor.document.crop);
        }
        CaptureSessionState::Idle | CaptureSessionState::Preparing => {
            ui.set_frame_image(image_from_frame(&controller.frame));
            sync_window_geometry(ui, controller, controller.frame.bounds());
        }
    }
    refresh_selection_ui(ui, controller);
}

fn refresh_editor_image(ui: &CaptureWindow, controller: &Controller) {
    let CaptureSessionState::Editing(editor) = controller.session.state() else {
        return;
    };
    let mut document = editor.document.clone();
    if let Some(preview) = editor.active_preview() {
        document.annotations.push(preview);
    }
    let image = flatten(&document)
        .map(|rendered| image_from_rgba(rendered.width, rendered.height, &rendered.pixels))
        .unwrap_or_else(|_| image_from_frame(&controller.frame));
    ui.set_frame_image(image);
}

fn refresh_selection_ui(ui: &CaptureWindow, controller: &Controller) {
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
