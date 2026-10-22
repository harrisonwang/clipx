use crate::app::{AppEvent, PanelState};
#[cfg(not(target_os = "linux"))]
use anyhow::Context;
use anyhow::Result;
use tao::event_loop::EventLoopProxy;
#[cfg(not(target_os = "linux"))]
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[cfg(target_os = "linux")]
mod linux;

mod menu;

pub struct Tray {
    #[cfg(not(target_os = "linux"))]
    _icon: TrayIcon,
    #[cfg(target_os = "linux")]
    _icon: linux::Indicator,
    menu: menu::TrayMenu,
}

impl Tray {
    pub fn new(proxy: EventLoopProxy<AppEvent>) -> Result<Self> {
        let menu = menu::TrayMenu::new(proxy)?;
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
            let builder = builder.with_menu(Box::new(menu.menu.clone()));
            builder.build()?
        };
        Ok(Self { _icon: icon, menu })
    }

    pub fn update(&mut self, state: &PanelState) -> Result<()> {
        self.menu.update(state)
    }
}
