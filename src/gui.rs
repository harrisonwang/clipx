use crate::config::{DesktopConfig, DesktopRole};
use anyhow::{Context, Result};
use std::sync::mpsc::Sender;
use tao::{
    dpi::LogicalSize,
    event_loop::EventLoopWindowTarget,
    window::{Window, WindowBuilder, WindowId},
};
use wry::{WebView, WebViewBuilder};

pub(crate) struct PairingPanel {
    window: &'static Window,
    webview: WebView,
    last_peer_addresses: Vec<String>,
    last_runtime_state: Option<(String, Option<String>)>,
}

#[derive(Debug)]
pub(crate) enum PanelRequest {
    Disconnect,
    Reconnect,
    SaveConfig { role: DesktopRole, address: String },
}

impl PairingPanel {
    pub(crate) fn new(
        event_loop: &EventLoopWindowTarget<()>,
        config: &DesktopConfig,
        request_tx: Sender<PanelRequest>,
    ) -> Result<Self> {
        let listen_mode = matches!(config.role, DesktopRole::Listen);
        let window = Box::new(
            WindowBuilder::new()
                .with_title("clipx")
                .with_inner_size(LogicalSize::new(420.0, 470.0))
                .with_resizable(false)
                .with_visible(false)
                .build(event_loop)
                .context("创建设备面板窗口失败")?,
        );
        let window: &'static Window = Box::leak(window);

        let panel_html = PANEL_HTML
            .replace(
                "__LISTEN_MODE__",
                if listen_mode { "true" } else { "false" },
            )
            .replace(
                "__LISTEN_ADDRESS_DISPLAY__",
                &html_text(match config.role {
                    DesktopRole::Listen => &config.address,
                    DesktopRole::Connect => "",
                }),
            )
            .replace(
                "__CONNECT_TARGET_DISPLAY__",
                &html_text(match config.role {
                    DesktopRole::Listen => "",
                    DesktopRole::Connect => &config.address,
                }),
            )
            .replace("__SETTINGS_ADDRESS__", &html_attr(&config.address));

        let builder = WebViewBuilder::new()
            .with_html(panel_html)
            .with_ipc_handler(move |request| {
                let body = request.body();
                let panel_request = match body.as_str() {
                    "disconnect" => Some(PanelRequest::Disconnect),
                    "reconnect" => Some(PanelRequest::Reconnect),
                    value => value.strip_prefix("save:").and_then(|value| {
                        let (role, address) = value.split_once(':')?;
                        let role = match role {
                            "listen" => DesktopRole::Listen,
                            "connect" => DesktopRole::Connect,
                            _ => return None,
                        };
                        Some(PanelRequest::SaveConfig {
                            role,
                            address: address.to_string(),
                        })
                    }),
                };
                if let Some(panel_request) = panel_request {
                    let _ = request_tx.send(panel_request);
                }
            });

        #[cfg(target_os = "linux")]
        let webview = {
            use tao::platform::unix::WindowExtUnix;
            use wry::WebViewBuilderExtUnix;

            let vbox = window.default_vbox().context("创建设备面板容器失败")?;
            builder
                .build_gtk(vbox)
                .context("创建设备面板 WebView 失败")?
        };

        #[cfg(not(target_os = "linux"))]
        let webview = builder.build(window).context("创建设备面板 WebView 失败")?;

        Ok(Self {
            window,
            webview,
            last_peer_addresses: Vec::new(),
            last_runtime_state: None,
        })
    }

    pub(crate) fn window_id(&self) -> WindowId {
        self.window.id()
    }

    pub(crate) fn show(&self) {
        self.window.set_visible(true);
        self.window.set_focus();
    }

    pub(crate) fn hide(&self) {
        self.window.set_visible(false);
    }

    pub(crate) fn set_peer_addresses(&mut self, addresses: &[String]) {
        if self.last_peer_addresses == addresses {
            return;
        }
        self.last_peer_addresses = addresses.to_vec();
        let _ = self
            .webview
            .evaluate_script(&format!("window.clipxSetPeers({});", js_array(addresses)));
    }

