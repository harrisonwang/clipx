use crate::cli::SyncOptions;
use anyhow::{Context, Result, anyhow, bail};
use arboard::Clipboard;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const FRAME_LIMIT: usize = 4 * 1024 * 1024;
const CHUNK_SIZE: usize = 1024 * 1024;
const MAX_TRANSFER_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const MAX_MANIFEST_ENTRIES: usize = 100_000;
const OUTGOING_QUEUE_SIZE: usize = 8;
const INCOMING_QUEUE_SIZE: usize = 32;
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const PEER_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const PEER_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const CACHE_DIRECTORY_NAME: &str = "clipx-sync";
const PROTOCOL_VERSION: u16 = 3;

mod files;
mod image;
mod transport;

use files::{
    CapturedFiles, IncomingTransfer, TransferEntry, capture_files, cleanup_stale_cache,
    files_summary, send_file_transfer,
};
use image::{
    CachedImage, CapturedImage, IncomingImage, apply_image, capture_image, send_image_transfer,
};
use transport::{PeerHub, connect_loop, listen_loop};

#[derive(Debug, Clone, Hash, Serialize, Deserialize)]
enum ClipboardPayload {
    Text(String),
    Image {
        width: u32,
        height: u32,
        bytes: Vec<u8>,
    },
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
    Hello {
        version: u16,
    },
    ImageStart {
        id: String,
        fingerprint: u64,
        width: u32,
        height: u32,
        size: u64,
    },
    ImageChunk {
        id: String,
        offset: u64,
        bytes: Vec<u8>,
    },
    ImageEnd {
        id: String,
        sha256: [u8; 32],
    },
}

enum SendRequest {
    Text {
        id: String,
        fingerprint: u64,
        payload: ClipboardPayload,
    },
    Files {
        id: String,
        fingerprint: u64,
        files: Arc<CapturedFiles>,
    },
    Image {
        id: String,
        fingerprint: u64,
        image: CapturedImage,
    },
}

struct SendResult {
    fingerprint: u64,
    result: Result<()>,
}

enum CapturedPayload {
    Text(ClipboardPayload),
    Image(CapturedImage),
    Files(Arc<CapturedFiles>),
}

#[derive(Default)]
struct CaptureCache {
    files: Option<(Vec<PathBuf>, Arc<CapturedFiles>)>,
    image: Option<CachedImage>,
    last_file_error: Option<String>,
}

struct SyncState {
    origin: String,
    sequence: u64,
    last_fingerprint: Option<u64>,
    last_capture_error: Option<String>,
    last_attempted_fingerprint: Option<u64>,
    last_send_error: Option<String>,
    pending_send_fingerprint: Option<u64>,
    observed_fingerprint: Option<u64>,
    seen: SeenIds,
    active_transfers: HashMap<String, IncomingTransfer>,
    active_images: HashMap<String, IncomingImage>,
}

impl SyncState {
    fn new() -> Self {
        Self {
            origin: format!("{}-{}", std::process::id(), timestamp_nanos()),
            sequence: 0,
            last_fingerprint: None,
            last_capture_error: None,
            last_attempted_fingerprint: None,
            last_send_error: None,
            pending_send_fingerprint: None,
            observed_fingerprint: None,
            seen: SeenIds::default(),
            active_transfers: HashMap::new(),
            active_images: HashMap::new(),
        }
    }

    fn next_id(&mut self) -> String {
        self.sequence += 1;
        format!("{}-{}", self.origin, self.sequence)
    }
}

