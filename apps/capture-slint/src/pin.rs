use crate::actions::{choose_save_path, copy_rendered_to_clipboard};
use crate::PinWindow;
use capture_core::geometry::PhysicalSize;
use capture_render::{save_png, RenderedImage};
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

pub(super) fn configure_pin_window(
    pin: &PinWindow,
    rendered: Arc<RenderedImage>,
    save_directory: PathBuf,
) {
    {
        let pin_weak = pin.as_weak();
        let rendered = rendered.clone();
        pin.on_run_action(move |action| {
            let Some(pin) = pin_weak.upgrade() else {
                return;
            };
            match action.as_str() {
                "copy" => match copy_rendered_to_clipboard(&rendered) {
                    Ok(()) => set_pin_status(&pin, "已复制到剪贴板"),
                    Err(error) => set_pin_status(&pin, format!("复制失败：{error}")),
                },
                "save-as" => match choose_save_path(&save_directory, pin.window()) {
                    Ok(Some(path)) => match save_png(&path, &rendered) {
                        Ok(()) => set_pin_status(&pin, format!("已保存到 {}", path.display())),
                        Err(error) => set_pin_status(&pin, format!("保存失败：{error}")),
                    },
                    Ok(None) => {}
                    Err(error) => set_pin_status(&pin, format!("无法打开另存为对话框：{error}")),
                },
                "close" => {
                    let _ = pin.hide();
                }
                _ => {}
            }
        });
    }

    let zoom_factor = Rc::new(Cell::new(1.0_f32));
    {
        let pin_weak = pin.as_weak();
        let zoom_factor = zoom_factor.clone();
        let base_size = PhysicalSize::new(rendered.width, rendered.height);
        pin.on_zoom(move |delta, pointer_x, pointer_y| {
            let Some(pin) = pin_weak.upgrade() else {
                return;
            };
            let previous_zoom = zoom_factor.get();
            let next_zoom = next_pin_zoom(previous_zoom, delta, base_size);
            set_pin_zoom(&pin, next_zoom);

            let previous_size = pin.window().size();
            let next_size = pin_size_for_zoom(base_size, next_zoom);
            if previous_size == next_size {
                return;
            }

            let window_scale = pin.window().scale_factor().max(0.1);
            let anchor_x = (pointer_x * window_scale).clamp(0.0, previous_size.width as f32);
            let anchor_y = (pointer_y * window_scale).clamp(0.0, previous_size.height as f32);
            let next_anchor_x =
                anchor_x * next_size.width as f32 / previous_size.width.max(1) as f32;
            let next_anchor_y =
                anchor_y * next_size.height as f32 / previous_size.height.max(1) as f32;
            let position = pin.window().position();
            let next_position = slint::PhysicalPosition::new(
                position
                    .x
                    .saturating_add((anchor_x - next_anchor_x).round() as i32),
                position
                    .y
                    .saturating_add((anchor_y - next_anchor_y).round() as i32),
            );

            pin.window().set_size(next_size);
            pin.window().set_position(next_position);
            zoom_factor.set(next_zoom);
        });
    }

    let drag_anchor = Rc::new(RefCell::new(None::<(f32, f32)>));
    {
        let pin_weak = pin.as_weak();
        let drag_anchor = drag_anchor.clone();
        pin.on_pointer_down(move |x, y| {
            if pin_weak.upgrade().is_some() {
                *drag_anchor.borrow_mut() = Some((x, y));
            }
        });
    }
    {
        let pin_weak = pin.as_weak();
        let drag_anchor = drag_anchor.clone();
        pin.on_pointer_move(move |x, y| {
            let Some((anchor_x, anchor_y)) = *drag_anchor.borrow() else {
                return;
            };
            let Some(pin) = pin_weak.upgrade() else {
                return;
            };
            let scale = pin.window().scale_factor();
            let delta_x = ((x - anchor_x) * scale).round() as i32;
            let delta_y = ((y - anchor_y) * scale).round() as i32;
            if delta_x == 0 && delta_y == 0 {
                return;
            }
            let position = pin.window().position();
            pin.window().set_position(slint::PhysicalPosition::new(
                position.x.saturating_add(delta_x),
                position.y.saturating_add(delta_y),
            ));
        });
    }
    pin.on_pointer_up(move || {
        *drag_anchor.borrow_mut() = None;
    });
}

pub(super) fn next_pin_zoom(current: f32, delta: f32, base_size: PhysicalSize) -> f32 {
    const STEP: f32 = 1.1;
    const MIN_ZOOM: f32 = 0.25;
    const MAX_ZOOM: f32 = 8.0;
    const MAX_PIN_EDGE: f32 = 16_384.0;

    let size_limit = (MAX_PIN_EDGE / base_size.width.max(1) as f32)
        .min(MAX_PIN_EDGE / base_size.height.max(1) as f32)
        .clamp(MIN_ZOOM, MAX_ZOOM);
    let candidate = if delta > 0.0 {
        current * STEP
    } else {
        current / STEP
    };
    candidate.clamp(MIN_ZOOM, size_limit)
}

pub(super) fn pin_size_for_zoom(base_size: PhysicalSize, zoom: f32) -> slint::PhysicalSize {
    slint::PhysicalSize::new(
        (base_size.width as f32 * zoom).round().clamp(1.0, 16_384.0) as u32,
        (base_size.height as f32 * zoom)
            .round()
            .clamp(1.0, 16_384.0) as u32,
    )
}

fn set_pin_status(pin: &PinWindow, message: impl Into<slint::SharedString>) {
    pin.set_status(message.into());
    pin.set_status_revision(pin.get_status_revision().wrapping_add(1));
}

fn set_pin_zoom(pin: &PinWindow, zoom: f32) {
    pin.set_zoom_text(format!("{}%", (zoom * 100.0).round() as u32).into());
    pin.set_zoom_revision(pin.get_zoom_revision().wrapping_add(1));
}
