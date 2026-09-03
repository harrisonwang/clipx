use anyhow::{Context, Result, bail};
use std::{ffi::OsString, net::SocketAddr, path::PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Help,
    Version,
    Copy(Vec<PathBuf>),
    Sync(SyncOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncOptions {
    pub(crate) listen: Option<SocketAddr>,
    pub(crate) connect: Option<String>,
    pub(crate) tray: bool,
}

pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command> {
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return Ok(Command::Help);
    };

    match command.to_str() {
        Some("-h") | Some("--help") | Some("help") => Ok(Command::Help),
        Some("-V") | Some("--version") => Ok(Command::Version),
        Some("copy") => parse_copy(args),
        Some("sync") => parse_sync(args),
        Some(unknown) => bail!("未知命令：{unknown}\n\n{}", usage()),
        None => bail!("命令必须是有效的 UTF-8\n\n{}", usage()),
    }
}

fn parse_copy(args: impl Iterator<Item = OsString>) -> Result<Command> {
    let mut paths = args.collect::<Vec<_>>();
    if paths.len() == 1 && is_help(&paths[0]) {
        return Ok(Command::Help);
    }
    if paths.first().is_some_and(|argument| argument == "--") {
        paths.remove(0);
    }
    if paths.is_empty() {
        bail!("copy 至少需要一个文件或目录路径\n\n{}", usage())
    }
    Ok(Command::Copy(
        paths.into_iter().map(PathBuf::from).collect(),
    ))
}

fn parse_sync(mut args: impl Iterator<Item = OsString>) -> Result<Command> {
    let mut listen = None;
    let mut connect = None;
    let mut tray = false;

    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("-h") | Some("--help") => return Ok(Command::Help),
            Some("--listen") => {
                if listen.is_some() {
                    bail!("--listen 只能指定一次")
                }
                let value = args
                    .next()
                    .context("--listen 需要地址，例如 0.0.0.0:45876")?;
                let value = value.to_str().context("--listen 地址必须是有效的 UTF-8")?;
                listen = Some(
                    value
                        .parse::<SocketAddr>()
                        .with_context(|| format!("无效的监听地址：{value}"))?,
                );
            }
            Some("--connect") => {
                if connect.is_some() {
                    bail!("--connect 只能指定一次")
                }
                let value = args
                    .next()
                    .context("--connect 需要地址，例如 192.168.1.20:45876")?;
                let value = value
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("--connect 地址必须是有效的 UTF-8"))?;
                if value.is_empty() {
                    bail!("--connect 地址不能为空")
                }
                connect = Some(value);
            }
            Some("--tray") => {
                tray = true;
            }
            Some(unknown) => bail!("未知的同步参数：{unknown}\n\n{}", usage()),
            None => bail!("同步参数必须是有效的 UTF-8\n\n{}", usage()),
        }
    }

    if listen.is_none() && connect.is_none() {
        bail!("sync 至少需要 --listen 或 --connect\n\n{}", usage())
    }
    if listen.is_some() && connect.is_some() {
        bail!("--listen 和 --connect 不能同时使用")
    }
    Ok(Command::Sync(SyncOptions {
        listen,
        connect,
        tray,
    }))
}

fn is_help(argument: &OsString) -> bool {
    matches!(argument.to_str(), Some("-h" | "--help"))
}

pub(crate) fn usage() -> &'static str {
    "用法：\n  clipx copy <文件或目录路径>...\n  clipx sync [--tray] [--listen <地址>] [--connect <地址>]...\n  clipx --help\n  clipx --version"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn empty_arguments_show_help() {
        assert_eq!(parse(Vec::new()).unwrap(), Command::Help);
    }

    #[test]
    fn copy_accepts_multiple_paths() {
        let command = parse(strings(&["copy", "report.pdf", "documents"])).unwrap();
        assert_eq!(
            command,
            Command::Copy(vec![
                PathBuf::from("report.pdf"),
                PathBuf::from("documents")
            ])
        );
    }

    #[test]
    fn sync_accepts_single_connect_target() {
        let command = parse(strings(&["sync", "--connect", "mac.local:45876"])).unwrap();
        assert_eq!(
            command,
            Command::Sync(SyncOptions {
                listen: None,
                connect: Some("mac.local:45876".to_string()),
                tray: false,
            })
        );
    }

    #[test]
    fn duplicate_connect_is_rejected() {
        assert!(
            parse(strings(&[
                "sync",
                "--connect",
                "mac.local:45876",
                "--connect",
                "192.168.1.20:45876",
            ]))
            .is_err()
        );
    }

    #[test]
    fn listen_and_connect_are_rejected_together() {
        assert!(
            parse(strings(&[
                "sync",
                "--listen",
                "0.0.0.0:45876",
                "--connect",
                "192.168.1.20:45876",
            ]))
            .is_err()
        );
    }

    #[test]
    fn sync_accepts_tray_mode() {
        let command = parse(strings(&[
            "sync",
            "--tray",
            "--connect",
            "192.168.1.20:45876",
        ]))
        .unwrap();
        assert_eq!(
            command,
            Command::Sync(SyncOptions {
                listen: None,
                connect: Some("192.168.1.20:45876".to_string()),
                tray: true,
            })
        );
    }

    #[test]
    fn duplicate_listen_is_rejected() {
        assert!(
            parse(strings(&[
                "sync",
                "--listen",
                "0.0.0.0:45876",
                "--listen",
                "127.0.0.1:45876",
            ]))
            .is_err()
        );
    }

    #[test]
    fn malformed_listen_address_is_rejected() {
        assert!(parse(strings(&["sync", "--listen", "localhost"])).is_err());
    }

    #[test]
    fn unknown_command_is_not_treated_as_a_path() {
        assert!(parse(strings(&["status"])).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn copy_preserves_non_utf8_paths() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let path = OsString::from_vec(vec![b'n', b'a', 0x80, b'm', b'e']);
        let command = parse(vec![OsString::from("copy"), path]).unwrap();
        let Command::Copy(paths) = command else {
            panic!("应解析为 copy 命令");
        };
        assert_eq!(paths[0].as_os_str().as_bytes(), b"na\x80me");
    }
}
