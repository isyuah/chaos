use crate::image::image_from_frame;
use crate::inspector::{format_selection_info, place_selection_info, refresh_pixel_inspector};
use crate::{CaptureWindow, Controller};
use capture_annotation::{Annotation, AnnotationTool, CaptureSessionState};
use capture_core::geometry::{PhysicalPoint, PhysicalRect, PhysicalSize};
use capture_core::selection::{ResizeHandle, SelectionSession};
use capture_core::{place_toolbar, SnapCandidate, SnapKind, ToolbarPlacementReason};
use slint::ComponentHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub(super) struct EditorLayout {
    pub(super) window_bounds: PhysicalRect,
    pub(super) toolbar_rect: PhysicalRect,
    pub(super) work_area: PhysicalRect,
    pub(super) toolbar_reason: ToolbarPlacementReason,
}

pub(super) fn state_label(state: &CaptureSessionState) -> &'static str {
    match state {
        CaptureSessionState::Idle => "idle",
        CaptureSessionState::Preparing => "preparing",
        CaptureSessionState::Selecting(_) => "selecting",
        CaptureSessionState::Editing(_) => "editing",
    }
}

pub(super) fn cursor_kind_for_point(controller: &Controller, point: PhysicalPoint) -> &'static str {
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

pub(super) fn input_origin(
    _state: &CaptureSessionState,
    frame_origin: PhysicalPoint,
) -> PhysicalPoint {
    // The overlay remains the full virtual desktop in every state. Selection
    // and editor coordinates are therefore always translated from frame.origin.
    frame_origin
}

pub(super) fn refresh_ui(ui: &CaptureWindow, controller: &Controller) {
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
    refresh_pixel_inspector(ui, controller);
    controller.log.event(
        "render.refresh_ui",
        format!(
            "state={} duration_ms={:.3}",
            state_label(controller.runtime.state()),
            started.elapsed().as_secs_f64() * 1000.0
        ),
    );
}

pub(super) fn refresh_pointer_visuals(ui: &CaptureWindow, controller: &Controller) {
    match controller.runtime.state() {
        CaptureSessionState::Selecting(_) => refresh_selection_geometry(ui, controller),
        CaptureSessionState::Editing(_) => {
            refresh_selection_geometry(ui, controller);
            refresh_editor_overlay(ui, controller);
        }
        CaptureSessionState::Idle | CaptureSessionState::Preparing => refresh_ui(ui, controller),
    }
    refresh_pixel_inspector(ui, controller);
}

pub(super) fn refresh_editor_ui(ui: &CaptureWindow, controller: &Controller, refresh_base: bool) {
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
    refresh_pixel_inspector(ui, controller);
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

pub(super) fn refresh_selection_geometry(ui: &CaptureWindow, controller: &Controller) {
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
    ui.set_selection_info_text(format_selection_info(rect).into());
    let selection_info_position = place_selection_info(rect, controller.frame.bounds(), scale);
    ui.set_selection_info_x((selection_info_position.x - window_origin.x) as f32 / scale);
    ui.set_selection_info_y((selection_info_position.y - window_origin.y) as f32 / scale);
    ui.set_selection_info_visible(!rect.is_empty());
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

pub(super) fn is_committable_snap(candidate: &SnapCandidate) -> bool {
    candidate.kind != SnapKind::Desktop && !candidate.bounds.is_empty()
}

pub(super) fn handle_tolerance(controller: &Controller) -> u32 {
    ((10.0 * controller.scale_factor).round() as u32).max(6)
}

pub(super) fn moved_enough(start: PhysicalPoint, current: PhysicalPoint) -> bool {
    (current.x as i64 - start.x as i64).unsigned_abs() > 2
        || (current.y as i64 - start.y as i64).unsigned_abs() > 2
}

pub(super) fn editor_layout(controller: &Controller) -> Option<EditorLayout> {
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
        (428.0 * controller.scale_factor).round().max(1.0) as u32,
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

pub(super) fn sync_window_geometry(
    ui: &CaptureWindow,
    controller: &Controller,
    bounds: PhysicalRect,
) {
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
