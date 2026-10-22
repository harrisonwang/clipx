use crate::{platform::Panel, tray::Tray};
use anyhow::{Result, anyhow};
use clipx_core::{DeviceId, DeviceInfo, JoinProgress, Snapshot, SyncOptions, SyncRuntime};
use std::{sync::mpsc, thread, time::Duration};
use tao::{
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    platform::run_return::EventLoopExtRunReturn,
};

#[derive(Clone, Debug)]
pub enum Action {
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
}

#[derive(Clone, Default)]
pub struct PanelState {
    pub page: Page,
    pub snapshot: Snapshot,
}

pub const PANEL_WIDTH: f64 = 368.0;

pub fn normalize_code(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).take(6).collect()
}

pub fn can_pair(code: &str, nearby: &[DeviceId], selected: Option<DeviceId>) -> bool {
    code.len() == 6 && (nearby.len() <= 1 || selected.is_some())
}

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

    pub fn message(&self) -> Option<&str> {
        self.snapshot
            .error
            .as_deref()
            .or(match self.snapshot.joining {
                Some(JoinProgress::Discovering) => Some("正在查找设备…"),
                Some(JoinProgress::Connecting) => Some("正在连接…"),
                None => None,
            })
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

    pub fn code_label(&self) -> String {
        self.snapshot.code.as_ref().map_or_else(
            || "正在生成…".into(),
            |code| format!("{} {}", &code[..3], &code[3..]),
        )
    }

    pub fn remaining_label(&self) -> String {
        if self.snapshot.code.is_none() {
            return String::new();
        }
        format!(
            "{:02}:{:02} 后自动更新",
            self.snapshot.seconds_left / 60,
            self.snapshot.seconds_left % 60
        )
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
    let mut visible = false;
    let mut failure = None;

    events.run_return(|event, target, control| {
        *control = ControlFlow::Wait;
        let was_visible = visible;
        let mut activate = false;
        let mut action = None;
        match event {
            Event::NewEvents(StartCause::Init) => {
                let result = Panel::new(target, proxy.clone()).and_then(|new_panel| {
                    let new_tray = Tray::new(proxy.clone())?;
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
                if paired {
                    state.page = Page::Pair;
                    visible = false;
                    let _ = tx.send(Action::Cancel);
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
                Action::Hide => {
                    visible = false;
                }
                Action::Add => {
                    visible = true;
                    activate = true;
                }
                Action::Cancel => {
                    state.page = Page::Pair;
                    state.snapshot.error = None;
                    state.snapshot.code = None;
                    state.snapshot.seconds_left = 0;
                    let _ = tx.send(Action::Cancel);
                }
                Action::Generate => {
                    visible = true;
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
                        state.snapshot.joining = Some(JoinProgress::Discovering);
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
            if visible && (!was_visible || activate) {
                panel.show();
            } else if !visible && was_visible {
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
