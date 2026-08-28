use anyhow::{Context, Result, anyhow, bail};
use arboard::{Clipboard, ImageData};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet, VecDeque},
    env,
    fs::{self, File, OpenOptions},
    hash::{Hash, Hasher},
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const FRAME_LIMIT: usize = 4 * 1024 * 1024;
const CHUNK_SIZE: usize = 1024 * 1024;
const MAX_TRANSFER_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const MAX_MANIFEST_ENTRIES: usize = 100_000;
const OUTGOING_QUEUE_SIZE: usize = 8;
const INCOMING_QUEUE_SIZE: usize = 32;
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const CACHE_DIRECTORY_NAME: &str = "clipx-sync";

#[derive(Debug, Clone, Hash, Serialize, Deserialize)]
enum ClipboardPayload {
    Text(String),
    Image {
        width: u32,
        height: u32,
        bytes: Vec<u8>,
    },
}

#[derive(Debug, Clone, Hash, Serialize, Deserialize)]
enum TransferEntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Hash, Serialize, Deserialize)]
struct TransferEntry {
    path: String,
    kind: TransferEntryKind,
    size: u64,
    mode: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum WireMessage {
    Clipboard {
        id: String,
        payload: ClipboardPayload,
    },
    TransferStart {
        id: String,
        fingerprint: u64,
        entries: Vec<TransferEntry>,
    },
    TransferChunk {
        id: String,
        file_index: u32,
        offset: u64,
        bytes: Vec<u8>,
    },
    TransferFileEnd {
        id: String,
        file_index: u32,
        sha256: [u8; 32],
    },
    TransferEnd {
        id: String,
    },
    TransferAbort {
        id: String,
    },
}

struct LocalEntry {
    entry: TransferEntry,
    source: Option<PathBuf>,
    modified_nanos: u128,
}

struct CapturedFiles {
    entries: Vec<LocalEntry>,
    identity: u64,
    internal: bool,
}

enum CapturedPayload {
    Text(ClipboardPayload),
    Image(ClipboardPayload),
    Files(CapturedFiles),
}

#[derive(Clone, Default)]
struct PeerHub {
    peers: Arc<Mutex<Vec<SyncSender<Vec<u8>>>>>,
}

impl PeerHub {
    fn add(&self, sender: SyncSender<Vec<u8>>) {
        self.peers.lock().expect("设备列表锁已损坏").push(sender);
    }

    fn broadcast(&self, frame: &[u8]) -> usize {
        let mut peers = self.peers.lock().expect("设备列表锁已损坏");
        let mut sent = 0;
        peers.retain(|peer| match peer.send(frame.to_vec()) {
            Ok(()) => {
                sent += 1;
                true
            }
            Err(_) => false,
        });
        sent
    }

    fn send_message(&self, message: &WireMessage) -> Result<usize> {
        let frame = bincode::serialize(message).context("序列化剪贴板消息失败")?;
        if frame.is_empty() || frame.len() > FRAME_LIMIT {
            bail!(
                "剪贴板消息过大，无法放入单帧（{}）",
                format_bytes(frame.len() as u64)
            );
        }
        Ok(self.broadcast(&frame))
    }

    fn send_required(&self, message: &WireMessage) -> Result<()> {
        if self.send_message(message)? == 0 {
            bail!("当前没有已连接的设备");
        }
        Ok(())
    }
}

pub fn run(args: &[std::ffi::OsString]) -> Result<()> {
    let mut listen = None;
    let mut peer = None;
    let mut args = args.iter();

    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("-h") | Some("--help") => {
                println!("{}", usage());
                return Ok(());
            }
            Some("--listen") => {
                listen = Some(
                    args.next()
                        .context("--listen 需要地址，例如 0.0.0.0:45876")?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            Some("--peer") => {
                peer = Some(
                    args.next()
                        .context("--peer 需要地址，例如 192.168.1.20:45876")?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            Some(unknown) => bail!("未知的同步参数：{unknown}\n{}", usage()),
            None => bail!("同步参数必须是有效的 UTF-8\n{}", usage()),
        }
    }

    if listen.is_none() && peer.is_none() {
        bail!("同步模式至少需要 --listen 或 --peer\n{}", usage());
    }

    cleanup_stale_cache();

    let hub = PeerHub::default();
    let (incoming_tx, incoming_rx) = mpsc::sync_channel(INCOMING_QUEUE_SIZE);

    if let Some(address) = listen {
        let listener_hub = hub.clone();
        let listener_incoming = incoming_tx.clone();
        thread::spawn(move || listen_loop(&address, listener_hub, listener_incoming));
    }

    if let Some(address) = peer {
        let connector_hub = hub.clone();
        let connector_incoming = incoming_tx;
        thread::spawn(move || connect_loop(&address, connector_hub, connector_incoming));
    }

    eprintln!("剪贴板同步正在运行；未启用认证和加密");
    clipboard_loop(hub, incoming_rx)
}

fn listen_loop(address: &str, hub: PeerHub, incoming: SyncSender<WireMessage>) {
    let listener = match TcpListener::bind(address) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("剪贴板同步：监听 {address} 失败：{error}");
            return;
        }
    };

    eprintln!("剪贴板同步：正在监听 {address}");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                eprintln!("剪贴板同步：设备已连接 {:?}", stream.peer_addr());
                register_connection(stream, hub.clone(), incoming.clone());
            }
            Err(error) => eprintln!("剪贴板同步：接受连接失败：{error}"),
        }
    }
}

