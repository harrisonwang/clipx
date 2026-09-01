use anyhow::{Context, Result, bail};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use super::{
    FRAME_LIMIT, OUTGOING_QUEUE_SIZE, PEER_POLL_INTERVAL, PEER_SEND_TIMEOUT, PROTOCOL_VERSION,
    WireMessage, format_bytes,
};

#[derive(Clone, Default)]
pub(super) struct PeerHub {
    peers: Arc<Mutex<Vec<Peer>>>,
}

struct Peer {
    sender: SyncSender<Arc<[u8]>>,
    alive: Arc<AtomicBool>,
}

impl PeerHub {
    pub(super) fn add(&self, sender: SyncSender<Arc<[u8]>>, alive: Arc<AtomicBool>) {
        let mut peers = self.peers.lock().expect("设备列表锁已损坏");
        peers.retain(|peer| peer.alive.load(Ordering::Relaxed));
        peers.push(Peer { sender, alive });
    }

    fn broadcast(&self, frame: Arc<[u8]>) -> usize {
        let peers = self.peers.lock().expect("设备列表锁已损坏");
        let targets = peers
            .iter()
            .filter(|peer| peer.alive.load(Ordering::Relaxed))
            .map(|peer| (peer.sender.clone(), peer.alive.clone()))
            .collect::<Vec<_>>();
        drop(peers);

        let sent = thread::scope(|scope| {
            let handles = targets
                .into_iter()
                .map(|(sender, alive)| {
                    let frame = frame.clone();
                    scope.spawn(move || send_to_peer(&sender, &alive, frame))
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap_or(false))
                .filter(|sent| *sent)
                .count()
        });

        self.peers
            .lock()
            .expect("设备列表锁已损坏")
            .retain(|peer| peer.alive.load(Ordering::Relaxed));
        sent
    }

    pub(super) fn send_message(&self, message: &WireMessage) -> Result<usize> {
        let frame = encode_message(message)?;
        Ok(self.broadcast(frame))
    }

    pub(super) fn send_required(&self, message: &WireMessage) -> Result<()> {
        if self.send_message(message)? == 0 {
            bail!("当前没有已连接的设备");
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
pub(super) fn listen_loop(address: &SocketAddr, hub: PeerHub, incoming: SyncSender<WireMessage>) {
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

pub(super) fn connect_loop(address: &str, hub: PeerHub, incoming: SyncSender<WireMessage>) {
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

pub(super) fn register_connection(
    stream: TcpStream,
    hub: PeerHub,
    incoming: SyncSender<WireMessage>,
) -> Arc<AtomicBool> {
    let (outgoing_tx, outgoing_rx) = mpsc::sync_channel::<Arc<[u8]>>(OUTGOING_QUEUE_SIZE);

    let alive = Arc::new(AtomicBool::new(true));
    let writer_alive = alive.clone();
    let reader_alive = alive.clone();
    let mut writer = stream.try_clone().expect("克隆设备连接失败");
    let mut reader = stream;

    if let Err(error) = writer.set_write_timeout(Some(PEER_SEND_TIMEOUT)) {
        eprintln!("剪贴板同步：设置发送超时失败：{error}");
    }
    if let Err(error) = writer.set_nodelay(true) {
        eprintln!("剪贴板同步：启用低延迟发送失败：{error}");
    }

    let hello = encode_message(&WireMessage::Hello {
        version: PROTOCOL_VERSION,
    })
    .expect("序列化握手消息失败");
    outgoing_tx.send(hello).expect("发送握手消息失败");
    hub.add(outgoing_tx, alive.clone());

    thread::spawn(move || {
        while writer_alive.load(Ordering::Relaxed) {
            match outgoing_rx.recv_timeout(PEER_POLL_INTERVAL) {
                Ok(frame) => {
                    if let Err(error) = write_frame(&mut writer, &frame) {
                        eprintln!("剪贴板同步：发送数据失败：{error}");
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        writer_alive.store(false, Ordering::Relaxed);
        let _ = writer.shutdown(Shutdown::Both);
    });

    thread::spawn(move || {
        let mut handshaken = false;
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
                Ok(WireMessage::Hello { version }) if !handshaken => {
                    if version != PROTOCOL_VERSION {
                        eprintln!(
                            "剪贴板同步：协议版本不兼容，对端为 {version}，本地为 {PROTOCOL_VERSION}"
                        );
                        break;
                    }
                    handshaken = true;
                }
                Ok(WireMessage::Hello { .. }) => {
                    eprintln!("剪贴板同步：收到重复握手消息");
                    break;
                }
                Ok(message) if handshaken => {
                    if incoming.send(message).is_err() {
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
        let _ = reader.shutdown(Shutdown::Both);
    });

    alive
}

fn send_to_peer(sender: &SyncSender<Arc<[u8]>>, alive: &Arc<AtomicBool>, frame: Arc<[u8]>) -> bool {
    let deadline = Instant::now() + PEER_SEND_TIMEOUT;
    let mut pending = frame;
    loop {
        if !alive.load(Ordering::Relaxed) {
            return false;
        }
        match sender.try_send(pending) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => {
                alive.store(false, Ordering::Relaxed);
                return false;
            }
            Err(TrySendError::Full(frame)) => {
                if Instant::now() >= deadline {
                    eprintln!("剪贴板同步：设备发送队列持续拥塞，已断开设备");
                    alive.store(false, Ordering::Relaxed);
                    return false;
                }
                pending = frame;
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
fn write_frame<W: Write>(stream: &mut W, frame: &[u8]) -> io::Result<()> {
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

    #[test]
    fn frame_round_trip_preserves_bytes() {
        let mut encoded = Vec::new();
        write_frame(&mut encoded, b"hello clipx").expect("写入测试帧失败");
        let mut receiver = Cursor::new(encoded);
        assert_eq!(
            read_frame(&mut receiver).expect("读取测试帧失败"),
            Some(b"hello clipx".to_vec())
        );
    }

    #[test]
    fn frame_limits_reject_invalid_sizes() {
        let mut encoded = Vec::new();
        assert!(write_frame(&mut encoded, &[]).is_err());

        encoded.clear();
        encoded
            .write_all(&((FRAME_LIMIT as u32) + 1).to_be_bytes())
            .expect("写入无效帧头失败");
        let mut receiver = Cursor::new(encoded);
        assert!(read_frame(&mut receiver).is_err());
    }
}
