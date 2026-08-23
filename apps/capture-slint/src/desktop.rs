use crate::settings::Shortcut;
use std::sync::mpsc::Sender;

#[derive(Debug, Clone)]
pub enum DesktopCommand {
    Capture,
    Settings,
    Quit,
    #[cfg(target_os = "linux")]
    ShortcutStatus {
        shortcut: String,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutApply {
    Active,
    #[cfg(target_os = "linux")]
    AwaitingPortal,
}

pub struct DesktopStartup {
    pub integration: DesktopIntegration,
    pub messages: Vec<String>,
    pub shortcut_error: Option<String>,
}

#[cfg(any(windows, target_os = "linux"))]
const NO_HOTKEY: u32 = u32::MAX;

#[cfg(any(windows, target_os = "linux"))]
struct NativeHotkey {
    manager: global_hotkey::GlobalHotKeyManager,
    hotkey: global_hotkey::hotkey::HotKey,
    active_id: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

#[cfg(any(windows, target_os = "linux"))]
impl NativeHotkey {
    fn register(
        shortcut: &Shortcut,
        active_id: std::sync::Arc<std::sync::atomic::AtomicU32>,
    ) -> Result<Self, String> {
        use global_hotkey::GlobalHotKeyManager;
        use std::sync::atomic::Ordering;

        let hotkey = shortcut
            .global_hotkey()
            .map_err(|error| error.to_string())?;
        let manager = GlobalHotKeyManager::new().map_err(|error| error.to_string())?;
        manager
            .register(hotkey)
            .map_err(|error| error.to_string())?;
        active_id.store(hotkey.id(), Ordering::Release);
        Ok(Self {
            manager,
            hotkey,
            active_id,
        })
    }

    fn replace(&mut self, shortcut: &Shortcut) -> Result<(), String> {
        use std::sync::atomic::Ordering;

        let next = shortcut
            .global_hotkey()
            .map_err(|error| error.to_string())?;
        if next == self.hotkey {
            return Ok(());
        }
        self.manager
            .register(next)
            .map_err(|error| error.to_string())?;
        if let Err(error) = self.manager.unregister(self.hotkey) {
            let _ = self.manager.unregister(next);
            return Err(format!("无法释放原快捷键：{error}"));
        }
        self.hotkey = next;
        self.active_id.store(next.id(), Ordering::Release);
        Ok(())
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn install_native_hotkey_handler(
    sender: Sender<DesktopCommand>,
    active_id: std::sync::Arc<std::sync::atomic::AtomicU32>,
) {
    use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
    use std::sync::atomic::Ordering;

    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.id == active_id.load(Ordering::Acquire) && event.state == HotKeyState::Pressed {
            let _ = sender.send(DesktopCommand::Capture);
        }
    }));
}

#[cfg(windows)]
pub struct DesktopIntegration {
    active_hotkey_id: std::sync::Arc<std::sync::atomic::AtomicU32>,
    hotkey: Option<NativeHotkey>,
    _tray: Option<tray_icon::TrayIcon>,
}

#[cfg(windows)]
impl DesktopIntegration {
    pub fn set_shortcut(&mut self, shortcut: &Shortcut) -> Result<ShortcutApply, String> {
        if let Some(hotkey) = &mut self.hotkey {
            hotkey.replace(shortcut)?;
        } else {
            self.hotkey = Some(NativeHotkey::register(
                shortcut,
                self.active_hotkey_id.clone(),
            )?);
        }
        Ok(ShortcutApply::Active)
    }
}

#[cfg(windows)]
pub fn initialize(
    sender: Sender<DesktopCommand>,
    _native_wayland: bool,
    shortcut: &Shortcut,
) -> DesktopStartup {
    use std::sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    };
    use tray_icon::{
        menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
        Icon, TrayIconBuilder,
    };

    let mut messages = Vec::new();
    let capture_item = MenuItem::new("截图", true, None);
    let settings_item = MenuItem::new("设置", true, None);
    let quit_item = MenuItem::new("退出", true, None);
    let separator = PredefinedMenuItem::separator();
    let capture_item_id = capture_item.id().clone();
    let settings_item_id = settings_item.id().clone();
    let quit_item_id = quit_item.id().clone();
    let menu = match Menu::with_items(&[&capture_item, &settings_item, &separator, &quit_item]) {
        Ok(menu) => Some(menu),
        Err(error) => {
            messages.push(format!("desktop.tray.menu.error error={error}"));
            None
        }
    };

    let tray_sender = sender.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id() == &capture_item_id {
            let _ = tray_sender.send(DesktopCommand::Capture);
        } else if event.id() == &settings_item_id {
            let _ = tray_sender.send(DesktopCommand::Settings);
        } else if event.id() == &quit_item_id {
            let _ = tray_sender.send(DesktopCommand::Quit);
        }
    }));
    let tray = menu.and_then(|menu| {
        let icon = match Icon::from_rgba(tray_icon_rgba(32), 32, 32) {
            Ok(icon) => icon,
            Err(error) => {
                messages.push(format!("desktop.tray.icon.error error={error}"));
                return None;
            }
        };
        match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_tooltip("Chaos 截图")
            .build()
        {
            Ok(tray) => Some(tray),
            Err(error) => {
                messages.push(format!("desktop.tray.error error={error}"));
                None
            }
        }
    });

    let active_hotkey_id = Arc::new(AtomicU32::new(NO_HOTKEY));
    install_native_hotkey_handler(sender, active_hotkey_id.clone());
    let (hotkey, shortcut_error) = match NativeHotkey::register(shortcut, active_hotkey_id.clone())
    {
        Ok(hotkey) => (Some(hotkey), None),
        Err(error) => {
            active_hotkey_id.store(NO_HOTKEY, Ordering::Release);
            (
                None,
                Some(format!("无法注册全局快捷键 {shortcut}：{error}")),
            )
        }
    };

    DesktopStartup {
        integration: DesktopIntegration {
            active_hotkey_id,
            hotkey,
            _tray: tray,
        },
        messages,
        shortcut_error,
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct LinuxTray {
    sender: Sender<DesktopCommand>,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for LinuxTray {
    fn id(&self) -> String {
        "chaos-capture".into()
    }

    fn title(&self) -> String {
        "Chaos 截图".into()
    }

    fn icon_name(&self) -> String {
        "camera-photo".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![ksni::Icon {
            width: 32,
            height: 32,
            data: tray_icon_argb(32),
        }]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.sender.send(DesktopCommand::Capture);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};
        vec![
            StandardItem {
                label: "截图".into(),
                icon_name: "camera-photo".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.sender.send(DesktopCommand::Capture);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "设置".into(),
                icon_name: "preferences-system".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.sender.send(DesktopCommand::Settings);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "退出".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.sender.send(DesktopCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[cfg(target_os = "linux")]
struct PortalHotkey {
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    _thread: std::thread::JoinHandle<()>,
}

#[cfg(target_os = "linux")]
impl Drop for PortalHotkey {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(target_os = "linux")]
pub struct DesktopIntegration {
    sender: Sender<DesktopCommand>,
    native_wayland: bool,
    active_hotkey_id: std::sync::Arc<std::sync::atomic::AtomicU32>,
    hotkey: Option<NativeHotkey>,
    _tray: Option<ksni::blocking::Handle<LinuxTray>>,
    portal_hotkey: Option<PortalHotkey>,
}

#[cfg(target_os = "linux")]
impl DesktopIntegration {
    pub fn set_shortcut(&mut self, shortcut: &Shortcut) -> Result<ShortcutApply, String> {
        if self.native_wayland {
            let portal = spawn_wayland_shortcut(self.sender.clone(), shortcut.clone())?;
            self.portal_hotkey = Some(portal);
            return Ok(ShortcutApply::AwaitingPortal);
        }
        if let Some(hotkey) = &mut self.hotkey {
            hotkey.replace(shortcut)?;
        } else {
            self.hotkey = Some(NativeHotkey::register(
                shortcut,
                self.active_hotkey_id.clone(),
            )?);
        }
        Ok(ShortcutApply::Active)
    }
}

#[cfg(target_os = "linux")]
pub fn initialize(
    sender: Sender<DesktopCommand>,
    native_wayland: bool,
    shortcut: &Shortcut,
) -> DesktopStartup {
    use ksni::blocking::TrayMethods;
    use std::sync::{atomic::AtomicU32, Arc};

    let mut messages = Vec::new();
    let tray = match (LinuxTray {
        sender: sender.clone(),
    })
    .spawn()
    {
        Ok(handle) => Some(handle),
        Err(error) => {
            messages.push(format!("desktop.tray.error error={error}"));
            None
        }
    };

    let active_hotkey_id = Arc::new(AtomicU32::new(NO_HOTKEY));
    let (hotkey, portal_hotkey, shortcut_error) = if native_wayland {
        match spawn_wayland_shortcut(sender.clone(), shortcut.clone()) {
            Ok(portal) => (None, Some(portal), None),
            Err(error) => (
                None,
                None,
                Some(format!("无法启动 Wayland 全局快捷键：{error}")),
            ),
        }
    } else {
        install_native_hotkey_handler(sender.clone(), active_hotkey_id.clone());
        match NativeHotkey::register(shortcut, active_hotkey_id.clone()) {
            Ok(hotkey) => (Some(hotkey), None, None),
            Err(error) => (
                None,
                None,
                Some(format!("无法注册全局快捷键 {shortcut}：{error}")),
            ),
        }
    };

    DesktopStartup {
        integration: DesktopIntegration {
            sender,
            native_wayland,
            active_hotkey_id,
            hotkey,
            _tray: tray,
            portal_hotkey,
        },
        messages,
        shortcut_error,
    }
}

#[cfg(target_os = "linux")]
fn spawn_wayland_shortcut(
    sender: Sender<DesktopCommand>,
    shortcut: Shortcut,
) -> Result<PortalHotkey, String> {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    let cancelled = Arc::new(AtomicBool::new(false));
    let thread_cancelled = cancelled.clone();
    let shortcut_text = shortcut.to_string();
    let thread = std::thread::Builder::new()
        .name("wayland-global-shortcut".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build();
            let result = runtime
                .map_err(|error| error.to_string())
                .and_then(|runtime| {
                    runtime.block_on(run_wayland_shortcut(
                        sender.clone(),
                        shortcut,
                        thread_cancelled.clone(),
                    ))
                });
            if let Err(error) = result {
                if !thread_cancelled.load(Ordering::Acquire) {
                    let _ = sender.send(DesktopCommand::ShortcutStatus {
                        shortcut: shortcut_text,
                        error: Some(format!("Wayland 全局快捷键不可用：{error}")),
                    });
                }
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(PortalHotkey {
        cancelled,
        _thread: thread,
    })
}

#[cfg(target_os = "linux")]
async fn run_wayland_shortcut(
    sender: Sender<DesktopCommand>,
    shortcut: Shortcut,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    use futures_util::{
        future::{select, Either},
        pin_mut, StreamExt,
    };
    use std::sync::atomic::Ordering;

    let shortcuts = GlobalShortcuts::new()
        .await
        .map_err(|error| error.to_string())?;
    let session = shortcuts
        .create_session()
        .await
        .map_err(|error| error.to_string())?;
    let trigger = shortcut.portal_trigger();
    let request = shortcuts
        .bind_shortcuts(
            &session,
            &[NewShortcut::new("capture", "截取屏幕").preferred_trigger(Some(trigger.as_str()))],
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
    let response = request.response().map_err(|error| error.to_string())?;
    if !response
        .shortcuts()
        .iter()
        .any(|shortcut| shortcut.id() == "capture")
    {
        return Err("桌面门户没有绑定截图快捷键".into());
    }
    if cancelled.load(Ordering::Acquire) {
        return Ok(());
    }
    let _ = sender.send(DesktopCommand::ShortcutStatus {
        shortcut: shortcut.to_string(),
        error: None,
    });

    let mut activated = shortcuts
        .receive_activated()
        .await
        .map_err(|error| error.to_string())?;
    loop {
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        let event = activated.next();
        let timeout = tokio::time::sleep(std::time::Duration::from_millis(100));
        pin_mut!(event, timeout);
        match select(event, timeout).await {
            Either::Left((Some(event), _)) => {
                if event.shortcut_id() == "capture"
                    && !cancelled.load(Ordering::Acquire)
                    && sender.send(DesktopCommand::Capture).is_err()
                {
                    break;
                }
            }
            Either::Left((None, _)) => {
                return Err("桌面门户的快捷键事件流已结束".to_string());
            }
            Either::Right(_) => {}
        }
    }
    drop(session);
    Ok(())
}

fn tray_icon_rgba(size: usize) -> Vec<u8> {
    let mut pixels = vec![0; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let bracket = ((5..=8).contains(&x) && (5..=14).contains(&y))
                || ((5..=14).contains(&x) && (5..=8).contains(&y))
                || ((size - 9..=size - 6).contains(&x) && (5..=14).contains(&y))
                || ((size - 15..=size - 6).contains(&x) && (5..=8).contains(&y))
                || ((5..=8).contains(&x) && (size - 15..=size - 6).contains(&y))
                || ((5..=14).contains(&x) && (size - 9..=size - 6).contains(&y))
                || ((size - 9..=size - 6).contains(&x) && (size - 15..=size - 6).contains(&y))
                || ((size - 15..=size - 6).contains(&x) && (size - 9..=size - 6).contains(&y));
            if bracket {
                let offset = (y * size + x) * 4;
                pixels[offset..offset + 4].copy_from_slice(&[37, 99, 235, 255]);
            }
        }
    }
    pixels
}

#[cfg(target_os = "linux")]
fn tray_icon_argb(size: usize) -> Vec<u8> {
    tray_icon_rgba(size)
        .chunks_exact(4)
        .flat_map(|rgba| [rgba[3], rgba[0], rgba[1], rgba[2]])
        .collect()
}