fn connect_loop(address: &str, hub: PeerHub, incoming: SyncSender<WireMessage>) {
    loop {
        match TcpStream::connect(address) {
            Ok(stream) => {
                eprintln!("剪贴板同步：已连接到 {address}");
                let alive = register_connection(stream, hub.clone(), incoming.clone());
                while alive.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(500));
                }
                eprintln!("剪贴板同步：与 {address} 的连接已关闭");
            }
            Err(error) => eprintln!("剪贴板同步：连接 {address} 失败：{error}"),
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn register_connection(
    stream: TcpStream,
    hub: PeerHub,
    incoming: SyncSender<WireMessage>,
) -> Arc<AtomicBool> {
    let (outgoing_tx, outgoing_rx) = mpsc::sync_channel::<Vec<u8>>(OUTGOING_QUEUE_SIZE);
    hub.add(outgoing_tx.clone());

    let alive = Arc::new(AtomicBool::new(true));
    let writer_alive = alive.clone();
    let reader_alive = alive.clone();
    let mut writer = stream.try_clone().expect("克隆设备连接失败");
    let mut reader = stream;

    thread::spawn(move || {
        while let Ok(frame) = outgoing_rx.recv() {
            if frame.is_empty() {
                break;
            }
            if let Err(error) = write_frame(&mut writer, &frame) {
                eprintln!("剪贴板同步：发送数据失败：{error}");
                break;
            }
        }
        writer_alive.store(false, Ordering::Relaxed);
        let _ = writer.shutdown(Shutdown::Both);
    });

    thread::spawn(move || {
        loop {
            let frame = match read_frame(&mut reader) {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) => {
                    eprintln!("剪贴板同步：读取设备数据失败：{error}");
                    break;
                }
            };

            match bincode::deserialize::<WireMessage>(&frame) {
                Ok(message) => {
                    if incoming.send(message).is_err() {
                        break;
                    }
                }
                Err(error) => eprintln!("剪贴板同步：收到无效消息：{error}"),
            }
        }

        let _ = outgoing_tx.send(Vec::new());
        reader_alive.store(false, Ordering::Relaxed);
        let _ = reader.shutdown(Shutdown::Both);
    });

    alive
}

fn write_frame(stream: &mut TcpStream, frame: &[u8]) -> io::Result<()> {
    if frame.is_empty() || frame.len() > FRAME_LIMIT || frame.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "无效的数据帧大小",
        ));
    }

    stream.write_all(&(frame.len() as u32).to_be_bytes())?;
    stream.write_all(frame)?;
    stream.flush()
}

fn read_frame(stream: &mut TcpStream) -> io::Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    match stream.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }

    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > FRAME_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "收到无效的数据帧大小",
        ));
    }

    let mut frame = vec![0u8; length];
    stream.read_exact(&mut frame)?;
    Ok(Some(frame))
}

