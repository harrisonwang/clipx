use crate::app::Page;
use crate::app::{Action, AppEvent, PanelState};
use anyhow::Result;
use std::collections::HashMap;
use tao::event_loop::EventLoopProxy;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use zipover_core::DeviceId;

pub struct TrayMenu {
    pub menu: Menu,
    status: MenuItem,
    code: MenuItem,
    add: MenuItem,
    cancel: MenuItem,
    quit: MenuItem,
    remove_items: HashMap<DeviceId, MenuItem>,
    previous: Option<PanelState>,
}

impl TrayMenu {
    pub fn new(proxy: EventLoopProxy<AppEvent>) -> Result<Self> {
        let mut tray = Self {
            menu: Menu::new(),
            status: MenuItem::new("未配对", false, None),
            code: MenuItem::with_id("code", "正在生成…", true, None),
            add: MenuItem::with_id("add", "添加设备…", true, None),
            cancel: MenuItem::with_id("cancel", "取消配对", true, None),
            quit: MenuItem::with_id("quit", "退出", true, None),
            remove_items: HashMap::new(),
            previous: None,
        };
        #[cfg(target_os = "macos")]
        {
            use tray_icon::menu::ContextMenu;
            // Muda owns the NSMenu; keep the compact native tray width.
            unsafe {
                let native = &*tray.menu.ns_menu().cast::<objc2_app_kit::NSMenu>();
                native.setMinimumWidth(280.0);
            }
        }
        tray.update(&PanelState::default())?;
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = match event.id.as_ref() {
                "add" | "code" => Action::Add,
                "cancel" => Action::Cancel,
                "quit" => Action::Quit,
                id => {
                    let Some(id) = id.strip_prefix("remove:").and_then(|id| {
                        let mut bytes = [0; 16];
                        hex::decode_to_slice(id, &mut bytes).ok()?;
                        Some(bytes)
                    }) else {
                        return;
                    };
                    Action::Remove(id)
                }
            };
            let _ = proxy.send_event(AppEvent::Action(action));
        }));
        Ok(tray)
    }

    pub fn update(&mut self, state: &PanelState) -> Result<()> {
        if self
            .previous
            .as_ref()
            .is_some_and(|old| old.page == state.page && old.snapshot == state.snapshot)
        {
            return Ok(());
        }
        let waiting = state.page == Page::Waiting;
        if self.previous.as_ref().is_none_or(|old| {
            old.snapshot.devices != state.snapshot.devices
                || (old.page == Page::Waiting) != waiting
                || old.snapshot.error != state.snapshot.error
        }) {
            // AppKit may deliver an old item's action even after its device is removed.
            // Keep actionable items alive for the tray's lifetime.
            while self.menu.remove_at(0).is_some() {}
            self.menu.append(&self.status)?;
            if waiting {
                self.menu.append(&self.code)?;
            }
            if let Some(error) = &state.snapshot.error {
                self.menu.append(&MenuItem::new(error, false, None))?;
            }
            if !state.snapshot.devices.is_empty() {
                self.menu.append(&PredefinedMenuItem::separator())?;
                for device in &state.snapshot.devices {
                    let label = format!(
                        "{} · {}",
                        device.name,
                        if device.connected { "在线" } else { "离线" }
                    );
                    let device_menu = Submenu::new(label, true);
                    let remove = self.remove_items.entry(device.id).or_insert_with(|| {
                        MenuItem::with_id(
                            format!("remove:{}", hex::encode(device.id)),
                            "移除设备…",
                            true,
                            None,
                        )
                    });
                    device_menu.append(remove)?;
                    self.menu.append(&device_menu)?;
                }
            }
            self.menu.append(&PredefinedMenuItem::separator())?;
            self.menu.append(&self.add)?;
            if waiting {
                self.menu.append(&self.cancel)?;
            }
            self.menu.append(&PredefinedMenuItem::separator())?;
            self.menu.append(&self.quit)?;
        }
        self.status.set_text(state.status());
        self.code.set_text(match &state.snapshot.code {
            Some(code) => format!(
                "{code} · {:02}:{:02} 后更新",
                state.snapshot.seconds_left / 60,
                state.snapshot.seconds_left % 60
            ),
            None if state.snapshot.error.is_some() => "配对码不可用".into(),
            None => "正在生成…".into(),
        });
        self.previous = Some(state.clone());
        Ok(())
    }
}
