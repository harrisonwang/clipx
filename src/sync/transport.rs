use anyhow::{Context, Result, anyhow, bail};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::Duration,
};

use super::{
    FRAME_LIMIT, IncomingEvent, MAX_PEERS, PEER_CONNECT_TIMEOUT, PEER_SEND_TIMEOUT,
    PROTOCOL_VERSION, WireMessage, format_bytes,
};

const OUTGOING_QUEUE_SIZE: usize = 16;
static NEXT_PEER_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Clone)]
pub(super) struct PeerHub {
    peers: Arc<Mutex<Vec<Peer>>>,
    shutdown: Arc<AtomicBool>,
}

#[derive(Clone)]
pub(super) struct ConnectControl {
    enabled: Arc<AtomicBool>,
}

impl Default for ConnectControl {
    fn default() -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl ConnectControl {
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(super) fn pause(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(super) fn resume(&self) {
        self.enabled.store(true, Ordering::Release);
    }

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(super) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
}

impl Default for PeerHub {
    fn default() -> Self {
        Self {
            peers: Arc::new(Mutex::new(Vec::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }
}

struct Peer {
    peer_id: u64,
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    address: SocketAddr,
    sender: SyncSender<Arc<[u8]>>,
    alive: Arc<AtomicBool>,
    stream: Arc<Mutex<TcpStream>>,
}

impl PeerHub {
    pub(super) fn add(
        &self,
        peer_id: u64,
        address: SocketAddr,
        sender: SyncSender<Arc<[u8]>>,
        alive: Arc<AtomicBool>,
        stream: Arc<Mutex<TcpStream>>,
    ) -> Result<bool> {
        if !alive.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let mut peers = self.peers.lock().map_err(|_| anyhow!("设备列表锁已损坏"))?;
        peers.retain(|peer| peer.alive.load(Ordering::Relaxed));
        if peers.len() >= MAX_PEERS {
            return Ok(false);
        }
        peers.push(Peer {
            peer_id,
            address,
            sender,
            alive,
            stream,
        });
        Ok(true)
    }

    pub(super) fn active_peer_ids(&self) -> Result<Vec<u64>> {
        let mut peers = self.peers.lock().map_err(|_| anyhow!("设备列表锁已损坏"))?;
        peers.retain(|peer| peer.alive.load(Ordering::Relaxed));
        Ok(peers.iter().map(|peer| peer.peer_id).collect())
    }

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(super) fn active_peer_addresses(&self) -> Result<Vec<SocketAddr>> {
        let mut peers = self.peers.lock().map_err(|_| anyhow!("设备列表锁已损坏"))?;
        peers.retain(|peer| peer.alive.load(Ordering::Relaxed));
        Ok(peers.iter().map(|peer| peer.address).collect())
    }

    pub(super) fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.disconnect_all();
    }

    pub(super) fn disconnect_all(&self) {
        let peers = self.peers.lock().expect("设备列表锁已损坏");
        for peer in peers.iter() {
            peer.alive.store(false, Ordering::Release);
            let _ = peer
                .stream
                .lock()
                .expect("设备连接锁已损坏")
                .shutdown(Shutdown::Both);
        }
    }

    pub(super) fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    fn remove(&self, peer_id: u64) -> Result<()> {
        let mut peers = self.peers.lock().map_err(|_| anyhow!("设备列表锁已损坏"))?;
        peers.retain(|peer| peer.peer_id != peer_id);
        Ok(())
    }

    pub(super) fn send_to_targets(&self, peer_ids: &[u64], message: &WireMessage) -> Result<()> {
        if peer_ids.is_empty() {
            bail!("当前没有已连接的设备");
        }
        let frame = encode_message(message)?;
        let targets = {
            let peers = self.peers.lock().map_err(|_| anyhow!("设备列表锁已损坏"))?;
            peer_ids
                .iter()
                .filter_map(|peer_id| {
                    peers
                        .iter()
                        .find(|peer| peer.peer_id == *peer_id)
                        .map(|peer| (peer.peer_id, peer.sender.clone(), peer.alive.clone()))
                })
                .collect::<Vec<_>>()
        };
        if targets.len() != peer_ids.len() {
            bail!("设备连接已关闭");
        }

        let mut failure = None;
        for (peer_id, sender, alive) in targets {
            if !alive.load(Ordering::Relaxed) {
                failure = Some(format!("设备 {peer_id} 已关闭"));
                continue;
            }
            match sender.send(frame.clone()) {
                Ok(()) => {}
                Err(_) => {
                    alive.store(false, Ordering::Relaxed);
                    failure = Some(format!("设备 {peer_id} 已关闭"));
                }
            }
        }
        let mut peers = self.peers.lock().map_err(|_| anyhow!("设备列表锁已损坏"))?;
        peers.retain(|peer| peer.alive.load(Ordering::Relaxed));
        if let Some(error) = failure {
            bail!("{error}");
        }
        Ok(())
    }
}

fn encode_message(message: &WireMessage) -> Result<Arc<[u8]>> {
    let frame = bincode::serialize(message).context("序列化剪贴板消息失败")?;
    if frame.is_empty() || frame.len() > FRAME_LIMIT {
        bail!(
            "剪贴板消息过大，无法放入单帧（{}）",
            format_bytes(frame.len() as u64)
        );
    }
    Ok(Arc::from(frame.into_boxed_slice()))
}

pub(super) fn listen_loop(
    listener: TcpListener,
    hub: PeerHub,
    incoming: SyncSender<IncomingEvent>,
) {
    if let Err(error) = listener.set_nonblocking(true) {
        eprintln!("剪贴板同步：设置监听非阻塞失败：{error}");
        return;
    }

    while !hub.is_shutdown() {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(error) = stream.set_nonblocking(false) {
                    eprintln!("剪贴板同步：设置设备连接为阻塞模式失败：{error}");
                    continue;
                }
                eprintln!("剪贴板同步：设备已连接 {:?}", stream.peer_addr());
                register_connection(stream, hub.clone(), incoming.clone());
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => eprintln!("剪贴板同步：接受连接失败：{error}"),
        }
    }
}

pub(super) fn connect_loop(
    address: &str,
    hub: PeerHub,
    incoming: SyncSender<IncomingEvent>,
    control: ConnectControl,
) {
    while !hub.is_shutdown() {
        if !control.is_enabled() {
            thread::sleep(Duration::from_millis(100));
            continue;
        }
        match connect_with_timeout(address) {
            Ok(stream) => {
                eprintln!("剪贴板同步：已连接到 {address}");
                let alive = register_connection(stream, hub.clone(), incoming.clone());
                if !control.is_enabled() {
                    hub.disconnect_all();
                }
                while alive.load(Ordering::Relaxed) && control.is_enabled() && !hub.is_shutdown() {
                    thread::sleep(std::time::Duration::from_millis(250));
                }
                eprintln!("剪贴板同步：与 {address} 的连接已关闭");
            }
            Err(error) => eprintln!("剪贴板同步：连接 {address} 失败：{error}"),
        }
        for _ in 0..20 {
            if hub.is_shutdown() || !control.is_enabled() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn connect_with_timeout(address: &str) -> io::Result<TcpStream> {
    connect_to_addresses(address.to_socket_addrs()?)
}

fn connect_to_addresses(addresses: impl IntoIterator<Item = SocketAddr>) -> io::Result<TcpStream> {
    let mut last_error = None;
    for target in addresses {
        match TcpStream::connect_timeout(&target, PEER_CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "设备地址没有可用的解析结果",
        )
    }))
}

pub(super) fn register_connection(
    stream: TcpStream,
    hub: PeerHub,
    incoming: SyncSender<IncomingEvent>,
) -> Arc<AtomicBool> {
    let address = stream
        .peer_addr()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
    let (outgoing_tx, outgoing_rx) = mpsc::sync_channel::<Arc<[u8]>>(OUTGOING_QUEUE_SIZE);
    let peer_id = NEXT_PEER_ID.fetch_add(1, Ordering::Relaxed);
    let alive = Arc::new(AtomicBool::new(true));
    let stream_for_shutdown = match stream.try_clone() {
        Ok(stream) => Arc::new(Mutex::new(stream)),
        Err(error) => {
            eprintln!("剪贴板同步：克隆设备连接失败：{error}");
            alive.store(false, Ordering::Relaxed);
            return alive;
        }
    };

    let mut writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(error) => {
            eprintln!("剪贴板同步：克隆设备连接失败：{error}");
            alive.store(false, Ordering::Relaxed);
            return alive;
        }
    };
    let mut reader = stream;
    if let Err(error) = writer.set_write_timeout(Some(PEER_SEND_TIMEOUT)) {
        eprintln!("剪贴板同步：设置发送超时失败：{error}");
    }
    if let Err(error) = writer.set_nodelay(true) {
        eprintln!("剪贴板同步：启用低延迟发送失败：{error}");
    }
    if let Err(error) = reader.set_read_timeout(Some(PEER_CONNECT_TIMEOUT)) {
        eprintln!("剪贴板同步：设置握手超时失败：{error}");
        alive.store(false, Ordering::Relaxed);
        let _ = writer.shutdown(Shutdown::Both);
        return alive;
    }

    let writer_alive = alive.clone();
    thread::spawn(move || {
        while writer_alive.load(Ordering::Relaxed) {
            let Ok(frame) = outgoing_rx.recv() else { break };
            if let Err(error) = write_frame(&mut writer, &frame) {
                eprintln!("剪贴板同步：发送数据失败：{error}");
                break;
            }
        }
        writer_alive.store(false, Ordering::Relaxed);
        let _ = writer.shutdown(Shutdown::Both);
    });

    let hello = match encode_message(&WireMessage::Hello {
        version: PROTOCOL_VERSION,
    }) {
        Ok(hello) => hello,
        Err(error) => {
            eprintln!("剪贴板同步：序列化握手消息失败：{error:#}");
            alive.store(false, Ordering::Relaxed);
            let _ = reader.shutdown(Shutdown::Both);
            return alive;
        }
    };
    if outgoing_tx.send(hello).is_err() {
        eprintln!("剪贴板同步：发送握手消息失败");
        alive.store(false, Ordering::Relaxed);
        let _ = reader.shutdown(Shutdown::Both);
        return alive;
    }

    let reader_alive = alive.clone();
    let reader_hub = hub.clone();
    thread::spawn(move || {
        let mut handshaken = false;
        loop {
            let frame = match read_frame(&mut reader) {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    eprintln!("剪贴板同步：设备握手超时");
                    break;
                }
                Err(error) => {
                    eprintln!("剪贴板同步：读取设备数据失败：{error}");
                    break;
                }
            };

            match bincode::deserialize::<WireMessage>(&frame) {
                Ok(WireMessage::Hello { version }) if !handshaken => {
                    if version != PROTOCOL_VERSION {
                        eprintln!(
                            "剪贴板同步：协议版本不兼容，对端为 {version}，本地为 {PROTOCOL_VERSION}"
                        );
                        break;
                    }
                    if let Err(error) = reader.set_read_timeout(None) {
                        eprintln!("剪贴板同步：设置接收超时失败：{error}");
                        break;
                    }
                    match reader_hub.add(
                        peer_id,
                        address,
                        outgoing_tx.clone(),
                        reader_alive.clone(),
                        stream_for_shutdown.clone(),
                    ) {
                        Ok(true) => handshaken = true,
                        Ok(false) => {
                            eprintln!("剪贴板同步：无法注册设备连接");
                            break;
                        }
                        Err(error) => {
                            eprintln!("剪贴板同步：注册设备连接失败：{error:#}");
                            break;
                        }
                    }
                }
                Ok(WireMessage::Hello { .. }) => {
                    eprintln!("剪贴板同步：收到重复握手消息");
                    break;
                }
                Ok(message) if handshaken => {
                    if incoming
                        .send(IncomingEvent::Message { peer_id, message })
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(_) => {
                    eprintln!("剪贴板同步：收到数据前未完成协议握手");
                    break;
                }
                Err(error) => {
                    eprintln!("剪贴板同步：收到无效消息，断开设备：{error}");
                    break;
                }
            }
        }
        reader_alive.store(false, Ordering::Relaxed);
        if let Err(error) = reader_hub.remove(peer_id) {
            eprintln!("剪贴板同步：清理设备连接失败：{error:#}");
        }
        let _ = reader.shutdown(Shutdown::Both);
        let _ = incoming.send(IncomingEvent::Disconnected { peer_id });
    });

    alive
}

fn write_frame<W: Write>(stream: &mut W, frame: &[u8]) -> io::Result<()> {
    if frame.is_empty() || frame.len() > FRAME_LIMIT || frame.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "无效的数据帧大小",
        ));
    }
    stream.write_all(&(frame.len() as u32).to_be_bytes())?;
    stream.write_all(frame)
}

