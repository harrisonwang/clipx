use crate::app::{AppEvent, PanelState};
use crate::platform::Panel;
#[cfg(not(target_os = "linux"))]
use anyhow::Context;
use anyhow::Result;
use tao::event_loop::EventLoopProxy;
#[cfg(not(target_os = "linux"))]
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(target_os = "macos"))]
mod menu;

pub struct Tray {
    #[cfg(not(target_os = "linux"))]
    _icon: TrayIcon,
    #[cfg(target_os = "linux")]
    _icon: linux::Indicator,
    #[cfg(not(target_os = "macos"))]
    menu: menu::TrayMenu,
}

impl Tray {
    pub fn new(_proxy: EventLoopProxy<AppEvent>, _panel: &Panel) -> Result<Self> {
        #[cfg(not(target_os = "macos"))]
        let menu = menu::TrayMenu::new(_proxy)?;
        #[cfg(target_os = "linux")]
        let icon = linux::Indicator::new(&menu.menu)?;
        #[cfg(not(target_os = "linux"))]
        let icon = {
            let decoder = png::Decoder::new(std::io::Cursor::new(include_bytes!(
                "../../../assets/tray-icon.png"
            )));
            let mut reader = decoder.read_info().context("读取托盘图标失败")?;
            let mut pixels = vec![0; reader.output_buffer_size()];
            let info = reader.next_frame(&mut pixels)?;
            pixels.truncate(info.buffer_size());
            let builder = TrayIconBuilder::new()
                .with_tooltip("clipx")
                .with_icon(Icon::from_rgba(pixels, info.width, info.height)?)
                .with_icon_as_template(cfg!(target_os = "macos"));
            #[cfg(target_os = "windows")]
            let builder = builder.with_menu(Box::new(menu.menu.clone()));
            #[cfg(target_os = "macos")]
            let builder = builder.with_menu(Box::new(_panel.menu()));
            builder.build()?
        };
        Ok(Self {
            _icon: icon,
            #[cfg(not(target_os = "macos"))]
            menu,
        })
    }

    #[cfg(target_os = "macos")]
    pub fn status_item(&self) -> objc2::rc::Retained<objc2_app_kit::NSStatusItem> {
        self._icon.ns_status_item().expect("托盘创建原生状态栏项")
    }

    pub fn update(&mut self, state: &PanelState) -> Result<()> {
        #[cfg(not(target_os = "macos"))]
        self.menu.update(state)?;
        #[cfg(target_os = "macos")]
        {
            let _ = state;
        }
        Ok(())
    }
}