fn clipboard_loop(hub: PeerHub, incoming: Receiver<WireMessage>) -> Result<()> {
    let mut clipboard = Clipboard::new().context("访问系统剪贴板失败")?;
    let origin = format!("{}-{}", std::process::id(), timestamp_nanos());
    let mut sequence = 0u64;
    let mut last_fingerprint = None;
    let mut last_capture_error = None;
    let mut last_attempted_fingerprint = None;
    let mut last_send_error = None;
    let mut seen = SeenIds::default();
    let mut active_transfers = HashMap::<String, IncomingTransfer>::new();

    loop {
        while let Ok(message) = incoming.try_recv() {
            match message {
                WireMessage::Clipboard { id, payload } => {
                    if !seen.insert(&id) {
                        continue;
                    }
                    let fingerprint = fingerprint(&payload);
                    let summary = payload_summary(&payload);
                    eprintln!("剪贴板同步：收到 {summary}");
                    match apply_payload(&mut clipboard, &payload) {
                        Ok(()) => {
                            last_fingerprint = Some(fingerprint);
                            eprintln!("剪贴板同步：已应用 {summary}");
                        }
                        Err(error) => eprintln!("剪贴板同步：应用远端剪贴板失败：{error:#}"),
                    }
                }
                WireMessage::TransferStart {
                    id,
                    fingerprint: _,
                    entries,
                } => {
                    if !seen.insert(&id) {
                        continue;
                    }
                    match IncomingTransfer::start(id.clone(), entries) {
                        Ok(transfer) => {
                            eprintln!("剪贴板同步：开始接收 {}", transfer.summary());
                            active_transfers.insert(id, transfer);
                        }
                        Err(error) => eprintln!("剪贴板同步：拒绝文件传输：{error:#}"),
                    }
                }
                WireMessage::TransferChunk {
                    id,
                    file_index,
                    offset,
                    bytes,
                } => {
                    let result = active_transfers
                        .get_mut(&id)
                        .ok_or_else(|| anyhow!("找不到文件传输 {}", id))
                        .and_then(|transfer| transfer.accept_chunk(file_index, offset, &bytes));
                    if let Err(error) = result {
                        eprintln!("剪贴板同步：接收文件分块失败：{error:#}");
                        if let Some(transfer) = active_transfers.remove(&id) {
                            transfer.cleanup();
                        }
                    }
                }
                WireMessage::TransferFileEnd {
                    id,
                    file_index,
                    sha256,
                } => {
                    let result = active_transfers
                        .get_mut(&id)
                        .ok_or_else(|| anyhow!("找不到文件传输 {}", id))
                        .and_then(|transfer| transfer.finish_file(file_index, sha256));
                    if let Err(error) = result {
                        eprintln!("剪贴板同步：文件校验失败：{error:#}");
                        if let Some(transfer) = active_transfers.remove(&id) {
                            transfer.cleanup();
                        }
                    }
                }
                WireMessage::TransferEnd { id } => {
                    let Some(transfer) = active_transfers.remove(&id) else {
                        eprintln!("剪贴板同步：找不到要结束的文件传输 {id}");
                        continue;
                    };
                    let summary = transfer.summary();
                    match transfer.finish() {
                        Ok(finished) => {
                            if let Err(error) = set_file_list(&mut clipboard, &finished.roots) {
                                eprintln!("剪贴板同步：写入接收文件列表失败：{error:#}");
                                finished.cleanup();
                            } else {
                                last_fingerprint = Some(finished.fingerprint);
                                last_attempted_fingerprint = None;
                                last_send_error = None;
                                eprintln!("剪贴板同步：已完成 {summary}");
                            }
                        }
                        Err(error) => eprintln!("剪贴板同步：完成文件传输失败：{error:#}"),
                    }
                }
                WireMessage::TransferAbort { id } => {
                    if let Some(transfer) = active_transfers.remove(&id) {
                        transfer.cleanup();
                        eprintln!("剪贴板同步：文件传输已中止 {id}");
                    }
                }
            }
        }

        match capture_payload(&mut clipboard) {
            Ok(payload) => {
                last_capture_error = None;
                let current_fingerprint = captured_fingerprint(&payload);
                if last_fingerprint != Some(current_fingerprint) {
                    sequence += 1;
                    let id = format!("{origin}-{sequence}");
                    let announce_attempt = last_attempted_fingerprint != Some(current_fingerprint);
                    last_attempted_fingerprint = Some(current_fingerprint);
                    let mut ignored_internal = false;
                    let sent = match payload {
                        CapturedPayload::Text(payload) | CapturedPayload::Image(payload) => {
                            let summary = payload_summary(&payload);
                            if announce_attempt {
                                eprintln!("剪贴板同步：正在发送 {summary}");
                            }
                            if let Err(error) =
                                hub.send_required(&WireMessage::Clipboard { id, payload })
                            {
                                let message = format!("{error:#}");
                                if last_send_error.as_deref() != Some(message.as_str()) {
                                    eprintln!("剪贴板同步：发送剪贴板失败：{message}");
                                }
                                last_send_error = Some(message);
                                false
                            } else {
                                true
                            }
                        }
                        CapturedPayload::Files(files) => {
                            if files.internal {
                                ignored_internal = true;
                                eprintln!("剪贴板同步：忽略内部临时路径，避免同步回环");
                                true
                            } else {
                                if announce_attempt {
                                    eprintln!("剪贴板同步：正在发送 {}", files_summary(&files));
                                }
                                if let Err(error) = send_file_transfer(&hub, &id, &files) {
                                    let message = format!("{error:#}");
                                    if last_send_error.as_deref() != Some(message.as_str()) {
                                        eprintln!("剪贴板同步：发送文件失败：{message}");
                                    }
                                    last_send_error = Some(message);
                                    false
                                } else {
                                    true
                                }
                            }
                        }
                    };
                    if sent {
                        // 只有完整发送成功后才记住指纹，断线期间的复制内容会自动重试。
                        last_fingerprint = Some(current_fingerprint);
                        last_attempted_fingerprint = None;
                        last_send_error = None;
                        if !ignored_internal {
                            eprintln!("剪贴板同步：发送完成");
                        }
                    }
                }
            }
            Err(error) => {
                let message = format!("{error:#}");
                if last_capture_error.as_deref() != Some(message.as_str()) {
                    eprintln!("剪贴板同步：读取本地剪贴板失败：{message}");
                    last_capture_error = Some(message);
                }
            }
        }

        thread::sleep(POLL_INTERVAL);
    }
}

