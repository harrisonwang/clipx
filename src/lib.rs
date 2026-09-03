mod cli;
#[cfg(feature = "gui")]
mod config;
#[cfg(feature = "gui")]
mod desktop;
#[cfg(feature = "gui")]
mod gui;
mod sync;
#[cfg(feature = "tray")]
mod tray;

use anyhow::{Context, Result, bail};
use arboard::Clipboard;
use std::{env, path::PathBuf};

pub fn run_cli() -> Result<()> {
    match cli::parse(env::args_os().skip(1))? {
        cli::Command::Help => {
            println!("{}", cli::usage());
            Ok(())
        }
        cli::Command::Version => {
            println!("clipx {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        cli::Command::Copy(paths) => copy_paths(paths),
        cli::Command::Sync(options) => {
            if options.tray {
                #[cfg(feature = "tray")]
                {
                    return tray::run(options);
                }
                #[cfg(not(feature = "tray"))]
                {
                    bail!("当前构建未启用托盘支持")
                }
            }
            sync::run(options)
        }
    }
}

#[cfg(feature = "gui")]
pub fn run_desktop() -> Result<()> {
    desktop::run()
}

fn copy_paths(paths: Vec<PathBuf>) -> Result<()> {
    for path in &paths {
        if !path.exists() {
            bail!("路径不存在：{}", path.display());
        }
        if !path.is_file() && !path.is_dir() {
            bail!("暂不支持此类路径：{}", path.display());
        }
    }

    let mut clipboard = Clipboard::new().context("访问系统剪贴板失败")?;
    clipboard
        .set()
        .file_list(&paths)
        .context("复制路径到系统剪贴板失败")?;

    if paths.len() == 1 {
        println!("已复制路径 {}", paths[0].display());
    } else {
        println!("已复制 {} 个路径", paths.len());
    }
    Ok(())
}
