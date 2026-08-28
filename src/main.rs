use anyhow::{Context, Result, bail};
use arboard::Clipboard;
use std::{env, path::PathBuf};

fn main() {
    if let Err(error) = run() {
        eprintln!("copy: {error:#}");
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
            println!("copy {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }

    let path = PathBuf::from(argument);

    if args.next().is_some() {
        bail!("{}", usage());
    }

    if !path.is_file() {
        bail!("file does not exist: {}", path.display());
    }

    let mut clipboard = Clipboard::new().context("failed to access the system clipboard")?;

    clipboard
        .set()
        .file_list(std::slice::from_ref(&path))
        .context("failed to copy file to the system clipboard")?;

    println!("Copied file {}", path.display());

    Ok(())
}

fn usage() -> &'static str {
    "usage: copy <path>"
}
