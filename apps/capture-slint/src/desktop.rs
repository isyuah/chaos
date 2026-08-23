use std::sync::mpsc::Sender;

#[derive(Debug, Clone)]
pub enum DesktopCommand {
    Capture,
    Quit,
    #[cfg(target_os = "linux")]
    Error(String),
}

pub struct DesktopStartup {
    pub integration: DesktopIntegration,
    pub messages: Vec<String>,
}

#[cfg(windows)]
pub struct DesktopIntegration {
    _hotkey: Option<global_hotkey::GlobalHotKeyManager>,
    _tray: Option<tray_icon::TrayIcon>,
}

#[cfg(windows)]
pub fn initialize(sender: Sender<DesktopCommand>, _native_wayland: bool) -> DesktopStartup {
    use global_hotkey::{
        hotkey::{Code, HotKey, Modifiers},
        GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    };
    use tray_icon::{
        menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
        Icon, TrayIconBuilder,
    };

    let mut messages = Vec::new();
    let capture_item = MenuItem::new("截图\tCtrl+Shift+S", true, None);
    let quit_item = MenuItem::new("退出", true, None);
    let separator = PredefinedMenuItem::separator();
    let capture_item_id = capture_item.id().clone();
    let quit_item_id = quit_item.id().clone();
    let menu = match Menu::with_items(&[&capture_item, &separator, &quit_item]) {
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

    let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyS);
    let hotkey_id = hotkey.id();
    let hotkey_sender = sender;
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.id == hotkey_id && event.state == HotKeyState::Pressed {
            let _ = hotkey_sender.send(DesktopCommand::Capture);
        }
    }));
    let hotkey_manager = match GlobalHotKeyManager::new().and_then(|manager| {
        manager.register(hotkey)?;
        Ok(manager)
    }) {
        Ok(manager) => Some(manager),
        Err(error) => {
            messages.push(format!(
                "desktop.hotkey.error shortcut=Ctrl+Shift+S error={error}"
            ));
            None
        }
    };

    DesktopStartup {
        integration: DesktopIntegration {
            _hotkey: hotkey_manager,
            _tray: tray,
        },
        messages,
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
                label: "截图  Ctrl+Shift+S".into(),
                icon_name: "camera-photo".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.sender.send(DesktopCommand::Capture);
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
pub struct DesktopIntegration {
    _hotkey: Option<global_hotkey::GlobalHotKeyManager>,
    _tray: Option<ksni::blocking::Handle<LinuxTray>>,
    _portal_thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "linux")]
pub fn initialize(sender: Sender<DesktopCommand>, native_wayland: bool) -> DesktopStartup {
    use global_hotkey::{
        hotkey::{Code, HotKey, Modifiers},
        GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    };
    use ksni::blocking::TrayMethods;

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

    let (hotkey_manager, portal_thread) = if native_wayland {
        let portal_sender = sender.clone();
        let thread = std::thread::Builder::new()
            .name("wayland-global-shortcut".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build();
                let result = runtime
                    .map_err(|error| error.to_string())
                    .and_then(|runtime| {
                        runtime.block_on(run_wayland_shortcut(portal_sender.clone()))
                    });
                if let Err(error) = result {
                    let _ = portal_sender.send(DesktopCommand::Error(format!(
                        "Wayland 全局快捷键不可用：{error}"
                    )));
                }
            });
        match thread {
            Ok(thread) => (None, Some(thread)),
            Err(error) => {
                messages.push(format!("desktop.hotkey.thread.error error={error}"));
                (None, None)
            }
        }
    } else {
        let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyS);
        let hotkey_id = hotkey.id();
        let hotkey_sender = sender;
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.id == hotkey_id && event.state == HotKeyState::Pressed {
                let _ = hotkey_sender.send(DesktopCommand::Capture);
            }
        }));
        let manager = match GlobalHotKeyManager::new().and_then(|manager| {
            manager.register(hotkey)?;
            Ok(manager)
        }) {
            Ok(manager) => Some(manager),
            Err(error) => {
                messages.push(format!(
                    "desktop.hotkey.error shortcut=Ctrl+Shift+S error={error}"
                ));
                None
            }
        };
        (manager, None)
    };

    DesktopStartup {
        integration: DesktopIntegration {
            _hotkey: hotkey_manager,
            _tray: tray,
            _portal_thread: portal_thread,
        },
        messages,
    }
}

#[cfg(target_os = "linux")]
async fn run_wayland_shortcut(sender: Sender<DesktopCommand>) -> Result<(), String> {
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    use futures_util::StreamExt;

    let shortcuts = GlobalShortcuts::new()
        .await
        .map_err(|error| error.to_string())?;
    let session = shortcuts
        .create_session()
        .await
        .map_err(|error| error.to_string())?;
    let request = shortcuts
        .bind_shortcuts(
            &session,
            &[NewShortcut::new("capture", "截取屏幕").preferred_trigger(Some("CTRL+SHIFT+s"))],
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

    let mut activated = shortcuts
        .receive_activated()
        .await
        .map_err(|error| error.to_string())?;
    while let Some(event) = activated.next().await {
        if event.shortcut_id() == "capture" && sender.send(DesktopCommand::Capture).is_err() {
            break;
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
