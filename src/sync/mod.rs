use crate::cli::SyncOptions;
use anyhow::{Context, Result, bail};
use arboard::Clipboard;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::{Hash, Hasher},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const FRAME_LIMIT: usize = 4 * 1024 * 1024;
const CHUNK_SIZE: usize = 1024 * 1024;
const MAX_TRANSFER_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const MAX_MANIFEST_ENTRIES: usize = 100_000;
const INCOMING_QUEUE_SIZE: usize = 32;
const MAX_PEERS: usize = 32;
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const PEER_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const PEER_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_MESSAGE_ID_LEN: usize = 256;
const MAX_ACTIVE_TRANSFERS: usize = 16;
const MAX_ACTIVE_TRANSFER_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_ACTIVE_IMAGES: usize = 4;
const MAX_ACTIVE_IMAGE_BYTES: u64 = 256 * 1024 * 1024;
const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const CACHE_DIRECTORY_NAME: &str = "clipx-sync";
const FILE_CACHE_VALIDATION_INTERVAL: Duration = Duration::from_secs(5);
const PROTOCOL_VERSION: u16 = 1;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

enum IncomingEvent {
    Message { peer_id: u64, message: WireMessage },
    Disconnected { peer_id: u64 },
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct SessionKey {
    peer_id: u64,
    message_id: String,
}

impl SessionKey {
    fn new(peer_id: u64, message_id: String) -> Self {
        Self {
            peer_id,
            message_id,
        }
    }
}

struct ActiveTransfer {
    last_activity: Instant,
    size: u64,
    transfer: IncomingTransfer,
}

struct ActiveImage {
    last_activity: Instant,
    image: IncomingImage,
    size: u64,
}

enum CapturedPayload {
    Text(String),
    Image(CapturedImage),
    Files(Arc<CapturedFiles>),
}

#[derive(Default)]
struct CaptureCache {
    files: Option<(Vec<PathBuf>, Arc<CapturedFiles>)>,
    next_file_validation: Option<Instant>,
    image: Option<CachedImage>,
    last_file_error: Option<String>,
}

struct SyncState {
    origin: String,
    sequence: u64,
    last_fingerprint: Option<u64>,
    sent_peers: HashSet<u64>,
    last_capture_error: Option<String>,
    last_send_error: Option<String>,
    completed: SeenIds,
    active_transfers: HashMap<SessionKey, ActiveTransfer>,
    active_images: HashMap<SessionKey, ActiveImage>,
}

impl SyncState {
    fn new() -> Self {
        Self {
            origin: format!("{}-{}", std::process::id(), timestamp_nanos()),
            sequence: 0,
            last_fingerprint: None,
            sent_peers: HashSet::new(),
            last_capture_error: None,
            last_send_error: None,
            completed: SeenIds::default(),
            active_transfers: HashMap::new(),
            active_images: HashMap::new(),
        }
    }

