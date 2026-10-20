use crate::{platform::Panel, tray::Tray};
use anyhow::{Result, anyhow};
use clipx_core::{DeviceId, DeviceInfo, Snapshot, SyncOptions, SyncRuntime};
use std::{sync::mpsc, thread, time::Duration};
use tao::{
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    platform::run_return::EventLoopExtRunReturn,
};

#[derive(Clone, Debug)]
pub enum Action {
    #[cfg(target_os = "macos")]
    MenuOpened,
    Generate,
    Join(String, Option<DeviceId>),
    Cancel,
    Add,
    Remove(DeviceId),
    Hide,
    Quit,
}

#[derive(Debug)]
pub enum AppEvent {
    Action(Action),
    State(Snapshot),
    Fatal(String),
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Pair,
    Waiting,
    Devices,
}

#[derive(Clone, Default)]
pub struct PanelState {
    pub page: Page,
    pub snapshot: Snapshot,
}

pub const PANEL_WIDTH: f64 = 280.0;

impl PanelState {
    pub fn status(&self) -> String {
        if self.snapshot.devices.is_empty() {
            "未配对".into()
        } else {
            let connected = self
                .snapshot
                .devices
                .iter()
                .filter(|peer| peer.connected)
                .count();
            format!("已连接 {connected} / {}", self.snapshot.devices.len())
        }
    }

    #[cfg(target_os = "macos")]
    pub fn title(&self) -> String {
        match self.page {
            Page::Pair if !self.snapshot.devices.is_empty() => "添加设备".into(),
            Page::Waiting => "等待配对".into(),
            _ => self.status(),
        }
    }

    pub fn message(&self) -> Option<&str> {
        self.snapshot
            .error
            .as_deref()
            .or_else(|| self.snapshot.joining.then_some("正在连接…"))
    }

    pub fn nearby_name(&self, device: &DeviceInfo) -> String {
        if self
            .snapshot
            .nearby
            .iter()
            .filter(|peer| peer.name == device.name)
            .count()
            > 1
        {
            format!("{} · {}", device.name, &hex::encode(device.id)[..6])
        } else {
            device.name.clone()
        }
    }

    pub fn home(&mut self) {
        self.page = if self.snapshot.devices.is_empty() {
            Page::Pair
        } else {
            Page::Devices
        };
    }
}

fn worker(options: SyncOptions, rx: mpsc::Receiver<Action>, proxy: EventLoopProxy<AppEvent>) {
    let runtime = match SyncRuntime::start(options) {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = proxy.send_event(AppEvent::Fatal(format!("{error:#}")));
            return;
        }
    };
    let mut last = None;
    let mut error = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Action::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(action) => {
                error = match action {
                    Action::Generate => runtime.generate_code().map(|_| ()),
                    Action::Join(code, device) => runtime.join(&code, device.map(hex::encode)),
                    Action::Cancel => {
                        runtime.cancel_pairing();
                        Ok(())
                    }
                    Action::Remove(id) => runtime.forget(id),
                    _ => Ok(()),
                }
                .err()
                .map(|error| format!("{error:#}"));
                last = None;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let mut state = runtime.snapshot();
        if let Some(error) = &error {
            state.error = Some(error.clone());
        }
        if last.as_ref() != Some(&state) {
            last = Some(state.clone());
            if proxy.send_event(AppEvent::State(state)).is_err() {
                break;
            }
        }
    }
}

