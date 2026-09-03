use crate::{cli::SyncOptions, sync::SyncRuntime};
#[cfg(feature = "gui")]
use crate::{
    config::{self, DesktopConfig, DesktopRole},
    gui::{PairingPanel, PanelRequest},
};
#[cfg(target_os = "linux")]
use anyhow::bail;
use anyhow::{Context, Result, anyhow};
#[cfg(target_os = "linux")]
use std::env;
use std::time::{Duration, Instant};
#[cfg(target_os = "macos")]
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tao::{
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    platform::run_return::EventLoopExtRunReturn,
};
use tray_icon::{
    Icon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem},
};

#[cfg(feature = "gui")]
use std::sync::mpsc;

const EXIT_MENU_ID: &str = "exit";
#[cfg(feature = "gui")]
const OPEN_MENU_ID: &str = "open";

pub(crate) fn run(options: SyncOptions) -> Result<()> {
    #[cfg(target_os = "linux")]
    require_graphical_session()?;

    #[cfg(feature = "gui")]
    let mut config = DesktopConfig::from_sync_options(&options)?;
    #[cfg(feature = "gui")]
    let (panel_request_tx, panel_request_rx) = mpsc::channel::<PanelRequest>();
    #[allow(unused_mut)]
    let mut runtime = Some(SyncRuntime::start(options)?);
    let mut event_loop = EventLoop::new();
    let menu = Menu::new();
    let status_item = MenuItem::with_id("status", "剪贴板同步运行中", false, None);
    #[cfg(feature = "gui")]
    let open_item = MenuItem::with_id(OPEN_MENU_ID, "打开设备面板", true, None);
    let exit_item = MenuItem::with_id(EXIT_MENU_ID, "退出", true, None);
    menu.append(&status_item).context("创建托盘菜单失败")?;
    #[cfg(feature = "gui")]
    menu.append(&open_item).context("创建托盘菜单失败")?;
    menu.append(&exit_item).context("创建托盘菜单失败")?;

    let icon = Icon::from_rgba(icon_rgba()?, 32, 32).context("创建托盘图标失败")?;
    let exit_id = exit_item.id().clone();
    #[cfg(feature = "gui")]
    let open_id = open_item.id().clone();
    let mut tray_icon = None;
    let mut tray_error = None;

    #[cfg(feature = "gui")]
    let mut panel: Option<PairingPanel> = None;

    #[cfg(target_os = "macos")]
    {
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        event_loop.set_dock_visibility(false);
    }

    eprintln!("剪贴板同步：进入托盘事件循环");
    event_loop.run_return(|event, _event_loop_target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(250));

        match event {
            Event::NewEvents(StartCause::Init) => {
                match TrayIconBuilder::new()
                    .with_menu(Box::new(menu.clone()))
                    .with_tooltip("clipx 剪贴板同步")
                    .with_icon(icon.clone())
                    .with_icon_as_template(cfg!(target_os = "macos"))
                    .build()
                {
                    Ok(icon) => {
                        tray_icon = Some(icon);
                        eprintln!("剪贴板同步：托盘模式已启动");
                    }
                    Err(error) => {
                        tray_error = Some(format!("创建托盘图标失败：{error}"));
                        runtime.as_ref().expect("同步运行时应存在").stop();
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            Event::WindowEvent {
                window_id: _window_id,
                event: WindowEvent::CloseRequested,
                ..
            } => {
                #[cfg(feature = "gui")]
                if panel
                    .as_ref()
                    .is_some_and(|panel| panel.window_id() == _window_id)
                    && let Some(panel) = panel.as_ref()
                {
                    panel.hide();
                }
            }
            Event::MainEventsCleared => {
                while let Ok(event) = MenuEvent::receiver().try_recv() {
                    if *event.id() == exit_id {
                        eprintln!("剪贴板同步：正在退出");
                        runtime.as_ref().expect("同步运行时应存在").stop();
                        *control_flow = ControlFlow::Exit;
                        break;
                    }

                    #[cfg(feature = "gui")]
                    if *event.id() == open_id {
                        if let Some(panel) = panel.as_ref() {
                            panel.show();
                        } else {
                            match PairingPanel::new(
                                _event_loop_target,
                                &config,
                                panel_request_tx.clone(),
                            ) {
                                Ok(new_panel) => {
                                    new_panel.show();
                                    panel = Some(new_panel);
                                }
                                Err(error) => eprintln!("剪贴板同步：打开设备面板失败：{error:#}"),
                            }
                        }
                    }
                }

                #[cfg(feature = "gui")]
                while let Ok(request) = panel_request_rx.try_recv() {
                    match request {
                        PanelRequest::Disconnect => {
                            runtime.as_ref().expect("同步运行时应存在").disconnect();
                        }
                        PanelRequest::Reconnect => {
                            runtime.as_ref().expect("同步运行时应存在").reconnect();
                        }
                        PanelRequest::SaveConfig { role, address } => {
                            match DesktopConfig::from_form(role, &address)
                                .and_then(|next| apply_config(&mut config, &mut runtime, next))
                            {
                                Ok(()) => {
                                    if let Some(panel) = panel.as_mut() {
                                        panel.set_config(&config);
                                        panel.set_message("设置已应用");
                                    }
                                }
                                Err(error) => {
                                    if let Some(panel) = panel.as_ref() {
                                        panel.set_message(&format!("设置未应用：{error:#}"));
                                    }
                                }
                            }
                        }
                    }
                }

                let active_runtime = runtime.as_ref().expect("同步运行时应存在");
                #[cfg(feature = "gui")]
                let status_text = tray_status_text(&config, active_runtime);
                #[cfg(not(feature = "gui"))]
                let status_text = format!("已连接 {} 台设备", active_runtime.peer_count());
                status_item.set_text(status_text);

                #[cfg(feature = "gui")]
                if let Some(panel) = panel.as_mut() {
                    panel.set_peer_addresses(&active_runtime.peer_addresses());
                    let (status, action) = runtime_state(&config, active_runtime);
                    panel.set_runtime_state(status, action);
                }
            }
            _ => {}
        }
    });

    eprintln!("剪贴板同步：退出托盘事件循环");
    drop(tray_icon);
    #[cfg(feature = "gui")]
    drop(panel);
    tray_error.map_or(Ok(()), |error| Err(anyhow!(error)))
}

#[cfg(feature = "gui")]
fn apply_config(
    current_config: &mut DesktopConfig,
    runtime: &mut Option<SyncRuntime>,
    next_config: DesktopConfig,
) -> Result<()> {
    if *current_config == next_config {
        return Ok(());
    }

    let previous_config = current_config.clone();
    let old_runtime = runtime.take().context("应用连接设置时同步运行时不存在")?;
    old_runtime.stop();
    drop(old_runtime);

    let new_runtime = match SyncRuntime::start(next_config.sync_options()) {
        Ok(runtime) => runtime,
        Err(error) => {
            *runtime = Some(
                SyncRuntime::start(previous_config.sync_options())
                    .context("应用新连接设置失败，恢复旧设置也失败")?,
            );
            return Err(error).context("应用新连接设置失败");
        }
    };

    if let Err(error) = config::save(&next_config) {
        drop(new_runtime);
        *runtime = Some(
            SyncRuntime::start(previous_config.sync_options())
                .context("保存新设置失败，恢复旧设置也失败")?,
        );
        return Err(error).context("保存新连接设置失败");
    }

    *current_config = next_config;
    *runtime = Some(new_runtime);
    Ok(())
}

#[cfg(feature = "gui")]
fn runtime_state(
    config: &DesktopConfig,
    runtime: &SyncRuntime,
) -> (&'static str, Option<&'static str>) {
    if matches!(config.role, DesktopRole::Listen) {
        return ("运行中", None);
    }

    if !runtime.connection_enabled() {
        ("已断开", Some("reconnect"))
    } else if runtime.peer_count() > 0 {
        ("已连接", Some("disconnect"))
    } else {
        ("正在连接", Some("disconnect"))
    }
}

#[cfg(feature = "gui")]
fn tray_status_text(config: &DesktopConfig, runtime: &SyncRuntime) -> String {
    if matches!(config.role, DesktopRole::Listen) {
        format!("已连接 {} 台设备", runtime.peer_count())
    } else if !runtime.connection_enabled() {
        "连接已断开".to_string()
    } else if runtime.peer_count() > 0 {
        "已连接".to_string()
    } else {
        "正在连接".to_string()
    }
}

#[cfg(target_os = "linux")]
fn require_graphical_session() -> Result<()> {
    let has_display = ["DISPLAY", "WAYLAND_DISPLAY"]
        .into_iter()
        .any(|name| env::var_os(name).is_some_and(|value| !value.is_empty()));
    if !has_display {
        bail!("托盘模式需要图形桌面会话；请在 Ubuntu 桌面终端运行，或使用不带 --tray 的 sync 命令");
    }
    Ok(())
}

fn icon_rgba() -> Result<Vec<u8>> {
    let decoder = png::Decoder::new(std::io::Cursor::new(include_bytes!(
        "../assets/tray-icon.png"
    )));
    let mut reader = decoder.read_info().context("读取托盘图标失败")?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).context("解码托盘图标失败")?;
    if info.width != 32
        || info.height != 32
        || info.color_type != png::ColorType::Rgba
        || info.bit_depth != png::BitDepth::Eight
    {
        anyhow::bail!("托盘图标必须是 32x32 的 RGBA PNG");
    }
    Ok(buffer[..info.buffer_size()].to_vec())
}

#[cfg(test)]
mod tests {
    use super::icon_rgba;

    #[test]
    fn icon_has_transparent_background_and_visible_colors() {
        let pixels = icon_rgba().expect("托盘图标应能解码");
        assert_eq!(pixels.len(), 32 * 32 * 4);
        assert_eq!(&pixels[..4], &[0, 0, 0, 0]);
        assert!(pixels.as_chunks::<4>().0.contains(&[255, 255, 255, 255]));
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] > 0 && pixel[3] < 255)
        );
    }
}