    fn next_id(&mut self) -> String {
        self.sequence = self.sequence.saturating_add(1);
        format!("{}-{}", self.origin, self.sequence)
    }
}

pub fn run(options: SyncOptions) -> Result<()> {
    cleanup_stale_cache();

    let hub = PeerHub::default();
    let (incoming_tx, incoming_rx) = mpsc::sync_channel(INCOMING_QUEUE_SIZE);

    if let Some(address) = options.listen {
        let listener =
            TcpListener::bind(address).with_context(|| format!("监听 {address} 失败"))?;
        let bound_address = listener.local_addr().unwrap_or(address);
        eprintln!("剪贴板同步：正在监听 {bound_address}");
        let listener_hub = hub.clone();
        let listener_incoming = incoming_tx.clone();
        thread::spawn(move || listen_loop(listener, listener_hub, listener_incoming));
    }

    for address in options.connect {
        let connector_hub = hub.clone();
        let connector_incoming = incoming_tx.clone();
        thread::spawn(move || connect_loop(&address, connector_hub, connector_incoming));
    }
    drop(incoming_tx);

    eprintln!(
        "剪贴板同步正在运行（clipx {}，协议 v{}）；未启用认证和加密",
        env!("CARGO_PKG_VERSION"),
        PROTOCOL_VERSION
    );
    clipboard_loop(incoming_rx, hub)
}

fn clipboard_loop(incoming: Receiver<IncomingEvent>, hub: PeerHub) -> Result<()> {
    let mut clipboard = Clipboard::new().context("访问系统剪贴板失败")?;
    let mut state = SyncState::new();
    let mut cache = CaptureCache::default();

    loop {
        while let Ok(event) = incoming.try_recv() {
            handle_incoming_event(event, &mut clipboard, &mut state);
        }
        expire_sessions(&mut state);

        match capture_payload(&mut clipboard, &mut cache) {
            Ok(payload) => {
                state.last_capture_error = None;
                let fingerprint = captured_fingerprint(&payload);
                let peers = hub.active_peer_ids()?;

                if state.last_fingerprint != Some(fingerprint) {
                    state.sent_peers.clear();
                }
                let targets = peers
                    .iter()
                    .copied()
                    .filter(|peer_id| !state.sent_peers.contains(peer_id))
                    .collect::<Vec<_>>();

                if !targets.is_empty() {
                    let id = state.next_id();
                    let summary = captured_summary(&payload);
                    eprintln!("剪贴板同步：正在发送 {summary}");
                    let result = send_payload(&hub, &targets, &id, payload);
                    state.sent_peers.extend(targets);
                    state.last_fingerprint = Some(fingerprint);
                    match result {
                        Ok(()) => state.last_send_error = None,
                        Err(error) => {
                            let message = format!("{error:#}");
                            if state.last_send_error.as_deref() != Some(&message) {
                                eprintln!("剪贴板同步：发送失败：{message}");
                            }
                            state.last_send_error = Some(message);
                        }
                    }
                }
            }
            Err(error) => {
                let message = format!("{error:#}");
                if state.last_capture_error.as_deref() != Some(&message) {
                    eprintln!("剪贴板同步：读取本地剪贴板失败：{message}");
                    state.last_capture_error = Some(message);
                }
            }
        }

        thread::sleep(POLL_INTERVAL);
    }
}

fn send_payload(hub: &PeerHub, peers: &[u64], id: &str, payload: CapturedPayload) -> Result<()> {
    match payload {
        CapturedPayload::Text(text) => hub.send_to_targets(
            peers,
            &WireMessage::Clipboard {
                id: id.to_string(),
                payload: ClipboardPayload::Text(text),
            },
        ),
        CapturedPayload::Image(image) => {
            send_image_transfer(hub, peers, id, image.fingerprint(), &image)
        }
        CapturedPayload::Files(files) => {
            if files.internal {
                eprintln!("剪贴板同步：忽略内部临时路径，避免同步回环");
                return Ok(());
            }
            send_file_transfer(hub, peers, id, &files)
        }
    }
}

fn handle_incoming_event(event: IncomingEvent, clipboard: &mut Clipboard, state: &mut SyncState) {
    let (peer_id, message) = match event {
        IncomingEvent::Message { peer_id, message } => (peer_id, message),
        IncomingEvent::Disconnected { peer_id } => {
            state.sent_peers.remove(&peer_id);
            cleanup_peer_sessions(state, peer_id);
            return;
        }
    };

    match message {
        WireMessage::Hello { .. } => {}
        WireMessage::Clipboard { id, payload } => {
            if !valid_message_id(&id) || state.completed.contains(&id) {
                return;
            }
            let fingerprint = fingerprint(&payload);
            let summary = payload_summary(&payload);
            eprintln!("剪贴板同步：收到 {summary}");
            match apply_payload(clipboard, &payload) {
                Ok(()) => {
                    state.completed.insert(&id);
                    mark_remote_applied(state, fingerprint, peer_id);
                    eprintln!("剪贴板同步：已应用 {summary}");
                }
                Err(error) => eprintln!("剪贴板同步：应用远端剪贴板失败：{error:#}"),
            }
        }
        WireMessage::TransferStart { id, entries } => {
            if !valid_message_id(&id) || state.completed.contains(&id) {
                return;
            }
            let key = SessionKey::new(peer_id, id.clone());
            if state.active_transfers.contains_key(&key) {
                remove_transfer(state, &key);
            }
            if state.active_transfers.len() >= MAX_ACTIVE_TRANSFERS {
                eprintln!("剪贴板同步：活动文件传输数量已达上限");
                return;
            }
            let Some(transfer_size) = entries
                .iter()
                .try_fold(0u64, |total, entry| total.checked_add(entry.size))
            else {
                eprintln!("剪贴板同步：活动文件传输大小溢出");
                return;
            };
            let active_size = state
                .active_transfers
                .values()
                .map(|active| active.size)
                .sum::<u64>();
            if active_size.saturating_add(transfer_size) > MAX_ACTIVE_TRANSFER_BYTES {
                eprintln!("剪贴板同步：活动文件传输大小已达上限");
                return;
            }
            let cache_scope = format!("{}-{peer_id}", state.origin);
            match IncomingTransfer::start(id.clone(), &cache_scope, entries) {
                Ok(transfer) => {
                    eprintln!("剪贴板同步：开始接收 {}", transfer.summary());
                    state.active_transfers.insert(
                        key,
                        ActiveTransfer {
                            last_activity: Instant::now(),
                            size: transfer_size,
                            transfer,
                        },
                    );
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
            if !valid_message_id(&id) || state.completed.contains(&id) {
                return;
            }
            let key = SessionKey::new(peer_id, id);
            let Some(active) = state.active_transfers.get_mut(&key) else {
                return;
            };
            active.last_activity = Instant::now();
            if let Err(error) = active.transfer.accept_chunk(file_index, offset, &bytes) {
                eprintln!("剪贴板同步：接收文件分块失败：{error:#}");
                remove_transfer(state, &key);
            }
        }
        WireMessage::TransferFileEnd {
            id,
            file_index,
            sha256,
        } => {
            if !valid_message_id(&id) || state.completed.contains(&id) {
                return;
            }
            let key = SessionKey::new(peer_id, id);
            let Some(active) = state.active_transfers.get_mut(&key) else {
                return;
            };
            active.last_activity = Instant::now();
            if let Err(error) = active.transfer.finish_file(file_index, sha256) {
                eprintln!("剪贴板同步：文件校验失败：{error:#}");
                remove_transfer(state, &key);
            }
        }
        WireMessage::TransferEnd { id } => {
            if !valid_message_id(&id) {
                return;
            }
            let key = SessionKey::new(peer_id, id.clone());
            if state.completed.contains(&id) {
                remove_transfer(state, &key);
                return;
            }
            let Some(active) = state.active_transfers.remove(&key) else {
                eprintln!("剪贴板同步：找不到要结束的文件传输 {id}");
                return;
            };
            let summary = active.transfer.summary();
            match active.transfer.finish() {
                Ok(finished) => match set_file_list(clipboard, &finished.roots) {
                    Ok(()) => {
                        state.completed.insert(&id);
                        mark_remote_applied(state, finished.fingerprint, peer_id);
                        eprintln!("剪贴板同步：已完成 {summary}");
                    }
                    Err(error) => {
                        eprintln!("剪贴板同步：写入接收文件列表失败：{error:#}");
                        finished.cleanup();
                    }
                },
                Err(error) => eprintln!("剪贴板同步：完成文件传输失败：{error:#}"),
            }
        }
        WireMessage::TransferAbort { id } => {
            let key = SessionKey::new(peer_id, id);
            remove_transfer(state, &key);
            state.active_images.remove(&key);
            eprintln!("剪贴板同步：远端中止传输");
        }
        WireMessage::ImageStart {
            id,
            fingerprint,
            width,
            height,
            size,
        } => {
            if !valid_message_id(&id) || state.completed.contains(&id) {
                return;
            }
            let key = SessionKey::new(peer_id, id);
            if state.active_images.len() >= MAX_ACTIVE_IMAGES {
                eprintln!("剪贴板同步：活动图片传输数量已达上限");
                return;
            }
            let active_size = state
                .active_images
                .values()
                .map(|active| active.size)
                .sum::<u64>();
            if active_size.saturating_add(size) > MAX_ACTIVE_IMAGE_BYTES {
                eprintln!("剪贴板同步：活动图片传输资源已达上限");
                return;
            }
            match IncomingImage::start(fingerprint, width, height, size) {
                Ok(image) => {
                    eprintln!(
                        "剪贴板同步：开始接收图片 {width}x{height}（{}）",
                        format_bytes(size)
                    );
                    state.active_images.insert(
                        key,
                        ActiveImage {
                            last_activity: Instant::now(),
                            image,
                            size,
                        },
                    );
                }
                Err(error) => eprintln!("剪贴板同步：拒绝图片传输：{error:#}"),
            }
        }
        WireMessage::ImageChunk { id, offset, bytes } => {
            if !valid_message_id(&id) || state.completed.contains(&id) {
                return;
            }
            let key = SessionKey::new(peer_id, id);
            let Some(active) = state.active_images.get_mut(&key) else {
                return;
            };
            active.last_activity = Instant::now();
            if let Err(error) = active.image.accept_chunk(offset, &bytes) {
                eprintln!("剪贴板同步：接收图片分块失败：{error:#}");
                state.active_images.remove(&key);
            }
        }
        WireMessage::ImageEnd { id, sha256 } => {
            if !valid_message_id(&id) {
                return;
            }
            let key = SessionKey::new(peer_id, id.clone());
            if state.completed.contains(&id) {
                state.active_images.remove(&key);
                return;
            }
            let Some(active) = state.active_images.remove(&key) else {
                eprintln!("剪贴板同步：找不到要结束的图片传输 {id}");
                return;
            };
            match active.image.finish(sha256) {
                Ok((fingerprint, payload)) => match apply_payload(clipboard, &payload) {
                    Ok(()) => {
                        state.completed.insert(&id);
                        mark_remote_applied(state, fingerprint, peer_id);
                        eprintln!("剪贴板同步：已应用图片");
                    }
                    Err(error) => eprintln!("剪贴板同步：应用远端图片失败：{error:#}"),
                },
                Err(error) => eprintln!("剪贴板同步：完成图片传输失败：{error:#}"),
            }
        }
    }
}

fn mark_remote_applied(state: &mut SyncState, fingerprint: u64, peer_id: u64) {
    if state.last_fingerprint != Some(fingerprint) {
        state.sent_peers.clear();
    }
    state.last_fingerprint = Some(fingerprint);
    state.sent_peers.insert(peer_id);
    state.last_send_error = None;
}

fn remove_transfer(state: &mut SyncState, key: &SessionKey) {
    if let Some(active) = state.active_transfers.remove(key) {
        active.transfer.cleanup();
    }
}

fn cleanup_peer_sessions(state: &mut SyncState, peer_id: u64) {
    let keys = state
        .active_transfers
        .keys()
        .filter(|key| key.peer_id == peer_id)
        .cloned()
        .collect::<Vec<_>>();
    for key in keys {
        remove_transfer(state, &key);
    }
    state.active_images.retain(|key, _| key.peer_id != peer_id);
}

fn expire_sessions(state: &mut SyncState) {
    let now = Instant::now();
    let keys = state
        .active_transfers
        .iter()
        .filter(|(_, active)| now.duration_since(active.last_activity) >= SESSION_IDLE_TIMEOUT)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in keys {
        remove_transfer(state, &key);
    }
    state
        .active_images
        .retain(|_, active| now.duration_since(active.last_activity) < SESSION_IDLE_TIMEOUT);
}

fn valid_message_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_MESSAGE_ID_LEN
}

fn capture_payload(clipboard: &mut Clipboard, cache: &mut CaptureCache) -> Result<CapturedPayload> {
    if let Ok(paths) = clipboard.get().file_list()
        && !paths.is_empty()
    {
        let cached = cache
            .files
            .as_ref()
            .filter(|(cached_paths, _)| *cached_paths == paths)
            .map(|(_, files)| files.clone());
        if let Some(files) = cached
            && cache
                .next_file_validation
                .is_some_and(|deadline| deadline > Instant::now())
        {
            return Ok(CapturedPayload::Files(files));
        }

        match capture_files(&paths) {
            Ok(files) => {
                let files = Arc::new(files);
                cache.files = Some((paths, files.clone()));
                cache.next_file_validation = Some(Instant::now() + FILE_CACHE_VALIDATION_INTERVAL);
                cache.last_file_error = None;
                return Ok(CapturedPayload::Files(files));
            }
            Err(error) => {
                let message = format!("{error:#}");
                if cache.last_file_error.as_deref() != Some(&message) {
                    eprintln!("剪贴板同步：忽略失效的文件列表：{message}");
                    cache.last_file_error = Some(message);
                }
            }
        }
    }
    cache.files = None;
    cache.next_file_validation = None;

    if let Some(image) = capture_image(clipboard, &mut cache.image)? {
        return Ok(CapturedPayload::Image(image));
    }
    if let Ok(text) = clipboard.get_text()
        && !text.is_empty()
    {
        cache.last_file_error = None;
        return Ok(CapturedPayload::Text(text));
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
        ClipboardPayload::Text(text) => text_summary(text),
        ClipboardPayload::Image {
            width,
            height,
            bytes,
        } => {
            format!(
                "图片 {width}x{height}（{}）",
                format_bytes(bytes.len() as u64)
            )
        }
    }
}

fn text_summary(text: &str) -> String {
    format!("文本（{}）", format_bytes(text.len() as u64))
}

fn captured_summary(payload: &CapturedPayload) -> String {
    match payload {
        CapturedPayload::Text(text) => text_summary(text),
        CapturedPayload::Image(image) => image.summary(),
        CapturedPayload::Files(files) => files_summary(files),
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    let value = format!("{value:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned();
    format!("{value} {}", UNITS[unit])
}

fn captured_fingerprint(payload: &CapturedPayload) -> u64 {
    match payload {
        CapturedPayload::Text(text) => text_fingerprint(text),
        CapturedPayload::Image(image) => image.fingerprint(),
        CapturedPayload::Files(files) => files.identity,
    }
}

fn fingerprint(payload: &ClipboardPayload) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match payload {
        ClipboardPayload::Text(text) => {
            0u8.hash(&mut hasher);
            text.hash(&mut hasher);
        }
        ClipboardPayload::Image {
            width,
            height,
            bytes,
        } => {
            1u8.hash(&mut hasher);
            width.hash(&mut hasher);
            height.hash(&mut hasher);
            bytes.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn text_fingerprint(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    0u8.hash(&mut hasher);
    text.hash(&mut hasher);
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
    fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }

    fn insert(&mut self, id: &str) -> bool {
        if !self.set.insert(id.to_owned()) {
            return false;
        }
        self.order.push_back(id.to_owned());
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
    use std::{env, fs};

    #[test]
    fn relative_path_rejects_traversal() {
        assert!(files::safe_relative_path(b"../secret.txt").is_err());
        assert_eq!(
            files::safe_relative_path(b"folder\\document.txt").unwrap(),
            PathBuf::from("folder/document.txt")
        );
    }

    #[test]
    fn directory_identity_changes_when_child_changes() {
        let root = env::temp_dir().join(format!("clipx-sync-test-{}", timestamp_nanos()));
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested").join("child.txt"), b"before").unwrap();
        let before = capture_files(std::slice::from_ref(&root)).unwrap().identity;
        fs::write(root.join("nested").join("child.txt"), b"after").unwrap();
        let after = capture_files(std::slice::from_ref(&root)).unwrap().identity;
        assert_ne!(before, after);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_chunks_verify_and_commit_atomically() {
        let entry = TransferEntry {
            path: b"sample.txt".to_vec(),
            kind: files::TransferEntryKind::File,
            size: 11,
            mode: files::file_mode(&fs::metadata(env::temp_dir()).unwrap()),
        };
        let mut transfer =
            IncomingTransfer::start(format!("test-{}", timestamp_nanos()), "0", vec![entry])
                .unwrap();
        transfer.accept_chunk(0, 0, b"hello world").unwrap();
        transfer
            .finish_file(0, Sha256::digest(b"hello world").into())
            .unwrap();
        let finished = transfer.finish().unwrap();
        assert_eq!(fs::read(&finished.roots[0]).unwrap(), b"hello world");
        finished.cleanup();
    }

    #[test]
    fn sync_state_generates_unique_message_ids() {
        let mut state = SyncState::new();
        assert_ne!(state.next_id(), state.next_id());
    }

    #[test]
    fn remote_clipboard_resets_delivery_tracking() {
        let mut state = SyncState::new();
        state.last_fingerprint = Some(1);
        state.sent_peers.extend([2, 3]);

        mark_remote_applied(&mut state, 2, 4);
        assert_eq!(state.sent_peers, HashSet::from([4]));

        mark_remote_applied(&mut state, 2, 5);
        assert_eq!(state.sent_peers, HashSet::from([4, 5]));
    }

    #[test]
    fn captured_text_matches_wire_text_fingerprint() {
        let text = "hello";
        assert_eq!(
            text_fingerprint(text),
            fingerprint(&ClipboardPayload::Text(text.to_owned()))
        );
    }

    #[test]
    fn format_bytes_uses_decimal_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1_000), "1 KB");
        assert_eq!(format_bytes(1_250_000), "1.25 MB");
    }
}
