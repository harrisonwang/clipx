use anyhow::{Context, Result, bail};
use arboard::{Clipboard, ImageData};
use sha2::{Digest, Sha256};
use std::{borrow::Cow, io::Cursor, sync::Arc};

use super::transport::PeerHub;
use super::{CHUNK_SIZE, ClipboardPayload, WireMessage, format_bytes};

const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_IMAGE_TRANSFER_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct CapturedImage {
    fingerprint: u64,
    width: u32,
    height: u32,
    bytes: Arc<Vec<u8>>,
}

pub(super) struct CachedImage {
    raw_fingerprint: u64,
    image: CapturedImage,
}

pub(super) struct IncomingImage {
    fingerprint: u64,
    width: u32,
    height: u32,
    expected_size: u64,
    received_size: u64,
    bytes: Vec<u8>,
    hasher: Sha256,
}

impl CapturedImage {
    pub(super) fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    pub(super) fn summary(&self) -> String {
        format!(
            "图片 {}x{}（{}）",
            self.width,
            self.height,
            format_bytes(self.bytes.len() as u64)
        )
    }
}

pub(super) fn capture_image(
    clipboard: &mut Clipboard,
    cache: &mut Option<CachedImage>,
) -> Result<Option<CapturedImage>> {
    let Ok(image) = clipboard.get_image() else {
        *cache = None;
        return Ok(None);
    };

    let width = u32::try_from(image.width).context("剪贴板图片宽度过大")?;
    let height = u32::try_from(image.height).context("剪贴板图片高度过大")?;
    let raw_fingerprint = image_fingerprint(width, height, image.bytes.as_ref());
    if let Some(cached) = cache
        && cached.raw_fingerprint == raw_fingerprint
    {
        return Ok(Some(cached.image.clone()));
    }

    let bytes = Arc::new(encode_png(width, height, image.bytes.as_ref())?);
    let fingerprint = image_payload_fingerprint(width, height, bytes.as_ref());
    let captured = CapturedImage {
        fingerprint,
        width,
        height,
        bytes,
    };
    *cache = Some(CachedImage {
        raw_fingerprint,
        image: captured.clone(),
    });
    Ok(Some(captured))
}

pub(super) fn send_image_transfer(
    hub: &PeerHub,
    id: &str,
    fingerprint: u64,
    image: &CapturedImage,
) -> Result<()> {
    let result = (|| {
        let size = u64::try_from(image.bytes.len()).context("图片大小溢出")?;
        if size > MAX_IMAGE_TRANSFER_BYTES as u64 {
            bail!(
                "图片大小超过限制（最多 {}）",
                format_bytes(MAX_IMAGE_TRANSFER_BYTES as u64)
            );
        }
        hub.send_required(&WireMessage::ImageStart {
            id: id.to_string(),
            fingerprint,
            width: image.width,
            height: image.height,
            size,
        })?;

        let mut offset = 0u64;
        for chunk in image.bytes.chunks(CHUNK_SIZE) {
            hub.send_required(&WireMessage::ImageChunk {
                id: id.to_string(),
                offset,
                bytes: chunk.to_vec(),
            })?;
            offset = offset
                .checked_add(u64::try_from(chunk.len()).expect("图片分块大小应可转换为 u64"))
                .context("图片偏移量溢出")?;
        }
        let sha256: [u8; 32] = Sha256::digest(image.bytes.as_ref()).into();
        hub.send_required(&WireMessage::ImageEnd {
            id: id.to_string(),
            sha256,
        })
    })();
    if result.is_err() {
        let _ = hub.send_message(&WireMessage::TransferAbort { id: id.to_string() });
    }
    result
}

impl IncomingImage {
    pub(super) fn start(
        fingerprint: u64,
        width: u32,
        height: u32,
        expected_size: u64,
    ) -> Result<Self> {
        if width == 0 || height == 0 {
            bail!("图片尺寸不能为空")
        }
        if expected_size == 0 || expected_size > MAX_IMAGE_TRANSFER_BYTES as u64 {
            bail!("图片传输大小超过限制")
        }
        let capacity = usize::try_from(expected_size).context("图片大小无法在本机表示")?;
        Ok(Self {
            fingerprint,
            width,
            height,
            expected_size,
            received_size: 0,
            bytes: Vec::with_capacity(capacity),
            hasher: Sha256::new(),
        })
    }

    pub(super) fn accept_chunk(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() || bytes.len() > CHUNK_SIZE {
            bail!("图片分块大小无效")
        }
        if offset != self.received_size {
            bail!("图片分块偏移量不连续")
        }
        let next = self
            .received_size
            .checked_add(u64::try_from(bytes.len()).expect("图片分块大小应可转换为 u64"))
            .context("图片接收大小溢出")?;
        if next > self.expected_size {
            bail!("图片接收大小超过声明值")
        }
        self.bytes.extend_from_slice(bytes);
        self.hasher.update(bytes);
        self.received_size = next;
        Ok(())
    }

