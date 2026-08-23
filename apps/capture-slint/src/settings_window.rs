use crate::actions::prepare_save_directory;
use crate::desktop::{DesktopIntegration, ShortcutApply};
use crate::image::{image_from_frame, placeholder_frame};
use crate::settings::{SettingsStore, Shortcut, ShortcutParseError};
use crate::{CaptureWindow, Controller, SettingsWindow};
use capture_annotation::{CaptureCommand, CaptureSessionState};
use capture_runtime::RuntimeCommand;
use slint::ComponentHandle;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub(super) fn show_settings_window(
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

pub(super) fn shortcut_from_key_event(
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

pub(super) fn record_shortcut_status(
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
