use crate::image::image_from_rgba;
use crate::{CaptureWindow, Controller};
use capture_annotation::CaptureSessionState;
use capture_core::capture::{CapturedFrame, PixelFormat};
use capture_core::geometry::{PhysicalPoint, PhysicalRect, PhysicalSize};
use slint::{Color, Image};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum InspectorColorFormat {
    #[default]
    Hex,
    Rgb,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum InspectorCoordinateMode {
    #[default]
    Relative,
    Absolute,
}

pub(super) fn format_selection_info(rect: PhysicalRect) -> String {
    format!(
        "({}, {})  {} × {}",
        rect.origin.x,
        rect.origin.y,
        rect.width(),
        rect.height()
    )
}

pub(super) fn place_selection_info(
    selection: PhysicalRect,
    frame_bounds: PhysicalRect,
    scale: f32,
) -> PhysicalPoint {
    let margin = (8.0 * scale).round().max(1.0) as i32;
    let info_width = (230.0 * scale).round().max(1.0) as u32;
    let info_height = (30.0 * scale).round().max(1.0) as u32;
    let above_y = selection
        .origin
        .y
        .saturating_sub(info_height as i32)
        .saturating_sub(margin);
    let above = PhysicalRect::new(
        PhysicalPoint::new(selection.origin.x, above_y),
        PhysicalSize::new(info_width, info_height),
    )
    .clamp(PhysicalRect::new(
        PhysicalPoint::new(frame_bounds.left(), above_y),
        PhysicalSize::new(frame_bounds.width(), info_height),
    ));
    if above.top() >= frame_bounds.top() {
        return above.origin;
    }

    let below_y = selection.bottom().saturating_add(margin);
    let below = PhysicalRect::new(
        PhysicalPoint::new(selection.origin.x, below_y),
        PhysicalSize::new(info_width, info_height),
    )
    .clamp(PhysicalRect::new(
        PhysicalPoint::new(frame_bounds.left(), below_y),
        PhysicalSize::new(frame_bounds.width(), info_height),
    ));
    if below.bottom() <= frame_bounds.bottom() {
        return below.origin;
    }

    let inside = PhysicalRect::new(
        PhysicalPoint::new(
            selection.origin.x.saturating_add(margin),
            selection.origin.y.saturating_add(margin),
        ),
        PhysicalSize::new(info_width, info_height),
    );
    inside.clamp(frame_bounds).origin
}

pub(super) fn refresh_pixel_inspector(ui: &CaptureWindow, controller: &Controller) {
    let Some(point) = controller.last_pointer else {
        ui.set_inspector_visible(false);
        return;
    };
    let Some(pixel) = sample_frame_pixel(&controller.frame, point) else {
        ui.set_inspector_visible(false);
        return;
    };
    let Some(magnified) = magnifier_image(&controller.frame, point, 15) else {
        ui.set_inspector_visible(false);
        return;
    };
    let scale = controller.scale_factor as f32;
    let local_x = (point.x - controller.frame.origin.x) as f32 / scale;
    let local_y = (point.y - controller.frame.origin.y) as f32 / scale;
    let coordinate = match controller.inspector_coordinate_mode {
        InspectorCoordinateMode::Relative => {
            let origin = current_selection_rect(controller)
                .map(|selection| selection.origin)
                .unwrap_or(controller.frame.origin);
            format!("相对 {}, {}", point.x - origin.x, point.y - origin.y)
        }
        InspectorCoordinateMode::Absolute => format!("绝对 {}, {}", point.x, point.y),
    };
    let color = format_pixel_color(pixel, controller.inspector_color_format);

    ui.set_inspector_x(local_x);
    ui.set_inspector_y(local_y);
    ui.set_inspector_image(magnified);
    ui.set_inspector_color(Color::from_argb_u8(pixel[3], pixel[0], pixel[1], pixel[2]));
    ui.set_inspector_color_text(color.into());
    ui.set_inspector_position_text(coordinate.into());
    ui.set_inspector_visible(true);
}

fn current_selection_rect(controller: &Controller) -> Option<PhysicalRect> {
    match controller.runtime.state() {
        CaptureSessionState::Selecting(selection) => {
            (!selection.rect.is_empty()).then_some(selection.rect)
        }
        CaptureSessionState::Editing(editor) => Some(editor.document.crop),
        CaptureSessionState::Idle | CaptureSessionState::Preparing => None,
    }
}

pub(super) fn sample_frame_pixel(frame: &CapturedFrame, point: PhysicalPoint) -> Option<[u8; 4]> {
    if frame.pixel_format != PixelFormat::Rgba8 || !frame.bounds().contains_exclusive(point) {
        return None;
    }
    let x = usize::try_from(point.x as i64 - frame.origin.x as i64).ok()?;
    let y = usize::try_from(point.y as i64 - frame.origin.y as i64).ok()?;
    let index = y
        .checked_mul(frame.stride as usize)?
        .checked_add(x.checked_mul(4)?)?;
    let pixel = frame.pixels.get(index..index.checked_add(4)?)?;
    Some([pixel[0], pixel[1], pixel[2], pixel[3]])
}

fn magnifier_image(frame: &CapturedFrame, center: PhysicalPoint, size: u32) -> Option<Image> {
    if size == 0 || size.is_multiple_of(2) {
        return None;
    }
    let mut pixels = vec![0_u8; size.checked_mul(size)?.checked_mul(4)? as usize];
    let radius = (size / 2) as i32;
    let bounds = frame.bounds();
    for y in 0..size {
        for x in 0..size {
            let sample = bounds.clamp_point(PhysicalPoint::new(
                center.x.saturating_add(x as i32 - radius),
                center.y.saturating_add(y as i32 - radius),
            ));
            let rgba = sample_frame_pixel(frame, sample)?;
            let index = ((y * size + x) * 4) as usize;
            pixels[index..index + 4].copy_from_slice(&rgba);
        }
    }
    Some(image_from_rgba(size, size, &pixels))
}

fn format_pixel_color(pixel: [u8; 4], format: InspectorColorFormat) -> String {
    match format {
        InspectorColorFormat::Hex => format!("#{:02X}{:02X}{:02X}", pixel[0], pixel[1], pixel[2]),
        InspectorColorFormat::Rgb => format!("RGB({}, {}, {})", pixel[0], pixel[1], pixel[2]),
    }
}

pub(super) fn current_pixel_color_text(controller: &Controller) -> Option<String> {
    let pixel = sample_frame_pixel(&controller.frame, controller.last_pointer?)?;
    Some(format_pixel_color(pixel, controller.inspector_color_format))
}