fn capture_payload(clipboard: &mut Clipboard) -> Result<CapturedPayload> {
    if let Ok(paths) = clipboard.get().file_list()
        && !paths.is_empty()
    {
        return Ok(CapturedPayload::Files(capture_files(paths)?));
    }

    if let Ok(image) = clipboard.get_image() {
        let width = u32::try_from(image.width).context("剪贴板图片宽度过大")?;
        let height = u32::try_from(image.height).context("剪贴板图片高度过大")?;
        return Ok(CapturedPayload::Image(ClipboardPayload::Image {
            width,
            height,
            bytes: image.bytes.into_owned(),
        }));
    }

    if let Ok(text) = clipboard.get_text()
        && !text.is_empty()
    {
        return Ok(CapturedPayload::Text(ClipboardPayload::Text(text)));
    }

    bail!("剪贴板为空或格式暂不支持")
}

fn capture_files(paths: Vec<PathBuf>) -> Result<CapturedFiles> {
    let internal = paths.iter().any(|path| is_internal_cache_path(path));
    let mut entries = Vec::new();
    let mut names = HashSet::new();

    for path in paths {
        let root_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("无法读取复制项目的文件名")?;
        collect_entry(&path, Path::new(root_name), &mut entries, &mut names)?;
    }

    if entries.is_empty() {
        bail!("复制项目为空")
    }
    if entries.len() > MAX_MANIFEST_ENTRIES {
        bail!("目录项目数量超过限制（最多 {} 项）", MAX_MANIFEST_ENTRIES)
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for local in &entries {
        local.entry.hash(&mut hasher);
        local.modified_nanos.hash(&mut hasher);
    }

    Ok(CapturedFiles {
        entries,
        identity: hasher.finish(),
        internal,
    })
}

fn is_internal_cache_path(path: &Path) -> bool {
    let cache_root = env::temp_dir().join(CACHE_DIRECTORY_NAME);
    if path.starts_with(&cache_root) {
        return true;
    }

    // macOS 可能把 /var 规范化为 /private/var，比较规范化后的路径才能识别缓存。
    let Some(canonical_path) = path.canonicalize().ok() else {
        return false;
    };
    let Some(canonical_root) = cache_root.canonicalize().ok() else {
        return false;
    };
    canonical_path.starts_with(canonical_root)
}

fn manifest_identity(directory: &Path, entries: &[TransferEntry]) -> Result<u64> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for entry in entries {
        entry.hash(&mut hasher);
        let path = directory.join(safe_relative_path(&entry.path)?);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("读取接收文件元数据失败：{}", path.display()))?;
        modified_nanos(&metadata).hash(&mut hasher);
    }
    Ok(hasher.finish())
}

fn collect_entry(
    source: &Path,
    relative: &Path,
    entries: &mut Vec<LocalEntry>,
    names: &mut HashSet<String>,
) -> Result<()> {
    if entries.len() >= MAX_MANIFEST_ENTRIES {
        bail!("目录项目数量超过限制（最多 {} 项）", MAX_MANIFEST_ENTRIES)
    }

    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("读取复制项目失败：{}", source.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("暂不支持符号链接：{}", source.display())
    }

    let relative = portable_relative_path(relative)?;
    if !names.insert(relative.clone()) {
        bail!("复制项目存在重名路径：{relative}")
    }

    let (kind, size, child_source) = if metadata.is_dir() {
        (TransferEntryKind::Directory, 0, None)
    } else if metadata.is_file() {
        (
            TransferEntryKind::File,
            metadata.len(),
            Some(source.to_path_buf()),
        )
    } else {
        bail!("暂不支持特殊文件：{}", source.display())
    };

    let local = LocalEntry {
        entry: TransferEntry {
            path: relative.clone(),
            kind: kind.clone(),
            size,
            mode: file_mode(&metadata),
        },
        source: child_source,
        modified_nanos: modified_nanos(&metadata),
    };
    entries.push(local);

    if matches!(kind, TransferEntryKind::Directory) {
        let mut children = fs::read_dir(source)
            .with_context(|| format!("读取目录失败：{}", source.display()))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .with_context(|| format!("读取目录内容失败：{}", source.display()))?;
        children.sort_by_key(|child| child.file_name());
        for child in children {
            collect_entry(
                &child.path(),
                &Path::new(&relative).join(child.file_name()),
                entries,
                names,
            )?;
        }
    }

    Ok(())
}