pub fn run(options: SyncOptions) -> Result<()> {
    cleanup_stale_cache();

    let hub = PeerHub::default();
    let (incoming_tx, incoming_rx) = mpsc::sync_channel(INCOMING_QUEUE_SIZE);

    if let Some(address) = options.listen {
        let listener_hub = hub.clone();
        let listener_incoming = incoming_tx.clone();
        thread::spawn(move || listen_loop(&address, listener_hub, listener_incoming));
    }

    for address in options.connect {
        let connector_hub = hub.clone();
        let connector_incoming = incoming_tx.clone();
        thread::spawn(move || connect_loop(&address, connector_hub, connector_incoming));
    }
    drop(incoming_tx);

    let (send_tx, send_rx) = mpsc::sync_channel::<SendRequest>(1);
    let (send_result_tx, send_result_rx) = mpsc::channel::<SendResult>();
    let send_worker_hub = hub.clone();
    thread::spawn(move || send_loop(send_worker_hub, send_rx, send_result_tx));

    eprintln!(
        "剪贴板同步正在运行（clipx {}，协议 v{}）；未启用认证和加密",
        env!("CARGO_PKG_VERSION"),
        PROTOCOL_VERSION
    );
    clipboard_loop(incoming_rx, send_tx, send_result_rx)
}

fn send_loop(hub: PeerHub, requests: Receiver<SendRequest>, results: mpsc::Sender<SendResult>) {
    while let Ok(request) = requests.recv() {
        let (fingerprint, result) = match request {
            SendRequest::Text {
                id,
                fingerprint,
                payload,
            } => {
                let result = hub.send_required(&WireMessage::Clipboard { id, payload });
                (fingerprint, result)
            }
            SendRequest::Files {
                id,
                fingerprint,
                files,
            } => {
                let result = send_file_transfer(&hub, &id, &files);
                (fingerprint, result)
            }
            SendRequest::Image {
                id,
                fingerprint,
                image,
            } => {
                let result = send_image_transfer(&hub, &id, fingerprint, &image);
                (fingerprint, result)
            }
        };
        let _ = results.send(SendResult {
            fingerprint,
            result,
        });
    }
}

