#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod app;
mod platform;
mod tray;

use anyhow::{Context, Result, bail};
use zipover_core::SyncOptions;

fn main() {
    if let Err(error) = run() {
        eprintln!("Zipover：{error:#}");
        platform::show_error(&format!("{error:#}"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut options = SyncOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("zipover {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                println!("zipover-gui [--listen IP:端口] [--connect IP:端口]");
                return Ok(());
            }
            "--listen" => options.listen = args.next().context("缺少监听地址")?.parse()?,
            "--connect" => options.connect = Some(args.next().context("缺少连接地址")?),
            _ => bail!("未知参数：{arg}"),
        }
    }
    app::run(options)
}