pub fn run(options: SyncOptions) -> Result<()> {
    #[cfg(target_os = "linux")]
    if !["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|v| !v.is_empty()))
    {
        anyhow::bail!("桌面版需要图形会话；命令行同步请运行 clipx");
    }
    let mut builder = EventLoopBuilder::<AppEvent>::with_user_event();
    #[cfg(target_os = "windows")]
    {
        use tao::platform::windows::EventLoopBuilderExtWindows;
        builder.with_msg_hook(crate::platform::process_message);
    }
    let mut events = builder.build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        events.set_activation_policy(ActivationPolicy::Accessory);
        events.set_dock_visibility(false);
    }
    let proxy = events.create_proxy();
    let (tx, rx) = mpsc::channel();
    let worker_proxy = proxy.clone();
    let worker = thread::spawn(move || worker(options, rx, worker_proxy));
    let mut tray = None;
    let mut panel: Option<Panel> = None;
    let mut state = PanelState::default();
    let mut ready = false;
    let mut visible = false;
    let mut failure = None;

    events.run_return(|event, target, control| {
        *control = ControlFlow::Wait;
        let mut action = None;
        match event {
            Event::NewEvents(StartCause::Init) => {
                let result = Panel::new(target, proxy.clone()).and_then(|new_panel| {
                    let new_tray = Tray::new(proxy.clone(), &new_panel)?;
                    #[cfg(target_os = "macos")]
                    let new_panel = {
                        let mut new_panel = new_panel;
                        new_panel.bind_tray(new_tray.status_item());
                        new_panel
                    };
                    panel = Some(new_panel);
                    tray = Some(new_tray);
                    Ok(())
                });
                match result {
                    Ok(()) => {}
                    Err(error) => {
                        failure = Some(format!("{error:#}"));
                        *control = ControlFlow::Exit;
                    }
                }
            }
            Event::UserEvent(AppEvent::Fatal(error)) => {
                failure = Some(error);
                *control = ControlFlow::Exit;
            }
            Event::UserEvent(AppEvent::State(snapshot)) => {
                let paired = snapshot.paired;
                state.snapshot = snapshot;
                if !ready || paired {
                    state.home();
                    if cfg!(target_os = "macos") && !ready && state.snapshot.devices.is_empty() {
                        visible = true;
                    }
                    if paired {
                        if !cfg!(target_os = "macos") {
                            visible = false;
                        }
                        let _ = tx.send(Action::Cancel);
                    }
                    ready = true;
                }
                if state.page == Page::Devices && state.snapshot.devices.is_empty() {
                    state.home();
                }
            }
            Event::UserEvent(AppEvent::Action(request)) => action = Some(request),
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => action = Some(Action::Hide),
            Event::WindowEvent {
                event: WindowEvent::KeyboardInput { event: input, .. },
                ..
            } if input.physical_key == tao::keyboard::KeyCode::Escape => {
                action = Some(Action::Hide)
            }
            _ => {}
        }
        if let Some(action) = action {
            match action {
                Action::Quit => *control = ControlFlow::Exit,
                #[cfg(target_os = "macos")]
                Action::MenuOpened => visible = true,
                Action::Hide => {
                    visible = false;
                    // Closing the macOS menu must not revoke a code that is
                    // being entered on another device.
                    if !cfg!(target_os = "macos") {
                        state.snapshot.error = None;
                        state.home();
                        let _ = tx.send(Action::Cancel);
                    }
                }
                Action::Add => {
                    visible = true;
                    state.page = Page::Pair;
                    state.snapshot.error = None;
                    let _ = tx.send(Action::Cancel);
                }
                Action::Cancel => {
                    visible = cfg!(target_os = "macos");
                    state.snapshot.error = None;
                    state.home();
                    let _ = tx.send(Action::Cancel);
                }
                Action::Generate => {
                    visible = cfg!(target_os = "macos");
                    state.page = Page::Waiting;
                    state.snapshot.error = None;
                    let _ = tx.send(Action::Generate);
                }
                Action::Join(code, device) => {
                    let device = device.or_else(|| {
                        (state.snapshot.nearby.len() == 1).then(|| state.snapshot.nearby[0].id)
                    });
                    if device.is_none() && state.snapshot.nearby.len() > 1 {
                        state.snapshot.error = Some("请选择要配对的设备".into());
                    } else {
                        state.snapshot.error = None;
                        state.snapshot.joining = true;
                        let _ = tx.send(Action::Join(code, device));
                    }
                }
                Action::Remove(id) => {
                    if let Some(device) =
                        state.snapshot.devices.iter().find(|device| device.id == id)
                        && panel
                            .as_ref()
                            .is_some_and(|panel| panel.confirm_remove(&device.name))
                    {
                        let _ = tx.send(Action::Remove(id));
                    }
                }
            }
        }
        if let Some(tray) = &mut tray
            && let Err(error) = tray.update(&state)
        {
            failure = Some(format!("{error:#}"));
            *control = ControlFlow::Exit;
        }
        if let Some(panel) = &mut panel {
            panel.update(&state);
            if visible {
                panel.show();
            } else {
                panel.hide();
            }
        }
    });
    let _ = tx.send(Action::Quit);
    drop(tx);
    let _ = worker.join();
    drop(panel);
    drop(tray);
    failure.map_or(Ok(()), |message| Err(anyhow!(message)))
}
