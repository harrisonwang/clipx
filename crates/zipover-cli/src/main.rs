mod args;

use anyhow::{Context, Result, bail};
use std::{env, thread, time::Duration};
use zipover_core::{SyncOptions, SyncRuntime};

fn main() {
    if let Err(error) = run() {
        eprintln!("zipover：{error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match args::parse(env::args_os().skip(1))? {
        args::Command::Help => println!("{}", args::usage()),
        args::Command::Version => println!("zipover {}", env!("CARGO_PKG_VERSION")),
        args::Command::Copy(paths) => {
            let count = paths.len();
            zipover_core::copy_paths(paths)?;
            println!("已复制 {count} 个路径");
        }
        args::Command::Devices => {
            for peer in zipover_core::paired_devices()? {
                println!("{}  {}", hex::encode(peer.id), peer.name);
            }
        }
        args::Command::Forget(id) => {
            let mut device = [0; 16];
            hex::decode_to_slice(id, &mut device).context("设备 ID 无效")?;
            zipover_core::forget_device(device)?;
            println!("已解除配对");
        }
        args::Command::Run {
            code,
            device,
            options,
            tray,
        } => {
            if tray {
                let cli = env::current_exe()?;
                let name = cli
                    .file_name()
                    .and_then(|name| name.to_str())
                    .context("程序路径无效")?;
                let suffix =
                    name.strip_prefix("zipover")
                        .unwrap_or(if cfg!(windows) { ".exe" } else { "" });
                let executable = cli.with_file_name(format!("zipover-gui{suffix}"));
                #[cfg(target_os = "macos")]
                let executable = {
                    let app = cli
                        .with_file_name("Zipover.app")
                        .join("Contents/MacOS/zipover-gui");
                    if app.is_file() { app } else { executable }
                };
                let mut command = std::process::Command::new(&executable);
                #[cfg(target_os = "macos")]
                if !executable.is_file() {
                    // Cask installs the app separately from the CLI's Cellar directory.
                    command = std::process::Command::new("open");
                    command.args(["-W", "-b", "com.harrisonwang.zipover", "--args"]);
                }
                command.arg("--listen").arg(options.listen.to_string());
                if let Some(address) = options.connect {
                    command.arg("--connect").arg(address);
                }
                if !command
                    .status()
                    .context("请安装桌面版，或将 zipover-gui 放在 zipover 同一目录后重试")?
                    .success()
                {
                    bail!("桌面程序启动失败");
                }
            } else {
                sync(options, code, device)?;
            }
        }
    }
    Ok(())
}

fn sync(options: SyncOptions, code: Option<String>, device: Option<String>) -> Result<()> {
    let runtime = SyncRuntime::start(options)?;
    if let Some(code) = code {
        runtime.join(&code, device)?;
        eprintln!("正在配对…");
        wait_for_pairing(&runtime)?;
    } else if runtime.snapshot().devices.is_empty() {
        runtime.generate_code()?;
        wait_for_pairing(&runtime)?;
    }
    eprintln!("同步已启动。按 Ctrl+C 退出。");
    runtime.wait()
}

fn wait_for_pairing(runtime: &SyncRuntime) -> Result<()> {
    let mut shown_code = None;
    loop {
        let state = runtime.snapshot();
        if state.paired {
            runtime.cancel_pairing();
            return Ok(());
        }
        if let Some(code) = state.code.as_deref()
            && shown_code.as_deref() != Some(code)
        {
            let heading = if shown_code.is_some() {
                "配对码已更新"
            } else {
                "配对码"
            };
            println!("{heading}：{code}\n在另一台设备运行：zipover --pair {code}");
            if shown_code.is_none() {
                eprintln!("配对码两分钟内有效。配对成功后自动开始同步。");
            }
            shown_code = Some(code.to_owned());
        }
        if let Some(error) = state.error {
            bail!("{error}");
        }
        thread::sleep(Duration::from_millis(100));
    }
}
