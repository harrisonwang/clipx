use crate::cli::SyncOptions;
use anyhow::{Context, Result, bail};
use std::{
    env,
    fs::{self, File},
    io::Write,
    net::SocketAddr,
    path::PathBuf,
};

const CONFIG_FILE_NAME: &str = "config";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DesktopRole {
    Listen,
    Connect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DesktopConfig {
    pub(crate) role: DesktopRole,
    pub(crate) address: String,
}

impl DesktopConfig {
    pub(crate) fn from_form(role: DesktopRole, address: &str) -> Result<Self> {
        let address = address.trim();
        if address.is_empty() || address.contains(['\n', '\r', '=']) {
            bail!("请输入有效的设备地址和端口")
        }
        match role {
            DesktopRole::Listen => {
                address
                    .parse::<SocketAddr>()
                    .with_context(|| format!("无效的监听地址：{address}"))?;
            }
            DesktopRole::Connect => validate_connect_address(address)?,
        }
        Ok(Self {
            role,
            address: address.to_string(),
        })
    }

    pub(crate) fn sync_options(&self) -> SyncOptions {
        match self.role {
            DesktopRole::Listen => SyncOptions {
                listen: Some(self.address.parse().expect("监听地址应已验证")),
                connect: None,
                tray: true,
            },
            DesktopRole::Connect => SyncOptions {
                listen: None,
                connect: Some(self.address.clone()),
                tray: true,
            },
        }
    }

    fn encode(&self) -> String {
        let role = match self.role {
            DesktopRole::Listen => "listen",
            DesktopRole::Connect => "connect",
        };
        format!("role={role}\naddress={}\n", self.address)
    }

    fn decode(contents: &str) -> Result<Self> {
        let mut role = None;
        let mut address = None;
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                bail!("配置文件格式无效")
            };
            match key {
                "role" => {
                    role = Some(match value {
                        "listen" => DesktopRole::Listen,
                        "connect" => DesktopRole::Connect,
                        _ => bail!("配置文件中的角色无效"),
                    });
                }
                "address" => address = Some(value.to_string()),
                _ => bail!("配置文件包含未知字段：{key}"),
            }
        }

        let role = role.context("配置文件缺少 role")?;
        let address = address.context("配置文件缺少 address")?;
        Self::from_form(role, &address)
    }
}

pub(crate) fn load() -> Result<Option<DesktopConfig>> {
    let path = config_path()?;
    match fs::read_to_string(&path) {
        Ok(contents) => DesktopConfig::decode(&contents)
            .map(Some)
            .with_context(|| format!("读取配置文件失败：{}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("读取配置文件失败：{}", path.display())),
    }
}

pub(crate) fn save(config: &DesktopConfig) -> Result<()> {
    let path = config_path()?;
    let parent = path.parent().context("配置文件路径无效")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("创建配置目录失败：{}", parent.display()))?;

    let temp_path = path.with_extension("tmp");
    let mut file = File::create(&temp_path)
        .with_context(|| format!("创建临时配置文件失败：{}", temp_path.display()))?;
    file.write_all(config.encode().as_bytes())
        .context("写入配置文件失败")?;
    file.sync_all().context("同步配置文件失败")?;
    drop(file);
    fs::rename(&temp_path, &path).with_context(|| format!("保存配置文件失败：{}", path.display()))
}

fn config_path() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    let base = env::var_os("APPDATA").map(PathBuf::from);

    #[cfg(target_os = "macos")]
    let base = env::var_os("HOME")
        .map(PathBuf::from)
        .map(|path| path.join("Library").join("Application Support"));

    #[cfg(all(unix, not(target_os = "macos")))]
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));

    base.map(|path| path.join("clipx").join(CONFIG_FILE_NAME))
        .context("无法确定用户配置目录")
}

fn validate_connect_address(address: &str) -> Result<()> {
    let Some((host, port)) = address.rsplit_once(':') else {
        bail!("连接地址必须包含端口，例如 192.168.1.10:45876")
    };
    if host.is_empty() || port.parse::<u16>().is_err() {
        bail!("连接地址必须包含有效的主机和端口")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip_preserves_listen_role() {
        let config = DesktopConfig::from_form(DesktopRole::Listen, "0.0.0.0:45876").unwrap();
        assert_eq!(DesktopConfig::decode(&config.encode()).unwrap(), config);
    }

    #[test]
    fn config_round_trip_preserves_connect_hostname() {
        let config = DesktopConfig::from_form(DesktopRole::Connect, "mac.local:45876").unwrap();
        assert_eq!(DesktopConfig::decode(&config.encode()).unwrap(), config);
    }

    #[test]
    fn config_rejects_invalid_addresses() {
        assert!(DesktopConfig::from_form(DesktopRole::Listen, "localhost").is_err());
        assert!(DesktopConfig::from_form(DesktopRole::Connect, "localhost").is_err());
        assert!(DesktopConfig::from_form(DesktopRole::Connect, "host:not-a-port").is_err());
    }
}
