//! `media_stream` 的用例：流式与缓冲**解出同一份字节**、临时文件的权限与 unlink、
//! 尾部（32 字节填充）在跨块读下的正确性、以及"退回缓冲路径"的判据。

use std::collections::VecDeque;
use std::io::Read;

use async_trait::async_trait;
use pretty_assertions::assert_eq;

use super::*;
use crate::wecom::media_crypt::MediaAesKey;
use crate::wecom::media_download::MediaChunkSource;

/// 一条 FIPS-197 的密钥（用例里到处用同一把，好让失败可复现）。
const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn key() -> MediaAesKey {
    let raw = hex::decode(KEY_HEX).expect("hex");
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&raw);
    MediaAesKey::from_bytes(bytes)
}

fn key_string() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(hex::decode(KEY_HEX).expect("hex"))
}

fn plaintext(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| {
            #[allow(clippy::cast_possible_truncation)]
            {
                (index as u8).wrapping_mul(37).wrapping_add(11)
            }
        })
        .collect()
}

/// 一个脚本化的分块来源：按给定切片喂，`None` 之后结束。
struct ScriptedSource {
    chunks: VecDeque<Result<Vec<u8>, MediaDownloadError>>,
    /// 每块最多再切多小（0 = 不切）—— 逼出 `carry` 与 `tail` 的跨块路径。
    slice: usize,
    pending: Vec<u8>,
}

impl ScriptedSource {
    /// 一整块喂进去。
    fn whole(data: Vec<u8>) -> Self {
        Self {
            chunks: VecDeque::from([Ok(data)]),
            slice: 0,
            pending: Vec::new(),
        }
    }

    /// 每次只喂 `slice` 个字节（密文块边界与读边界完全错开）。
    fn trickled(data: Vec<u8>, slice: usize) -> Self {
        Self {
            chunks: VecDeque::from([Ok(data)]),
            slice,
            pending: Vec::new(),
        }
    }

    /// 先给一段字节，再报一个下载层的失败。
    fn failing(data: Vec<u8>, error: MediaDownloadError) -> Self {
        Self {
            chunks: VecDeque::from([Ok(data), Err(error)]),
            slice: 0,
            pending: Vec::new(),
        }
    }
}

#[async_trait]
impl MediaChunkSource for ScriptedSource {
    async fn next_chunk(&mut self) -> Option<Result<Vec<u8>, MediaDownloadError>> {
        if !self.pending.is_empty() {
            let take = self.pending.len().min(self.slice);
            let piece = self.pending.drain(..take).collect::<Vec<u8>>();
            return Some(Ok(piece));
        }
        let next = self.chunks.pop_front()?;
        if self.slice > 0 {
            if let Ok(data) = next {
                self.pending = data;
                let take = self.pending.len().min(self.slice);
                let piece = self.pending.drain(..take).collect::<Vec<u8>>();
                return Some(Ok(piece));
            }
        }
        Some(next)
    }
}

fn read_all(file: &mut File) -> Vec<u8> {
    let mut out = Vec::new();
    file.read_to_end(&mut out).expect("read temp file");
    out
}

/// 流式解出的字节与**缓冲**路径逐字节相同（包括几个 32 的倍数 —— 那个陷阱所在）。
#[tokio::test]
async fn the_streamed_result_matches_the_buffered_one() {
    let key = key();
    for length in [0usize, 1, 15, 16, 17, 31, 32, 33, 47, 48, 64, 96, 1000] {
        let plain = plaintext(length);
        let ciphertext = key.encrypt(&plain);
        let mut source = ScriptedSource::whole(ciphertext.clone());
        let (mut file, size) = decrypt_to_file(&key_string(), &mut source, Path::new(""))
            .await
            .unwrap_or_else(|error| panic!("length {length}: {error}"));
        assert_eq!(size, i64::try_from(length).unwrap(), "length {length}");
        assert_eq!(read_all(&mut file), plain, "length {length}");
        // 与缓冲路径逐字节一致。
        assert_eq!(
            key.decrypt(&ciphertext).expect("buffered"),
            plain,
            "length {length}"
        );
    }
}

