use crate::{
    config::{self, DesktopConfig, DesktopRole},
    tray,
};
use anyhow::{Context, Result};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use tao::{
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    platform::run_return::EventLoopExtRunReturn,
    window::WindowBuilder,
};
use wry::WebViewBuilder;

struct SetupRequest {
    role: DesktopRole,
    address: String,
}

pub(crate) fn run() -> Result<()> {
    let config = match config::load()? {
        Some(config) => config,
        None => {
            let Some(config) = setup()? else {
                return Ok(());
            };
            config
        }
    };

    tray::run(config.sync_options())
}

fn setup() -> Result<Option<DesktopConfig>> {
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("clipx 设置")
        .with_inner_size(tao::dpi::LogicalSize::new(440.0, 300.0))
        .with_resizable(false)
        .with_visible(false)
        .build(&event_loop)
        .context("创建首次设置窗口失败")?;

    let (request_tx, request_rx) = mpsc::channel::<SetupRequest>();
    let builder = WebViewBuilder::new()
        .with_html(SETUP_HTML.to_string())
        .with_ipc_handler(move |request| {
            let body = request.body();
            let Some(address) = body.strip_prefix("save:listen:") else {
                if let Some(address) = body.strip_prefix("save:connect:") {
                    let _ = request_tx.send(SetupRequest {
                        role: DesktopRole::Connect,
                        address: address.to_string(),
                    });
                }
                return;
            };
            let _ = request_tx.send(SetupRequest {
                role: DesktopRole::Listen,
                address: address.to_string(),
            });
        });

    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;

        let vbox = window.default_vbox().context("创建设置窗口容器失败")?;
        builder
            .build_gtk(vbox)
            .context("创建首次设置窗口 WebView 失败")?
    };

    #[cfg(not(target_os = "linux"))]
    let webview = builder
        .build(&window)
        .context("创建首次设置窗口 WebView 失败")?;

    window.set_visible(true);
    let window_id = window.id();
    let mut result = None;
    let mut event_loop = event_loop;
    event_loop.run_return(|event, _event_loop_target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(100));

        match event {
            Event::NewEvents(StartCause::Init) => window.set_visible(true),
            Event::WindowEvent {
                window_id: event_window_id,
                event: WindowEvent::CloseRequested,
                ..
            } if event_window_id == window_id => {
                *control_flow = ControlFlow::Exit;
            }
            Event::MainEventsCleared => {
                while let Ok(request) = request_rx.try_recv() {
                    match DesktopConfig::from_form(request.role, &request.address)
                        .and_then(|config| config::save(&config).map(|()| config))
                    {
                        Ok(config) => {
                            result = Some(config);
                            *control_flow = ControlFlow::Exit;
                            break;
                        }
                        Err(error) => {
                            let script = format!(
                                "window.clipxSetError({});",
                                js_string(&format!("{error:#}"))
                            );
                            let _ = webview.evaluate_script(&script);
                        }
                    }
                }
            }
            _ => {}
        }
    });

    drop(webview);
    Ok(result)
}

fn js_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('\'');
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\'' => escaped.push_str("\\'"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            _ => escaped.push(character),
        }
    }
    escaped.push('\'');
    escaped
}

const SETUP_HTML: &str = r#"
<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <style>
    :root {
      color-scheme: light dark;
      --window: #f6f6f6;
      --surface: #ffffff;
      --ink: #1f1f1f;
      --muted: #6e6e73;
      --line: rgba(0, 0, 0, .14);
      --accent: #007aff;
      --accent-dark: #0066d6;
    }
    * { box-sizing: border-box; }
    html, body { width: 100%; height: 100%; overflow: hidden; }
    body {
      margin: 0;
      background: var(--window);
      color: var(--ink);
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
      font-size: 13px;
    }
    main { height: 100%; padding: 24px 24px 18px; }
    h1 { margin: 0; font-size: 20px; font-weight: 600; }
    .intro { margin: 5px 0 20px; color: var(--muted); font-size: 12px; }
    .role-picker { display: grid; grid-template-columns: 1fr 1fr; gap: 6px; }
    .role-picker button {
      height: 34px;
      border: 1px solid var(--line);
      border-radius: 6px;
      background: var(--surface);
      color: var(--muted);
      font: inherit;
      cursor: pointer;
    }
    .role-picker button.active { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 10%, var(--surface)); color: var(--accent-dark); }
    label { display: block; margin: 18px 0 6px; color: var(--muted); font-size: 11px; }
    input { width: 100%; height: 34px; padding: 0 10px; border: 1px solid GrayText; border-radius: 5px; background: Field; color: FieldText; font: inherit; outline: none; }
    input:focus { border-color: var(--accent); box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 24%, transparent); }
    .actions { display: flex; align-items: center; justify-content: space-between; margin-top: 18px; }
    #message { min-height: 14px; color: #a15c00; font-size: 11px; }
    #start { min-width: 82px; height: 32px; padding: 0 13px; border: 1px solid var(--accent-dark); border-radius: 5px; background: var(--accent); color: #fff; font: inherit; cursor: pointer; }
    #start:hover { background: var(--accent-dark); }
    @media (prefers-color-scheme: dark) {
      :root { --window: #2b2b2d; --surface: #363638; --ink: #f5f5f7; --muted: #a1a1a6; --line: rgba(255, 255, 255, .16); --accent: #0a84ff; --accent-dark: #409cff; }
      input { border-color: #77777c; }
      #message { color: #ffb340; }
    }
  </style>
</head>
<body>
  <main>
    <h1>设置 clipx</h1>
    <p class="intro">选择此设备的连接方式</p>
    <div class="role-picker" role="group" aria-label="连接方式">
      <button id="listen" class="active" type="button">监听端</button>
      <button id="connect" type="button">连接端</button>
    </div>
    <label id="address-label" for="address">监听地址</label>
    <input id="address" autocomplete="off" spellcheck="false" value="0.0.0.0:45876">
    <div class="actions">
      <span id="message" role="status"></span>
      <button id="start" type="button">启动 clipx</button>
    </div>
  </main>
  <script>
    document.addEventListener('contextmenu', event => event.preventDefault());

    const listen = document.getElementById('listen');
    const connect = document.getElementById('connect');
    const addressLabel = document.getElementById('address-label');
    const address = document.getElementById('address');
    const message = document.getElementById('message');
    let role = 'listen';

    const selectRole = nextRole => {
      role = nextRole;
      const isListen = role === 'listen';
      listen.classList.toggle('active', isListen);
      connect.classList.toggle('active', !isListen);
      addressLabel.textContent = isListen ? '监听地址' : '监听端地址';
      address.placeholder = isListen ? '0.0.0.0:45876' : '192.168.1.10:45876';
      if (isListen) address.value = address.value || '0.0.0.0:45876';
      else if (address.value === '0.0.0.0:45876') address.value = '';
      message.textContent = '';
    };

    listen.addEventListener('click', () => selectRole('listen'));
    connect.addEventListener('click', () => selectRole('connect'));
    document.getElementById('start').addEventListener('click', () => {
      const value = address.value.trim();
      if (!value) {
        message.textContent = '请输入地址和端口';
        address.focus();
        return;
      }
      message.textContent = '正在启动';
      window.ipc.postMessage('save:' + role + ':' + value);
    });
    window.clipxSetError = value => { message.textContent = value; };
  </script>
</body>
</html>
"#;