fn send_file_transfer(hub: &PeerHub, id: &str, files: &CapturedFiles) -> Result<()> {
    let result = send_file_transfer_inner(hub, id, files);
    if result.is_err() {
        let _ = hub.send_message(&WireMessage::TransferAbort { id: id.to_string() });
    }
    result
}

fn send_file_transfer_inner(hub: &PeerHub, id: &str, files: &CapturedFiles) -> Result<()> {
    let entries = files
        .entries
        .iter()
        .map(|local| local.entry.clone())
        .collect::<Vec<_>>();
    let total_bytes = entries.iter().try_fold(0u64, |total, entry| {
        total.checked_add(entry.size).context("文件总大小溢出")
    })?;
    if total_bytes > MAX_TRANSFER_BYTES {
        bail!(
            "文件总大小超过限制（最多 {} GiB）",
            MAX_TRANSFER_BYTES / (1024 * 1024 * 1024)
        )
    }

    hub.send_required(&WireMessage::TransferStart {
        id: id.to_string(),
        fingerprint: files.identity,
        entries,
    })?;

    let mut file_index = 0u32;
    let mut buffer = vec![0u8; CHUNK_SIZE];
    for local in &files.entries {
        if !matches!(local.entry.kind, TransferEntryKind::File) {
            continue;
        }

        let source = local.source.as_ref().context("文件来源路径丢失")?;
        eprintln!("剪贴板同步：正在读取文件 {}", local.entry.path);
        let mut file =
            File::open(source).with_context(|| format!("打开文件失败：{}", source.display()))?;
        let mut hasher = Sha256::new();
        let mut offset = 0u64;

        loop {
            let length = file
                .read(&mut buffer)
                .with_context(|| format!("读取文件失败：{}", source.display()))?;
            if length == 0 {
                break;
            }

            let length_u64 = u64::try_from(length).expect("分块大小应可转换为 u64");
            let next_offset = offset.checked_add(length_u64).context("文件大小溢出")?;
            if next_offset > local.entry.size {
                bail!("文件在传输过程中变大：{}", source.display())
            }
            hasher.update(&buffer[..length]);
            hub.send_required(&WireMessage::TransferChunk {
                id: id.to_string(),
                file_index,
                offset,
                bytes: buffer[..length].to_vec(),
            })?;
            offset = next_offset;
        }

        if offset != local.entry.size {
            bail!("文件在传输过程中发生变化：{}", source.display())
        }
        let sha256: [u8; 32] = hasher.finalize().into();
        hub.send_required(&WireMessage::TransferFileEnd {
            id: id.to_string(),
            file_index,
            sha256,
        })?;
        file_index = file_index.checked_add(1).context("文件数量溢出")?;
    }

    hub.send_required(&WireMessage::TransferEnd { id: id.to_string() })
}

fn apply_payload(clipboard: &mut Clipboard, payload: &ClipboardPayload) -> Result<()> {
    match payload {
        ClipboardPayload::Text(text) => clipboard.set_text(text).context("写入文本失败")?,
        ClipboardPayload::Image {
            width,
            height,
            bytes,
        } => clipboard
            .set_image(ImageData {
                width: *width as usize,
                height: *height as usize,
                bytes: Cow::Borrowed(bytes),
            })
            .context("写入图片失败")?,
    }
    Ok(())
}

fn set_file_list(clipboard: &mut Clipboard, paths: &[PathBuf]) -> Result<()> {
    #[cfg(windows)]
    clipboard.clear().context("清空 Windows 剪贴板失败")?;
    clipboard
        .set()
        .file_list(paths)
        .context("写入文件列表失败")?;
    Ok(())
}

struct IncomingTransfer {
    directory: PathBuf,
    parts_directory: PathBuf,
    entries: Vec<TransferEntry>,
    files: Vec<IncomingFile>,
    roots: Vec<PathBuf>,
}

struct IncomingFile {
    final_path: PathBuf,
    temporary_path: PathBuf,
    expected_size: u64,
    received_size: u64,
    hasher: Sha256,
    file: Option<File>,
    finished: bool,
}