/// 一个跨多个 `mediaStreamChunk` 的文件，且**每次只喂 7 个字节** ⇒ 密文块边界与读边界完全错开。
#[tokio::test]
async fn the_tail_survives_trickled_reads_across_chunk_boundaries() {
    let key = key();
    for length in [
        MEDIA_STREAM_CHUNK - 1,
        MEDIA_STREAM_CHUNK,
        MEDIA_STREAM_CHUNK + 1,
        MEDIA_STREAM_CHUNK * 2,
    ] {
        let plain = plaintext(length);
        let ciphertext = key.encrypt(&plain);
        let mut source = ScriptedSource::trickled(ciphertext, 7);
        let (mut file, size) = decrypt_to_file(&key_string(), &mut source, Path::new(""))
            .await
            .unwrap_or_else(|error| panic!("length {length}: {error}"));
        assert_eq!(size, i64::try_from(length).unwrap(), "length {length}");
        assert_eq!(read_all(&mut file), plain, "length {length}");
    }
}

/// 临时文件**立刻**被 unlink（`nlink == 0`）且按 `0600` 建 —— 里面装的是解密后的附件。
#[tokio::test]
#[cfg(unix)]
async fn the_temp_file_is_unlinked_and_private() {
    use std::os::unix::fs::MetadataExt as _;
    use std::os::unix::fs::PermissionsExt as _;

    let key = key();
    let ciphertext = key.encrypt(&plaintext(4096));
    let mut source = ScriptedSource::whole(ciphertext);
    let (file, size) = decrypt_to_file(&key_string(), &mut source, Path::new(""))
        .await
        .expect("stream");
    assert_eq!(size, 4096);
    let metadata = file.metadata().expect("metadata");
    assert_eq!(metadata.nlink(), 0, "临时文件必须立刻 unlink");
    assert_eq!(
        metadata.permissions().mode() & 0o777,
        0o600,
        "解密后的内容只能是本用户的"
    );
}

/// 非块对齐、截断、空 body、错密钥：四类都是错误，而且**各自的变体可区分**。
#[tokio::test]
async fn broken_bodies_are_refused_with_distinguishable_errors() {
    let key = key();
    let ciphertext = key.encrypt(&plaintext(64));

    let mut short = ciphertext.clone();
    short.truncate(short.len() - 3);
    let mut source = ScriptedSource::whole(short.clone());
    assert_eq!(
        decrypt_to_file(&key_string(), &mut source, Path::new(""))
            .await
            .unwrap_err(),
        MediaStreamError::Misaligned {
            trailing: short.len() % AES_BLOCK_BYTES
        }
    );

    let mut dropped = ciphertext.clone();
    dropped.truncate(dropped.len() - AES_BLOCK_BYTES);
    let mut source = ScriptedSource::whole(dropped);
    assert!(matches!(
        decrypt_to_file(&key_string(), &mut source, Path::new(""))
            .await
            .unwrap_err(),
        MediaStreamError::Crypt(
            MediaCryptError::PaddingDisagrees
                | MediaCryptError::PaddingOutOfRange { .. }
                | MediaCryptError::PlaintextEmpty
        )
    ));

    let mut source = ScriptedSource::whole(Vec::new());
    assert_eq!(
        decrypt_to_file(&key_string(), &mut source, Path::new(""))
            .await
            .unwrap_err(),
        MediaStreamError::Empty
    );

    let other = MediaAesKey::from_bytes([0x5a; 32]);
    let mut source = ScriptedSource::whole(other.encrypt(&plaintext(80)));
    assert!(matches!(
        decrypt_to_file(&key_string(), &mut source, Path::new(""))
            .await
            .unwrap_err(),
        MediaStreamError::Crypt(_)
    ));

    // 密钥本身解不出来 ⇒ 是 `Crypt`，不是别的。
    let mut source = ScriptedSource::whole(ciphertext);
    assert!(matches!(
        decrypt_to_file("not-a-key", &mut source, Path::new(""))
            .await
            .unwrap_err(),
        MediaStreamError::Crypt(MediaCryptError::KeyNotDecodable)
    ));
}