    pub(crate) fn set_runtime_state(&mut self, status: &str, action: Option<&str>) {
        if self
            .last_runtime_state
            .as_ref()
            .is_some_and(|(last_status, last_action)| {
                last_status == status && last_action.as_deref() == action
            })
        {
            return;
        }
        self.last_runtime_state = Some((status.to_string(), action.map(str::to_string)));
        let script = format!(
            "window.clipxSetRuntimeState({}, {});",
            js_string(status),
            action.map_or_else(|| "null".to_string(), js_string),
        );
        let _ = self.webview.evaluate_script(&script);
    }

    pub(crate) fn set_config(&mut self, config: &DesktopConfig) {
        let listen_mode = matches!(config.role, DesktopRole::Listen);
        let script = format!(
            "window.clipxSetConfig({{listenMode:{},listenAddress:{},connectTarget:{},address:{}}});",
            listen_mode,
            js_string(if listen_mode { &config.address } else { "" }),
            js_string(if listen_mode { "" } else { &config.address }),
            js_string(&config.address),
        );
        let _ = self.webview.evaluate_script(&script);
        self.last_peer_addresses.clear();
        self.last_runtime_state = None;
    }

    pub(crate) fn set_message(&self, message: &str) {
        let _ = self
            .webview
            .evaluate_script(&format!("window.clipxSetMessage({});", js_string(message)));
    }
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

fn js_array(values: &[String]) -> String {
    let values = values
        .iter()
        .map(|value| js_string(value))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{values}]")
}

fn html_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn html_attr(value: &str) -> String {
    html_text(value).replace('"', "&quot;")
}

