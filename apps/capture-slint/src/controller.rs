use crate::actions::{
    choose_save_path, copy_document_to_clipboard, next_capture_path, reveal_saved_file, SaveMode,
};
use crate::image::{image_from_frame, image_from_rgba, placeholder_frame};
use crate::pin::configure_pin_window;
use crate::presentation::{input_origin, refresh_editor_ui, refresh_ui, state_label};
use crate::{CaptureWindow, Controller, PinWindow};
use capture_actions::{ActionOutcome, CaptureAction, CopyAction, PinAction, SaveAction};
use capture_annotation::{CaptureCommand, CaptureEvent, CaptureSessionState};
use capture_core::geometry::PhysicalPoint;
use capture_core::{ActionId, SnapExclusionToken};
use capture_render::flatten;
use capture_runtime::{
    ActionCompletion, ActionRequestId, RuntimeCommand, RuntimeError, RuntimeEvent,
};
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::ComponentHandle;
use std::sync::Arc;
use std::time::{Duration, Instant};

impl Controller {
    pub(super) fn to_physical(&self, x: f32, y: f32) -> capture_core::PhysicalPoint {
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

    pub(super) fn sync_snap_exclusions(&mut self, ui: &CaptureWindow) {
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

    pub(super) fn apply(&mut self, command: CaptureCommand) {
        self.dispatch_runtime(RuntimeCommand::Capture(command));
    }

    pub(super) fn dispatch_runtime(&mut self, command: RuntimeCommand) -> Vec<RuntimeEvent> {
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

    pub(super) fn set_status(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_revision = self.status_revision.wrapping_add(1);
    }

    pub(super) fn should_query_snap(&mut self) -> bool {
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

    pub(super) fn should_render_visuals(&mut self) -> bool {
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

    pub(super) fn run_action(&mut self, action: &str, ui: &CaptureWindow) {
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
            "copy" | "save" | "save-open" | "save-as" | "pin" | "ask-ai" => {
                let save_mode = match action {
                    "save" => Some(SaveMode::Default),
                    "save-open" => Some(SaveMode::DefaultAndReveal),
                    "save-as" => Some(SaveMode::SaveAs),
                    _ => None,
                };
                let action_id = match action {
                    "copy" => ActionId::COPY,
                    "save" | "save-open" | "save-as" => ActionId::SAVE,
                    "pin" => ActionId::PIN,
                    "ask-ai" => ActionId::ASK_AI,
                    _ => unreachable!(),
                };
                let events = self.dispatch_runtime(RuntimeCommand::Capture(
                    CaptureCommand::InvokeAction(action_id),
                ));
                for event in events {
                    if let RuntimeEvent::ActionRequested { request_id, action } = event {
                        self.execute_builtin_action(request_id, action, save_mode, ui);
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
        save_mode: Option<SaveMode>,
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
                let save_mode = save_mode.unwrap_or(SaveMode::Default);
                let path = match save_mode {
                    SaveMode::Default | SaveMode::DefaultAndReveal => {
                        match next_capture_path(&self.config.settings.save_directory) {
                            Ok(path) => path,
                            Err(error) => {
                                self.complete_action(
                                    request_id,
                                    false,
                                    format!("保存失败：{error}"),
                                    ui,
                                );
                                return;
                            }
                        }
                    }
                    SaveMode::SaveAs => {
                        match choose_save_path(&self.config.settings.save_directory, ui.window()) {
                            Ok(Some(path)) => path,
                            Ok(None) => {
                                self.complete_action(request_id, false, "已取消另存为", ui);
                                return;
                            }
                            Err(error) => {
                                self.complete_action(
                                    request_id,
                                    false,
                                    format!("无法打开另存为对话框：{error}"),
                                    ui,
                                );
                                return;
                            }
                        }
                    }
                };
                match SaveAction::new(&path).invoke(&document) {
                    Ok(ActionOutcome::Saved(path)) => {
                        let message = if save_mode == SaveMode::DefaultAndReveal {
                            match reveal_saved_file(&path) {
                                Ok(()) => format!("已保存到 {}", path.display()),
                                Err(error) => format!(
                                    "已保存到 {}，但无法打开所在位置：{error}",
                                    path.display()
                                ),
                            }
                        } else {
                            format!("已保存到 {}", path.display())
                        };
                        self.complete_action(request_id, true, message, ui);
                    }
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
                        let rendered = Arc::new(rendered);
                        let image =
                            image_from_rgba(rendered.width, rendered.height, &rendered.pixels);
                        pin.set_pin_image(image);
                        pin.window()
                            .set_size(slint::PhysicalSize::new(rendered.width, rendered.height));
                        pin.window().set_position(slint::PhysicalPosition::new(
                            document.crop.origin.x.saturating_add(16),
                            document.crop.origin.y.saturating_add(16),
                        ));
                        configure_pin_window(
                            &pin,
                            rendered,
                            self.config.settings.save_directory.clone(),
                        );
                        let _ = pin.show();
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
