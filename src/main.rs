mod sync;

use anyhow::{Context, Result, bail};
use arboard::Clipboard;
use std::{env, ffi::OsString, path::PathBuf};

fn main() {
    if let Err(error) = run() {
        eprintln!("clipx：{error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = env::args_os();
    let _program = args.next();
    let argument = args.next().context(usage())?;

    match argument.to_str() {
        Some("-h") | Some("--help") => {
            println!("{}", usage());
            return Ok(());
        }
        Some("-V") | Some("--version") => {
            println!("clipx {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("sync") | Some("--sync") => {
            let options = args.collect::<Vec<OsString>>();
            return sync::run(&options);
        }
        _ => {}
    }

    let path = PathBuf::from(argument);

    if args.next().is_some() {
        bail!("{}", usage());
    }

    if !path.exists() {
        bail!("路径不存在：{}", path.display());
    }
    if !path.is_file() && !path.is_dir() {
        bail!("暂不支持此类路径：{}", path.display());
    }

    let mut clipboard = Clipboard::new().context("访问系统剪贴板失败")?;

    clipboard
        .set()
        .file_list(std::slice::from_ref(&path))
        .context("复制路径到系统剪贴板失败")?;

    println!("已复制路径 {}", path.display());

    Ok(())
}

fn usage() -> &'static str {
    "用法：\n  clipx <文件或目录路径>\n  clipx sync --listen <地址> [--peer <地址>]"
}