const PANEL_HTML: &str = r#"
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
    main { display: flex; height: 100%; flex-direction: column; padding: 18px 20px 14px; overflow: hidden; }
    .header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 14px; }
    .title { display: flex; align-items: baseline; gap: 7px; }
    .brand { color: var(--muted); font-size: 11px; font-weight: 600; }
    h1 { margin: 0; font-size: 18px; font-weight: 600; }
    .state { display: flex; align-items: center; gap: 6px; color: var(--muted); font-size: 12px; }
    .state .running { color: var(--ink); }
    .dot { width: 6px; height: 6px; border-radius: 50%; background: #34c759; }
    .dot.warn { background: #ff9f0a; }
    .dot.offline { background: #8e8e93; }
    .section { margin: 0; }
    .section + .section { margin-top: 14px; }
    .section-title { margin: 0 0 6px 1px; color: var(--muted); font-size: 11px; font-weight: 600; }
    .section-heading { display: flex; align-items: center; justify-content: space-between; margin-bottom: 6px; }
    .section-heading .section-title { margin-bottom: 0; }
    [hidden] { display: none !important; }
    .panel { overflow: hidden; border: 1px solid var(--line); border-radius: 6px; background: var(--surface); }
    .endpoint { display: grid; grid-template-columns: 92px minmax(0, 1fr); align-items: center; min-height: 36px; padding: 0 11px; }
    .endpoint + .endpoint { border-top: 1px solid var(--line); }
    .peer-list-row { align-items: start; padding-top: 8px; padding-bottom: 8px; }
    .endpoint h2 { margin: 0; color: var(--muted); font-size: 11px; font-weight: 400; }
    .endpoint strong { color: var(--ink); font-size: 13px; font-weight: 500; text-align: right; }
    code, .peer-list { display: block; min-width: 0; overflow: hidden; color: var(--ink); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11px; text-align: right; text-overflow: ellipsis; white-space: nowrap; }
    .peer-list { max-height: 72px; overflow-y: auto; text-overflow: clip; }
    .peer-list.empty, code.empty { color: var(--muted); font-family: inherit; }
    .peer + .peer { margin-top: 4px; }
    .actions { display: flex; gap: 8px; margin-top: 10px; }
    button { height: 32px; padding: 0 12px; border: 1px solid var(--line); border-radius: 5px; background: var(--surface); color: var(--ink); font: inherit; cursor: pointer; }
    button:hover { border-color: var(--accent); }
    button.primary { border-color: var(--accent-dark); background: var(--accent); color: #fff; }
    button.primary:hover { background: var(--accent-dark); }
    button[hidden] { display: none; }
    .settings { padding: 11px; }
    .field + .field { margin-top: 10px; }
    .field-label { display: block; margin-bottom: 5px; color: var(--muted); font-size: 11px; }
    .role-picker { display: grid; grid-template-columns: 1fr 1fr; gap: 6px; }
    .role-picker button { height: 30px; color: var(--muted); }
    .role-picker button.active { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 10%, var(--surface)); color: var(--accent-dark); }
    input { width: 100%; height: 32px; padding: 0 9px; border: 1px solid GrayText; border-radius: 5px; background: Field; color: FieldText; font: inherit; outline: none; }
    input:disabled { opacity: .72; }
    input:focus { border-color: var(--accent); box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 24%, transparent); }
    .settings-actions { display: flex; align-items: center; justify-content: flex-end; gap: 8px; margin-top: 11px; }
    #message { min-height: 14px; margin-right: auto; color: #a15c00; font-size: 11px; }
    @media (prefers-color-scheme: dark) {
      :root {
        --window: #2b2b2d;
        --surface: #363638;
        --ink: #f5f5f7;
        --muted: #a1a1a6;
        --line: rgba(255, 255, 255, .16);
        --accent: #0a84ff;
        --accent-dark: #409cff;
      }
      input { border-color: #77777c; }
    }
  </style>
</head>
<body>
  <main>
    <header class="header">
      <div class="title"><span class="brand">clipx</span><h1>剪贴板同步</h1></div>
      <div class="state"><i id="status-dot" class="dot"></i><span id="status-text" class="running">运行中</span></div>
    </header>

    <section class="section">
      <h2 class="section-title">连接状态</h2>
      <div class="panel">
        <div class="endpoint"><h2>当前连接</h2><strong id="peer-count">0 台</strong></div>
        <div class="endpoint" id="listen-address-row"><h2>监听地址</h2><code id="listen-address">__LISTEN_ADDRESS_DISPLAY__</code></div>
        <div class="endpoint" id="connect-target-row"><h2>连接目标</h2><code id="connect-target">__CONNECT_TARGET_DISPLAY__</code></div>
        <div class="endpoint peer-list-row" id="peer-list-row"><h2>已连接设备</h2><div id="peer-list" class="peer-list empty">暂无设备</div></div>
      </div>
    </section>

    <div class="actions">
      <button id="connection-action" type="button" hidden>断开连接</button>
    </div>

    <section class="section">
      <div class="section-heading">
        <h2 class="section-title">连接设置</h2>
        <button id="edit-settings" type="button">编辑设置</button>
      </div>
      <div class="panel settings">
        <div class="field">
          <span class="field-label">连接方式</span>
          <div class="role-picker" role="group" aria-label="连接方式">
            <button id="listen-role" type="button">监听端</button>
            <button id="connect-role" type="button">连接端</button>
          </div>
        </div>
        <div class="field">
          <label class="field-label" id="address-label" for="address">监听地址</label>
          <input id="address" autocomplete="off" spellcheck="false" value="__SETTINGS_ADDRESS__" disabled>
        </div>
        <div class="settings-actions">
          <span id="message" role="status"></span>
          <button id="cancel-settings" type="button" hidden>取消</button>
          <button id="save-settings" class="primary" type="button" hidden>应用设置</button>
        </div>
      </div>
    </section>
  </main>
  <script>
    document.addEventListener('contextmenu', event => event.preventDefault());

    let listenMode = __LISTEN_MODE__;
    let editing = false;
    const peerCount = document.getElementById('peer-count');
    const listenAddress = document.getElementById('listen-address');
    const connectTarget = document.getElementById('connect-target');
    const listenAddressRow = document.getElementById('listen-address-row');
    const connectTargetRow = document.getElementById('connect-target-row');
    const peerListRow = document.getElementById('peer-list-row');
    const peerList = document.getElementById('peer-list');
    const statusDot = document.getElementById('status-dot');
    const statusText = document.getElementById('status-text');
    const connectionAction = document.getElementById('connection-action');
    const editSettings = document.getElementById('edit-settings');
    const cancelSettings = document.getElementById('cancel-settings');
    const saveSettings = document.getElementById('save-settings');
    const message = document.getElementById('message');
    const address = document.getElementById('address');
    const addressLabel = document.getElementById('address-label');
    const listenRole = document.getElementById('listen-role');
    const connectRole = document.getElementById('connect-role');

    const setRole = role => {
      listenMode = role === 'listen';
      listenRole.classList.toggle('active', listenMode);
      connectRole.classList.toggle('active', !listenMode);
      addressLabel.textContent = listenMode ? '监听地址' : '监听端地址';
      address.placeholder = listenMode ? '0.0.0.0:45876' : '192.168.1.10:45876';
    };

    const selectRole = role => {
      const nextListenMode = role === 'listen';
      if (nextListenMode !== listenMode) address.value = nextListenMode ? '0.0.0.0:45876' : '';
      setRole(role);
    };

    const setEditing = next => {
      editing = next;
      address.disabled = !next;
      listenRole.disabled = !next;
      connectRole.disabled = !next;
      editSettings.hidden = next;
      cancelSettings.hidden = !next;
      saveSettings.hidden = !next;
      message.textContent = '';
      if (next) address.focus();
    };

    listenAddress.classList.toggle('empty', !listenAddress.textContent);
    connectTarget.classList.toggle('empty', !connectTarget.textContent);
    listenAddressRow.hidden = !listenMode;
    connectTargetRow.hidden = listenMode;
    peerListRow.hidden = !listenMode;
    setRole(listenMode ? 'listen' : 'connect');
    setEditing(false);

    listenRole.addEventListener('click', () => { if (editing) selectRole('listen'); });
    connectRole.addEventListener('click', () => { if (editing) selectRole('connect'); });
    editSettings.addEventListener('click', () => setEditing(true));
    cancelSettings.addEventListener('click', () => { window.clipxResetSettings(); });
    saveSettings.addEventListener('click', () => {
      const value = address.value.trim();
      if (!value) {
        message.textContent = '请输入地址和端口';
        address.focus();
        return;
      }
      message.textContent = '正在应用设置';
      window.ipc.postMessage('save:' + (listenMode ? 'listen:' : 'connect:') + value);
    });

    connectionAction.addEventListener('click', () => {
      if (connectionAction.dataset.action) window.ipc.postMessage(connectionAction.dataset.action);
    });

    window.clipxSetRuntimeState = (status, action) => {
      statusText.textContent = status;
      statusDot.className = 'dot' + (status === '已断开' ? ' offline' : status === '正在连接' ? ' warn' : '');
      connectionAction.hidden = listenMode || !action;
      connectionAction.dataset.action = action || '';
      connectionAction.textContent = action === 'reconnect' ? '重新连接' : (status === '正在连接' ? '取消连接' : '断开连接');
    };

    window.clipxSetConfig = config => {
      listenAddress.textContent = config.listenAddress;
      connectTarget.textContent = config.connectTarget;
      listenAddress.classList.toggle('empty', !config.listenAddress);
      connectTarget.classList.toggle('empty', !config.connectTarget);
      listenAddressRow.hidden = !config.listenMode;
      connectTargetRow.hidden = config.listenMode;
      peerListRow.hidden = !config.listenMode;
      address.value = config.address;
      setRole(config.listenMode ? 'listen' : 'connect');
      setEditing(false);
    };

    window.clipxResetSettings = () => {
      address.value = listenMode ? listenAddress.textContent : connectTarget.textContent;
      setRole(listenMode ? 'listen' : 'connect');
      setEditing(false);
    };

    window.clipxSetMessage = value => { message.textContent = value; };

    window.clipxSetPeers = peers => {
      peerCount.textContent = peers.length + ' 台';
      peerList.replaceChildren();
      peerList.classList.toggle('empty', peers.length === 0);
      if (peers.length === 0) {
        peerList.textContent = '暂无设备';
        return;
      }
      peers.forEach(address => {
        const peer = document.createElement('div');
        peer.className = 'peer';
        peer.textContent = address;
        peerList.append(peer);
      });
    };
  </script>
</body>
</html>
"#;