struct FinishedTransfer {
    fingerprint: u64,
    roots: Vec<PathBuf>,
    directory: PathBuf,
}

impl IncomingTransfer {
    fn start(id: String, entries: Vec<TransferEntry>) -> Result<Self> {
        if entries.is_empty() {
            bail!("传输清单为空")
        }
        if entries.len() > MAX_MANIFEST_ENTRIES {
            bail!("传输清单项目数量超过限制")
        }
        if id.is_empty() {
            bail!("传输标识不能为空")
        }

        let total_bytes = entries.iter().try_fold(0u64, |total, entry| {
            total
                .checked_add(entry.size)
                .context("传输清单文件大小溢出")
        })?;
        if total_bytes > MAX_TRANSFER_BYTES {
            bail!("传输文件总大小超过限制")
        }

        let directory = env::temp_dir()
            .join(CACHE_DIRECTORY_NAME)
            .join(transfer_component(&id));
        let parts_directory = directory.join(".parts");
        fs::create_dir_all(&parts_directory)
            .with_context(|| format!("创建临时传输目录失败：{}", directory.display()))?;

        let result = Self::start_inner(directory.clone(), parts_directory, entries);
        if result.is_err() {
            let _ = fs::remove_dir_all(&directory);
        }
        result
    }

    fn start_inner(
        directory: PathBuf,
        parts_directory: PathBuf,
        entries: Vec<TransferEntry>,
    ) -> Result<Self> {
        let mut seen_paths = HashSet::new();
        let mut roots = Vec::new();
        let mut files = Vec::new();

        for (entry_index, entry) in entries.iter().enumerate() {
            let relative = safe_relative_path(&entry.path)?;
            let relative_text = relative.to_string_lossy().into_owned();
            if !seen_paths.insert(relative_text) {
                bail!("传输清单存在重复路径：{}", entry.path)
            }
            let final_path = directory.join(&relative);
            if relative.components().count() == 1 {
                roots.push(final_path.clone());
            }

            match entry.kind {
                TransferEntryKind::Directory => {
                    if entry.size != 0 {
                        bail!("目录大小必须为 0：{}", entry.path)
                    }
                    fs::create_dir_all(&final_path)
                        .with_context(|| format!("创建目录失败：{}", final_path.display()))?;
                }
                TransferEntryKind::File => {
                    let parent = final_path.parent().context("接收文件缺少父目录")?;
                    fs::create_dir_all(parent)
                        .with_context(|| format!("创建文件父目录失败：{}", parent.display()))?;
                    let temporary_path = parts_directory.join(format!("{entry_index}.part"));
                    files.push(IncomingFile {
                        final_path,
                        temporary_path,
                        expected_size: entry.size,
                        received_size: 0,
                        hasher: Sha256::new(),
                        file: None,
                        finished: false,
                    });
                }
            }
        }

        if roots.is_empty() {
            bail!("传输清单没有顶层路径")
        }

        Ok(Self {
            directory,
            parts_directory,
            entries,
            files,
            roots,
        })
    }