fn clipboard_loop(
    incoming: Receiver<WireMessage>,
    send_tx: SyncSender<SendRequest>,
    send_result_rx: Receiver<SendResult>,
) -> Result<()> {
    let mut clipboard = Clipboard::new().context("访问系统剪贴板失败")?;
    let mut state = SyncState::new();
    let mut cache = CaptureCache::default();

    loop {
        while let Ok(result) = send_result_rx.try_recv() {
            if state.pending_send_fingerprint == Some(result.fingerprint) {
                state.pending_send_fingerprint = None;
            }
            match result.result {
                Ok(()) => {
                    if state.observed_fingerprint == Some(result.fingerprint) {
                        state.last_fingerprint = Some(result.fingerprint);
                        state.last_attempted_fingerprint = None;
                        state.last_send_error = None;
                        eprintln!("剪贴板同步：发送完成");
                    }
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    if state.last_send_error.as_deref() != Some(message.as_str()) {
                        eprintln!("剪贴板同步：发送失败：{message}");
                    }
                    state.last_send_error = Some(message);
                }
            }
        }

        while let Ok(message) = incoming.try_recv() {
            match message {
                WireMessage::Hello { .. } => {
                    eprintln!("剪贴板同步：握手消息不应进入剪贴板处理队列");
                }
                WireMessage::Clipboard { id, payload } => {
                    if !state.seen.insert(&id) {
                        continue;
                    }
                    let fingerprint = fingerprint(&payload);
                    let summary = payload_summary(&payload);
                    eprintln!("剪贴板同步：收到 {summary}");
                    match apply_payload(&mut clipboard, &payload) {
                        Ok(()) => {
                            state.last_fingerprint = Some(fingerprint);
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
                    if !state.seen.insert(&id) {
                        continue;
                    }
                    match IncomingTransfer::start(id.clone(), entries) {
                        Ok(transfer) => {
                            eprintln!("剪贴板同步：开始接收 {}", transfer.summary());
                            state.active_transfers.insert(id, transfer);
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
                    let result = state
                        .active_transfers
                        .get_mut(&id)
                        .ok_or_else(|| anyhow!("找不到文件传输 {id}"))
                        .and_then(|transfer| transfer.accept_chunk(file_index, offset, &bytes));
                    if let Err(error) = result {
                        eprintln!("剪贴板同步：接收文件分块失败：{error:#}");
                        if let Some(transfer) = state.active_transfers.remove(&id) {
                            transfer.cleanup();
                        }
                    }
                }
                WireMessage::TransferFileEnd {
                    id,
                    file_index,
                    sha256,
                } => {
                    let result = state
                        .active_transfers
                        .get_mut(&id)
                        .ok_or_else(|| anyhow!("找不到文件传输 {id}"))
                        .and_then(|transfer| transfer.finish_file(file_index, sha256));
                    if let Err(error) = result {
                        eprintln!("剪贴板同步：文件校验失败：{error:#}");
                        if let Some(transfer) = state.active_transfers.remove(&id) {
                            transfer.cleanup();
                        }
                    }
                }
                WireMessage::TransferEnd { id } => {
                    let Some(transfer) = state.active_transfers.remove(&id) else {
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
                                state.last_fingerprint = Some(finished.fingerprint);
                                state.last_attempted_fingerprint = None;
                                state.last_send_error = None;
                                eprintln!("剪贴板同步：已完成 {summary}");
                            }
                        }
                        Err(error) => eprintln!("剪贴板同步：完成文件传输失败：{error:#}"),
                    }
                }
                WireMessage::TransferAbort { id } => {
                    if let Some(transfer) = state.active_transfers.remove(&id) {
                        transfer.cleanup();
                        eprintln!("剪贴板同步：文件传输已中止 {id}");
                    }
                    if state.active_images.remove(&id).is_some() {
                        eprintln!("剪贴板同步：图片传输已中止 {id}");
                    }
                }
                WireMessage::ImageStart {
                    id,
                    fingerprint,
                    width,
                    height,
                    size,
                } => {
                    if !state.seen.insert(&id) {
                        continue;
                    }
                    match IncomingImage::start(fingerprint, width, height, size) {
                        Ok(image) => {
                            eprintln!(
                                "剪贴板同步：开始接收图片 {width}x{height}（{}）",
                                format_bytes(size)
                            );
                            state.active_images.insert(id, image);
                        }
                        Err(error) => eprintln!("剪贴板同步：拒绝图片传输：{error:#}"),
                    }
                }
                WireMessage::ImageChunk { id, offset, bytes } => {
                    let result = state
                        .active_images
                        .get_mut(&id)
                        .ok_or_else(|| anyhow!("找不到图片传输 {id}"))
                        .and_then(|image| image.accept_chunk(offset, &bytes));
                    if let Err(error) = result {
                        eprintln!("剪贴板同步：接收图片分块失败：{error:#}");
                        state.active_images.remove(&id);
                    }
                }
                WireMessage::ImageEnd { id, sha256 } => {
                    let Some(image) = state.active_images.remove(&id) else {
                        eprintln!("剪贴板同步：找不到要结束的图片传输 {id}");
                        continue;
                    };
                    match image.finish(sha256) {
                        Ok((fingerprint, payload)) => {
                            let summary = payload_summary(&payload);
                            match apply_payload(&mut clipboard, &payload) {
                                Ok(()) => {
                                    state.last_fingerprint = Some(fingerprint);
                                    state.last_attempted_fingerprint = None;
                                    state.last_send_error = None;
                                    eprintln!("剪贴板同步：已应用 {summary}");
                                }
                                Err(error) => {
                                    eprintln!("剪贴板同步：应用远端图片失败：{error:#}")
                                }
                            }
                        }
                        Err(error) => eprintln!("剪贴板同步：完成图片传输失败：{error:#}"),
                    }
                }
            }
        }

        match capture_payload(&mut clipboard, &mut cache) {
            Ok(payload) => {
                state.last_capture_error = None;
                let current_fingerprint = captured_fingerprint(&payload);
                state.observed_fingerprint = Some(current_fingerprint);
                if state.last_fingerprint != Some(current_fingerprint) {
                    let id = state.next_id();
                    let announce_attempt =
                        state.last_attempted_fingerprint != Some(current_fingerprint);
                    state.last_attempted_fingerprint = Some(current_fingerprint);
                    let mut ignored_internal = false;
                    let sent = match payload {
                        CapturedPayload::Text(payload) => {
                            let summary = payload_summary(&payload);
                            if announce_attempt {
                                eprintln!("剪贴板同步：正在发送 {summary}");
                            }
                            match send_tx.try_send(SendRequest::Text {
                                id,
                                fingerprint: current_fingerprint,
                                payload,
                            }) {
                                Ok(()) => {
                                    state.pending_send_fingerprint = Some(current_fingerprint);
                                    false
                                }
                                Err(TrySendError::Full(_)) => false,
                                Err(TrySendError::Disconnected(_)) => {
                                    eprintln!("剪贴板同步：发送线程已退出");
                                    false
                                }
                            }
                        }
                        CapturedPayload::Image(image) => {
                            let summary = image.summary();
                            if announce_attempt {
                                eprintln!("剪贴板同步：正在发送 {summary}");
                            }
                            match send_tx.try_send(SendRequest::Image {
                                id,
                                fingerprint: current_fingerprint,
                                image,
                            }) {
                                Ok(()) => {
                                    state.pending_send_fingerprint = Some(current_fingerprint);
                                    false
                                }
                                Err(TrySendError::Full(_)) => false,
                                Err(TrySendError::Disconnected(_)) => {
                                    eprintln!("剪贴板同步：发送线程已退出");
                                    false
                                }
                            }
                        }
                        CapturedPayload::Files(files) => {
                            if files.internal {
                                ignored_internal = true;
                                eprintln!("剪贴板同步：忽略内部临时路径，避免同步回环");
                                true
                            } else if state.pending_send_fingerprint == Some(current_fingerprint) {
                                false
                            } else {
                                if announce_attempt {
                                    eprintln!("剪贴板同步：正在发送 {}", files_summary(&files));
                                }
                                match send_tx.try_send(SendRequest::Files {
                                    id,
                                    fingerprint: current_fingerprint,
                                    files,
                                }) {
                                    Ok(()) => {
                                        state.pending_send_fingerprint = Some(current_fingerprint);
                                        state.last_attempted_fingerprint =
                                            Some(current_fingerprint);
                                        false
                                    }
                                    Err(TrySendError::Full(_)) => false,
                                    Err(TrySendError::Disconnected(_)) => {
                                        state.last_send_error =
                                            Some("文件发送线程已退出".to_string());
                                        eprintln!("剪贴板同步：文件发送线程已退出");
                                        false
                                    }
                                }
                            }
                        }
                    };
                    if sent {
                        // 只有完整发送成功后才记住指纹，断线期间的复制内容会自动重试。
                        state.last_fingerprint = Some(current_fingerprint);
                        state.last_attempted_fingerprint = None;
                        state.last_send_error = None;
                        if !ignored_internal {
                            eprintln!("剪贴板同步：发送完成");
                        }
                    }
                }
            }
            Err(error) => {
                let message = format!("{error:#}");
                if state.last_capture_error.as_deref() != Some(message.as_str()) {
                    eprintln!("剪贴板同步：读取本地剪贴板失败：{message}");
                    state.last_capture_error = Some(message);
                }
            }
        }

        thread::sleep(POLL_INTERVAL);
    }
}

fn capture_payload(clipboard: &mut Clipboard, cache: &mut CaptureCache) -> Result<CapturedPayload> {
    if let Ok(paths) = clipboard.get().file_list()
        && !paths.is_empty()
    {
        if let Some((cached_paths, files)) = &cache.files
            && *cached_paths == paths
        {
            return Ok(CapturedPayload::Files(files.clone()));
        }
        match capture_files(paths.clone()) {
            Ok(files) => {
                let files = Arc::new(files);
                cache.files = Some((paths, files.clone()));
                cache.last_file_error = None;
                return Ok(CapturedPayload::Files(files));
            }
            Err(error) => {
                // 文件管理器可能在复制后删除或移动源文件。失效路径不应阻塞文本和图片同步。
                let message = format!("{error:#}");
                if cache.last_file_error.as_deref() != Some(message.as_str()) {
                    eprintln!("剪贴板同步：忽略失效的文件列表：{message}");
                    cache.last_file_error = Some(message);
                }
            }
        }
    }
    cache.files = None;

    if let Some(image) = capture_image(clipboard, &mut cache.image)? {
        return Ok(CapturedPayload::Image(image));
    }

    if let Ok(text) = clipboard.get_text()
        && !text.is_empty()
    {
        cache.last_file_error = None;
        return Ok(CapturedPayload::Text(ClipboardPayload::Text(text)));
    }

    bail!("剪贴板为空或格式暂不支持")
}

fn apply_payload(clipboard: &mut Clipboard, payload: &ClipboardPayload) -> Result<()> {
    match payload {
        ClipboardPayload::Text(text) => clipboard.set_text(text).context("写入文本失败")?,
        ClipboardPayload::Image {
            width,
            height,
            bytes,
        } => apply_image(clipboard, *width, *height, bytes)?,
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
        CapturedPayload::Text(payload) => fingerprint(payload),
        CapturedPayload::Image(image) => image.fingerprint(),
        CapturedPayload::Files(files) => files.identity,
    }
}

fn fingerprint(payload: &ClipboardPayload) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    payload.hash(&mut hasher);
    hasher.finish()
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

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    #[cfg(unix)]
    use std::path::Path;
    use std::path::PathBuf;
    use std::{env, fs};

    #[cfg(unix)]
    use super::files::portable_relative_path;
    use super::files::{TransferEntryKind, file_mode, path_collision_key, safe_relative_path};

    #[test]
    fn relative_path_rejects_traversal() {
        assert!(safe_relative_path(b"../secret.txt").is_err());
        assert!(safe_relative_path(b"folder/../../secret.txt").is_err());
        assert_eq!(
            safe_relative_path(b"folder\\document.txt").unwrap(),
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
            .map(|entry| entry.entry.path.as_slice())
            .collect::<Vec<_>>();
        assert!(paths.iter().any(|path| path.ends_with(b"/top.txt")));
        assert!(
            paths
                .iter()
                .any(|path| path.ends_with(b"/nested/child.txt"))
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_chunks_verify_and_commit_atomically() {
        let id = format!("test-{}", timestamp_nanos());
        let mode = file_mode(&fs::metadata(env::temp_dir()).unwrap());
        let entry = TransferEntry {
            path: b"sample.txt".to_vec(),
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
            path: b"empty".to_vec(),
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

    #[cfg(unix)]
    #[test]
    fn unix_non_utf8_filename_is_preserved_in_manifest() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let name = std::ffi::OsString::from_vec(vec![b'n', b'a', 0x80, b'm', b'e']);
        let relative = portable_relative_path(Path::new(&name)).unwrap();
        assert_eq!(relative, vec![b'n', b'a', 0x80, b'm', b'e']);
        let reconstructed = safe_relative_path(&relative).unwrap();
        assert_eq!(reconstructed.as_os_str().as_bytes(), relative);
    }

    #[test]
    fn path_collision_key_rejects_case_variants() {
        assert_eq!(
            path_collision_key(b"Readme.txt"),
            path_collision_key(b"README.TXT")
        );
    }

    #[test]
    fn incoming_transfer_rejects_case_collisions() {
        let entries = vec![
            TransferEntry {
                path: b"Readme.txt".to_vec(),
                kind: TransferEntryKind::File,
                size: 0,
                mode: None,
            },
            TransferEntry {
                path: b"README.TXT".to_vec(),
                kind: TransferEntryKind::File,
                size: 0,
                mode: None,
            },
        ];
        assert!(IncomingTransfer::start("case-collision".to_string(), entries).is_err());
    }

    #[test]
    fn incoming_transfer_rejects_file_parent_conflict() {
        let entries = vec![
            TransferEntry {
                path: b"folder".to_vec(),
                kind: TransferEntryKind::File,
                size: 0,
                mode: None,
            },
            TransferEntry {
                path: b"folder/nested.txt".to_vec(),
                kind: TransferEntryKind::File,
                size: 0,
                mode: None,
            },
        ];
        assert!(IncomingTransfer::start("file-parent-conflict".to_string(), entries).is_err());
    }

    #[test]
    fn sync_state_generates_unique_message_ids() {
        let mut state = SyncState::new();
        let first = state.next_id();
        let second = state.next_id();
        assert_ne!(first, second);
        assert!(second.ends_with("-2"));
    }
}
