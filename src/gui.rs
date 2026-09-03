use anyhow::{Context, Result};
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
}

impl PairingPanel {
    pub(crate) fn new(
        event_loop: &EventLoopWindowTarget<()>,
        listen_address: Option<String>,
        connect_target: Option<String>,
    ) -> Result<Self> {
        let listen_mode = listen_address.is_some();
        let window = Box::new(
            WindowBuilder::new()
                .with_title("clipx")
                .with_inner_size(LogicalSize::new(
                    420.0,
                    if listen_mode { 300.0 } else { 240.0 },
                ))
                .with_resizable(false)
                .with_visible(false)
                .build(event_loop)
                .context("创建设备面板窗口失败")?,
        );
        let window: &'static Window = Box::leak(window);

        let listen_address = listen_address.unwrap_or_default();
        let connect_target = connect_target.unwrap_or_default();
        let panel_html = PANEL_HTML
            .replace(
                "__LISTEN_MODE__",
                if listen_mode { "true" } else { "false" },
            )
            .replace("__LISTEN_ADDRESS_DISPLAY__", &html_text(&listen_address))
            .replace("__CONNECT_TARGET_DISPLAY__", &html_text(&connect_target));

        let builder = WebViewBuilder::new().with_html(panel_html);

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
    .section { margin: 0; }
    .section + .section { margin-top: 14px; }
    .section-title { margin: 0 0 6px 1px; color: var(--muted); font-size: 11px; font-weight: 600; }
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
      <div class="state"><i class="dot"></i><span class="running">运行中</span></div>
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
  </main>
  <script>
    document.addEventListener('contextmenu', event => event.preventDefault());

    const listenMode = __LISTEN_MODE__;
    const peerCount = document.getElementById('peer-count');
    const listenAddress = document.getElementById('listen-address');
    const listenAddressRow = document.getElementById('listen-address-row');
    const connectTargetRow = document.getElementById('connect-target-row');
    const peerListRow = document.getElementById('peer-list-row');
    const peerList = document.getElementById('peer-list');

    listenAddress.classList.toggle('empty', !listenAddress.textContent);
    document.getElementById('connect-target').classList.toggle('empty', !'__CONNECT_TARGET_DISPLAY__');
    listenAddressRow.hidden = !listenMode;
    connectTargetRow.hidden = listenMode;
    peerListRow.hidden = !listenMode;

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