    pub(super) fn finish(self, expected_sha256: [u8; 32]) -> Result<(u64, ClipboardPayload)> {
        if self.received_size != self.expected_size {
            bail!(
                "图片大小不匹配：期望 {}，实际 {}",
                format_bytes(self.expected_size),
                format_bytes(self.received_size)
            )
        }
        let actual_sha256: [u8; 32] = self.hasher.finalize().into();
        if actual_sha256 != expected_sha256 {
            bail!("图片 SHA-256 校验不匹配")
        }
        let (decoded_width, decoded_height, _) = decode_png(&self.bytes)?;
        if decoded_width != self.width || decoded_height != self.height {
            bail!("图片尺寸与传输清单不一致")
        }
        Ok((
            self.fingerprint,
            ClipboardPayload::Image {
                width: self.width,
                height: self.height,
                bytes: self.bytes,
            },
        ))
    }
}

pub(super) fn apply_image(
    clipboard: &mut Clipboard,
    width: u32,
    height: u32,
    bytes: &[u8],
) -> Result<()> {
    let (decoded_width, decoded_height, decoded) = decode_png(bytes)?;
    if decoded_width != width || decoded_height != height {
        bail!("图片尺寸与传输清单不一致")
    }
    clipboard
        .set_image(ImageData {
            width: decoded_width as usize,
            height: decoded_height as usize,
            bytes: Cow::Owned(decoded),
        })
        .context("写入图片失败")?;
    Ok(())
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let expected = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .context("图片尺寸过大")?;
    if rgba.len() != expected {
        bail!("图片像素数据大小不正确")
    }
    if expected > MAX_IMAGE_BYTES {
        bail!("图片像素数据超过限制")
    }

    let mut encoded = Vec::new();
    let mut encoder = png::Encoder::new(&mut encoded, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().context("编码图片头失败")?;
    writer.write_image_data(rgba).context("编码图片失败")?;
    drop(writer);
    Ok(encoded)
}

fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::IDENTITY);
    let mut reader = decoder.read_info().context("读取图片头失败")?;
    let (decoded_width, decoded_height, color_type, bit_depth) = {
        let info = reader.info();
        (info.width, info.height, info.color_type, info.bit_depth)
    };
    if color_type != png::ColorType::Rgba || bit_depth != png::BitDepth::Eight {
        bail!("图片格式不是 RGBA PNG")
    }
    let expected = usize::try_from(decoded_width)
        .ok()
        .and_then(|width| {
            usize::try_from(decoded_height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .context("图片尺寸过大")?;
    if expected > MAX_IMAGE_BYTES || reader.output_buffer_size() > MAX_IMAGE_BYTES {
        bail!("图片解码大小超过限制")
    }

    let mut decoded = vec![0u8; reader.output_buffer_size()];
    let output = reader.next_frame(&mut decoded).context("解码图片失败")?;
    if output.buffer_size() != expected {
        bail!("图片像素数据大小不正确")
    }
    decoded.truncate(output.buffer_size());
    Ok((decoded_width, decoded_height, decoded))
}

fn image_fingerprint(width: u32, height: u32, bytes: &[u8]) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(width.to_be_bytes());
    hasher.update(height.to_be_bytes());
    hasher.update(bytes);
    let digest = hasher.finalize();
    u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 摘要长度应足够"))
}

fn image_payload_fingerprint(width: u32, height: u32, bytes: &[u8]) -> u64 {
    image_fingerprint(width, height, bytes)
}

#[cfg(test)]
mod tests {
    use super::super::FRAME_LIMIT;
    use super::*;

    #[test]
    fn png_round_trip_preserves_pixels() {
        let pixels = [
            255, 0, 0, 255, // 红色
            0, 255, 0, 255, // 绿色
        ];
        let encoded = encode_png(2, 1, &pixels).unwrap();
        assert_eq!(&encoded[..8], b"\x89PNG\r\n\x1a\n");
        let (width, height, decoded) = decode_png(&encoded).unwrap();
        assert_eq!((width, height), (2, 1));
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn large_png_uses_chunked_image_transfer() {
        let width = 1_200;
        let height = 1_200;
        let mut pixels = Vec::with_capacity(width * height * 4);
        let mut value = 0x1234_5678u32;
        for _ in 0..(width * height) {
            value = value.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            pixels.extend_from_slice(&value.to_le_bytes());
        }
        let encoded = encode_png(width as u32, height as u32, &pixels).unwrap();
        assert!(encoded.len() > FRAME_LIMIT);

        let fingerprint = image_payload_fingerprint(width as u32, height as u32, &encoded);
        let mut incoming = IncomingImage::start(
            fingerprint,
            width as u32,
            height as u32,
            encoded.len() as u64,
        )
        .unwrap();
        for (offset, chunk) in encoded.chunks(CHUNK_SIZE).enumerate() {
            incoming
                .accept_chunk((offset * CHUNK_SIZE) as u64, chunk)
                .unwrap();
        }
        let sha256: [u8; 32] = Sha256::digest(&encoded).into();
        let (received_fingerprint, payload) = incoming.finish(sha256).unwrap();
        assert_eq!(received_fingerprint, fingerprint);
        match payload {
            ClipboardPayload::Image {
                width: received_width,
                height: received_height,
                bytes,
            } => {
                assert_eq!(
                    (received_width, received_height),
                    (width as u32, height as u32)
                );
                assert_eq!(bytes, encoded);
            }
            ClipboardPayload::Text(_) => panic!("图片传输返回了文本"),
        }
    }
}