fn read_frame<R: Read>(stream: &mut R) -> io::Result<Option<Vec<u8>>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn wait_for_peers(hub: &PeerHub) -> Vec<u64> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let peers = hub.active_peer_ids().unwrap();
            if !peers.is_empty() {
                return peers;
            }
            assert!(std::time::Instant::now() < deadline, "等待设备握手超时");
            thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn frame_round_trip_preserves_bytes() {
        let mut encoded = Vec::new();
        write_frame(&mut encoded, b"hello clipx").unwrap();
        assert_eq!(
            read_frame(&mut Cursor::new(encoded)).unwrap(),
            Some(b"hello clipx".to_vec())
        );
    }

    #[test]
    fn frame_limits_reject_invalid_sizes() {
        let mut encoded = Vec::new();
        assert!(write_frame(&mut encoded, &[]).is_err());
        encoded.extend_from_slice(&((FRAME_LIMIT as u32) + 1).to_be_bytes());
        assert!(read_frame(&mut Cursor::new(encoded)).is_err());
    }

    #[test]
    fn connect_control_can_pause_and_resume_without_shutdown() {
        let control = ConnectControl::default();
        assert!(control.is_enabled());
        control.pause();
        assert!(!control.is_enabled());
        control.resume();
        assert!(control.is_enabled());
    }

    #[test]
    fn connect_loop_does_not_retry_after_controlled_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let connector_hub = PeerHub::default();
        let server_hub = PeerHub::default();
        let control = ConnectControl::default();
        let (connector_events_tx, _connector_events_rx) = mpsc::sync_channel(4);
        let (server_events_tx, _server_events_rx) = mpsc::sync_channel(4);
        let connect_address = address.to_string();
        let connector_thread = {
            let connector_hub = connector_hub.clone();
            let control = control.clone();
            thread::spawn(move || {
                connect_loop(
                    &connect_address,
                    connector_hub,
                    connector_events_tx,
                    control,
                )
            })
        };

        let (server_stream, _) = listener.accept().unwrap();
        let server_alive = register_connection(server_stream, server_hub.clone(), server_events_tx);
        wait_for_peers(&connector_hub);

        control.pause();
        connector_hub.disconnect_all();
        assert!(connector_hub.active_peer_ids().unwrap().is_empty());

        listener.set_nonblocking(true).unwrap();
        thread::sleep(Duration::from_millis(250));
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        ));

        connector_hub.shutdown();
        server_hub.shutdown();
        server_alive.store(false, Ordering::Relaxed);
        connector_thread.join().unwrap();
    }

    #[test]
    fn tcp_connection_exchanges_message() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let connector = thread::spawn(move || TcpStream::connect(address).unwrap());
        let (server_stream, _) = listener.accept().unwrap();
        let client_stream = connector.join().unwrap();

        let client_hub = PeerHub::default();
        let server_hub = PeerHub::default();
        let (client_events_tx, _client_events_rx) = mpsc::sync_channel(4);
        let (server_events_tx, server_events_rx) = mpsc::sync_channel(4);
        let client_alive = register_connection(client_stream, client_hub.clone(), client_events_tx);
        let server_alive = register_connection(server_stream, server_hub.clone(), server_events_tx);

        let peers = wait_for_peers(&client_hub);
        client_hub
            .send_to_targets(
                &peers,
                &WireMessage::Clipboard {
                    id: "message".to_string(),
                    payload: super::super::ClipboardPayload::Text("hello".to_string()),
                },
            )
            .unwrap();
        let IncomingEvent::Message { message, .. } = server_events_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
        else {
            panic!("应收到客户端消息");
        };
        assert!(matches!(message, WireMessage::Clipboard { id, .. } if id == "message"));

        client_hub.disconnect_all();
        assert!(client_hub.active_peer_ids().unwrap().is_empty());
        client_alive.store(false, Ordering::Relaxed);
        server_alive.store(false, Ordering::Relaxed);
    }
}