/// 建不出临时文件 ⇒ **`TempFile`**：调用方据此退回缓冲路径（这是一个判据，不是一句日志）。
#[tokio::test]
async fn a_temp_file_failure_is_the_fallback_signal() {
    let key = key();
    let ciphertext = key.encrypt(&plaintext(16));
    let mut source = ScriptedSource::whole(ciphertext);
    let error = decrypt_to_file(
        &key_string(),
        &mut source,
        Path::new("/nonexistent-wecom-media-dir"),
    )
    .await
    .expect_err("no such directory");
    assert_eq!(error, MediaStreamError::TempFile);
}

/// `peekFile` 只读头部、并且**倒回去**（调用方随后要读完整个文件）。
#[tokio::test]
async fn peek_reads_the_head_and_rewinds() {
    let key = key();
    let ciphertext = key.encrypt(&plaintext(2048));
    let mut source = ScriptedSource::whole(ciphertext);
    let (mut file, _) = decrypt_to_file(&key_string(), &mut source, Path::new(""))
        .await
        .expect("stream");
    let head = peek_file(&mut file, 512).expect("peek");
    assert_eq!(head, plaintext(2048)[..512]);
    // 倒回去了 ⇒ 重新读完拿到的是**整个**文件。
    assert_eq!(read_all(&mut file), plaintext(2048));
}

/// 比 `n` 短的文件：交出全部，不改长度。
#[tokio::test]
async fn peek_on_a_short_file_returns_everything_there_is() {
    let key = key();
    for length in [0usize, 1, 7, 511] {
        let ciphertext = key.encrypt(&plaintext(length));
        let mut source = ScriptedSource::whole(ciphertext);
        let (mut file, _) = decrypt_to_file(&key_string(), &mut source, Path::new(""))
            .await
            .expect("stream");
        let head = peek_file(&mut file, 512).expect("peek");
        assert_eq!(head.len(), length, "length {length}");
        assert_eq!(head, plaintext(length), "length {length}");
    }
}

/// 显式给一个目录时临时文件落在那里（上游 `os.CreateTemp(dir, …)`），而且同样立刻消失。
#[tokio::test]
async fn the_temp_file_honours_the_directory_it_is_given() {
    let dir = std::env::temp_dir().join(format!("wecom-media-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let key = key();
    let ciphertext = key.encrypt(&plaintext(100));
    let mut source = ScriptedSource::whole(ciphertext);
    let (mut file, size) = decrypt_to_file(&key_string(), &mut source, &dir)
        .await
        .expect("stream");
    assert_eq!(size, 100);
    assert_eq!(read_all(&mut file), plaintext(100));
    // 目录里**没有**留下任何东西（建完就 unlink）。
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .expect("read_dir")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    std::fs::remove_dir_all(&dir).ok();
}

/// 流上的**下载层**失败（超帽 / 传输 / 块闸拒了）原样带出来，不被吞成"i/o 失败"。
#[tokio::test]
async fn a_download_failure_on_the_stream_keeps_its_verdict() {
    let key = key();
    let ciphertext = key.encrypt(&plaintext(64));
    for verdict in [
        MediaDownloadError::TooLarge,
        MediaDownloadError::Transport,
        MediaDownloadError::Guard(crate::wecom::media_guard::MediaGuardError::BlockedAddress),
    ] {
        let mut source = ScriptedSource::failing(ciphertext.clone(), verdict.clone());
        assert_eq!(
            decrypt_to_file(&key_string(), &mut source, Path::new(""))
                .await
                .unwrap_err(),
            MediaStreamError::Download(verdict)
        );
    }
}