    fn accept_chunk(&mut self, file_index: u32, offset: u64, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() || bytes.len() > CHUNK_SIZE {
            bail!("收到的文件分块大小无效")
        }
        let file = self
            .files
            .get_mut(file_index as usize)
            .context("文件分块索引无效")?;
        if file.finished {
            bail!("文件已经结束，不能继续写入")
        }
        if offset != file.received_size {
            bail!(
                "文件分块偏移不连续：期望 {}，实际 {}",
                file.received_size,
                offset
            )
        }
        let length = u64::try_from(bytes.len()).expect("分块大小应可转换为 u64");
        let next_size = file
            .received_size
            .checked_add(length)
            .context("接收文件大小溢出")?;
        if next_size > file.expected_size {
            bail!("接收文件大小超过清单声明")
        }

        if file.file.is_none() {
            file.file = Some(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&file.temporary_path)
                    .with_context(|| {
                        format!("创建临时文件失败：{}", file.temporary_path.display())
                    })?,
            );
        }
        let handle = file.file.as_mut().context("接收文件句柄已关闭")?;
        handle.write_all(bytes).context("写入文件分块失败")?;
        file.hasher.update(bytes);
        file.received_size = next_size;
        Ok(())
    }

    fn finish_file(&mut self, file_index: u32, expected_sha256: [u8; 32]) -> Result<()> {
        let file = self
            .files
            .get_mut(file_index as usize)
            .context("文件结束索引无效")?;
        if file.finished {
            bail!("文件已经结束")
        }
        if file.received_size != file.expected_size {
            bail!(
                "文件大小不匹配：期望 {}，实际 {}",
                file.expected_size,
                file.received_size
            )
        }

        let actual_sha256: [u8; 32] = file.hasher.clone().finalize().into();
        if actual_sha256 != expected_sha256 {
            bail!("SHA-256 校验不匹配")
        }

        let mut handle = match file.file.take() {
            Some(handle) => handle,
            None => OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file.temporary_path)
                .with_context(|| format!("创建空文件失败：{}", file.temporary_path.display()))?,
        };
        handle.flush().context("刷新临时文件失败")?;
        handle.sync_all().context("同步临时文件失败")?;
        drop(handle);
        fs::rename(&file.temporary_path, &file.final_path).with_context(|| {
            format!(
                "将临时文件重命名为最终文件失败：{}",
                file.final_path.display()
            )
        })?;
        file.finished = true;
        Ok(())
    }

    fn finish(mut self) -> Result<FinishedTransfer> {
        let result = self.finish_inner();
        if result.is_err() {
            self.cleanup();
        }
        result
    }

    fn finish_inner(&mut self) -> Result<FinishedTransfer> {
        if self.files.iter().any(|file| !file.finished) {
            bail!("仍有文件没有完成校验")
        }
        // 先读取元数据计算指纹，再恢复目录权限，避免无权进入目录时无法完成收尾。
        let local_fingerprint = manifest_identity(&self.directory, &self.entries)?;
        for entry in self.entries.iter().rev() {
            let path = self.directory.join(safe_relative_path(&entry.path)?);
            apply_mode(&path, entry.mode)?;
        }
        if self.parts_directory.exists() {
            fs::remove_dir_all(&self.parts_directory).with_context(|| {
                format!("清理临时分块目录失败：{}", self.parts_directory.display())
            })?;
        }
        Ok(FinishedTransfer {
            fingerprint: local_fingerprint,
            roots: self.roots.clone(),
            directory: self.directory.clone(),
        })
    }

    fn summary(&self) -> String {
        let file_count = self.files.len();
        let directory_count = self
            .entries
            .iter()
            .filter(|entry| matches!(entry.kind, TransferEntryKind::Directory))
            .count();
        let total_bytes = self.entries.iter().map(|entry| entry.size).sum::<u64>();
        format!(
            "{} 个文件、{} 个目录（{}）",
            file_count,
            directory_count,
            format_bytes(total_bytes)
        )
    }

    fn cleanup(self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

impl FinishedTransfer {
    fn cleanup(self) {
        let _ = fs::remove_dir_all(self.directory);
    }
}

fn files_summary(files: &CapturedFiles) -> String {
    let file_count = files
        .entries
        .iter()
        .filter(|entry| matches!(entry.entry.kind, TransferEntryKind::File))
        .count();
    let directory_count = files
        .entries
        .iter()
        .filter(|entry| matches!(entry.entry.kind, TransferEntryKind::Directory))
        .count();
    let total_bytes = files
        .entries
        .iter()
        .map(|entry| entry.entry.size)
        .sum::<u64>();
    format!(
        "{} 个文件、{} 个目录（{}）",
        file_count,
        directory_count,
        format_bytes(total_bytes)
    )
}

fn payload_summary(payload: &ClipboardPayload) -> String {
    match payload {
        ClipboardPayload::Text(text) => format!("文本（{}）", format_bytes(text.len() as u64)),
        ClipboardPayload::Image {
            width,
            height,
            bytes,
        } => format!(
            "图片 {width}x{height}（{}）",
            format_bytes(bytes.len() as u64)
        ),
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit_index = 0;

    while value >= 1000.0 && unit_index < UNITS.len() - 1 {
        value /= 1000.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        return format!("{bytes} B");
    }

    let formatted = format!("{value:.2}");
    let formatted = formatted.trim_end_matches('0').trim_end_matches('.');
    format!("{formatted} {}", UNITS[unit_index])
}

fn captured_fingerprint(payload: &CapturedPayload) -> u64 {
    match payload {
        CapturedPayload::Text(payload) | CapturedPayload::Image(payload) => fingerprint(payload),
        CapturedPayload::Files(files) => files.identity,
    }
}

fn fingerprint(payload: &ClipboardPayload) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    payload.hash(&mut hasher);
    hasher.finish()
}

fn portable_relative_path(path: &Path) -> Result<String> {
    let mut components = Vec::new();
    for component in path.components() {
        let component = component.as_os_str().to_string_lossy();
        if component.is_empty() || component == "." || component == ".." {
            bail!("复制路径包含无效组件")
        }
        components.push(portable_component(&component));
    }
    if components.is_empty() {
        bail!("复制路径为空")
    }
    Ok(components.join("/"))
}

fn safe_relative_path(path: &str) -> Result<PathBuf> {
    let normalized = path.replace('\\', "/");
    let mut result = PathBuf::new();
    for component in normalized.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            bail!("传输路径包含无效组件：{path}")
        }
        result.push(portable_component(component));
    }
    if result.as_os_str().is_empty() {
        bail!("传输路径为空")
    }
    Ok(result)
}

fn portable_component(value: &str) -> String {
    let mut result: String = value
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect();
    while result.ends_with('.') || result.ends_with(' ') {
        result.pop();
    }
    if result.is_empty() {
        result.push('_');
    }
    let reserved = matches!(
        result
            .trim_end_matches(['.', ' '])
            .to_ascii_uppercase()
            .as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if reserved {
        result.push('_');
    }
    result
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn transfer_component(value: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{}-{:016x}", safe_component(value), hasher.finish())
}

fn cleanup_stale_cache() {
    let directory = env::temp_dir().join(CACHE_DIRECTORY_NAME);
    let Ok(entries) = fs::read_dir(&directory) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .and_then(|modified| now.duration_since(modified).map_err(std::io::Error::other))
            .map(|age| age >= CACHE_MAX_AGE)
            .unwrap_or(false);
        if is_old && fs::remove_dir_all(&path).is_ok() {
            eprintln!("剪贴板同步：已清理过期临时目录 {}", path.display());
        }
    }
}

fn modified_nanos(metadata: &fs::Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> Option<u32> {
    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn file_mode(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn apply_mode(path: &Path, mode: Option<u32>) -> Result<()> {
    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .with_context(|| format!("恢复文件权限失败：{}", path.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_mode(_path: &Path, _mode: Option<u32>) -> Result<()> {
    Ok(())
}

fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[derive(Default)]
struct SeenIds {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl SeenIds {
    fn insert(&mut self, id: &str) -> bool {
        if !self.set.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        if self.order.len() > 256
            && let Some(oldest) = self.order.pop_front()
        {
            self.set.remove(&oldest);
        }
        true
    }
}

fn usage() -> &'static str {
    "用法：clipx sync --listen <地址> [--peer <地址>]"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_rejects_traversal() {
        assert!(safe_relative_path("../secret.txt").is_err());
        assert!(safe_relative_path("folder/../../secret.txt").is_err());
        assert_eq!(
            safe_relative_path("folder\\document.txt").unwrap(),
            PathBuf::from("folder/document.txt")
        );
    }

    #[test]
    fn directory_manifest_preserves_relative_hierarchy() {
        let root = env::temp_dir().join(format!("clipx-sync-test-{}", timestamp_nanos()));
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("top.txt"), b"top").unwrap();
        fs::write(root.join("nested").join("child.txt"), b"child").unwrap();

        let captured = capture_files(vec![root.clone()]).unwrap();
        let paths = captured
            .entries
            .iter()
            .map(|entry| entry.entry.path.as_str())
            .collect::<Vec<_>>();
        assert!(paths.iter().any(|path| path.ends_with("/top.txt")));
        assert!(paths.iter().any(|path| path.ends_with("/nested/child.txt")));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_chunks_verify_and_commit_atomically() {
        let id = format!("test-{}", timestamp_nanos());
        let mode = file_mode(&fs::metadata(env::temp_dir()).unwrap());
        let entry = TransferEntry {
            path: "sample.txt".to_string(),
            kind: TransferEntryKind::File,
            size: 11,
            mode,
        };
        let mut transfer = IncomingTransfer::start(id, vec![entry]).unwrap();
        transfer.accept_chunk(0, 0, b"hello world").unwrap();
        let sha256: [u8; 32] = Sha256::digest(b"hello world").into();
        transfer.finish_file(0, sha256).unwrap();
        let finished = transfer.finish().unwrap();
        assert_eq!(fs::read(&finished.roots[0]).unwrap(), b"hello world");
        assert_eq!(
            finished.fingerprint,
            capture_files(finished.roots.clone()).unwrap().identity
        );
        finished.cleanup();
    }

    #[test]
    fn empty_directory_finishes_transfer() {
        let id = format!("empty-dir-{}", timestamp_nanos());
        let entry = TransferEntry {
            path: "empty".to_string(),
            kind: TransferEntryKind::Directory,
            size: 0,
            mode: None,
        };
        let transfer = IncomingTransfer::start(id, vec![entry]).unwrap();
        let finished = transfer.finish().unwrap();
        assert!(finished.roots[0].is_dir());
        finished.cleanup();
    }

    #[test]
    fn byte_format_uses_readable_decimal_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1_000), "1 KB");
        assert_eq!(format_bytes(5_485_145), "5.49 MB");
        assert_eq!(format_bytes(1_000_000_000), "1 GB");
    }
}
