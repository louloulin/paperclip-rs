//! `media_stream.go`（171 行）的本地落点：**一个不必装进内存的附件**。
//!
//! - **写者**：M7-18（`LUM-1783` / `docs/60-M7-PLAN.md` §3.3；本文件**不在** issue rev 5 的写集里，
//!   勘误见 `docs/32` §35 的 D1 —— 上游 `media_stream.go` 在派生表里就是本片的行）。
//! - **上游定位**（文件头逐字）：`media_download.go` 的缓冲路径把每个附件**装两遍** ——
//!   下载回来的整份密文，然后是解密出来的整份明文，两者在上传期间同时活着。引擎把并发媒体解析
//!   上限压在 8 条，而单件资源的帽是 100 MiB，于是一个**完全普通**的工作区能撞到的最坏情形
//!   —— 四个人各发两个大文件 —— 就是 8 × 200 MB 的活堆。在一台自托管的机器上那是一次 OOM，
//!   而它杀掉的那个进程同时还在服务 `Lark`、`Slack` 和 `DingTalk`。那条路径**只**作为
//!   "存储后端没有 `UploadStream`"的回退保留着；两个已发布的实现都有，所以真实部署跑的是本文件。
//!
//! 改成一个临时文件，而**密文根本不落盘**：它从 socket 流进来、逐块解进文件、上传再从那读回来。
//! 每个附件的峰值堆变成**一个缓冲区**，本来会乘起来的那个数被磁盘界住了。
//!
//! 临时文件也是流式上传**之所以可能**的原因：`S3Storage.UploadStream` 要求一个**精确的**
//! `ContentLength`，而明文长度是密文长度减去一个**只有在读完最后一块之后才知道**的填充
//! —— 在上传必须开始的那一刻不可知。磁盘上的一个文件已经回答了这个问题：它的大小**就是**长度。
//!
//! 文件按 `0600` 建、**每一条路径上都删掉**。它们装的是解密后的附件内容，所以它们的权限是这个
//! 功能的一部分，而不是保洁。
//!
//! # 与上游的三点形态差异（登记 `docs/32` §35 的 D6）
//!
//! 1. **`io.Reader` → `futures::Stream`**：本仓的下载体是 `reqwest` 的异步 body
//!    （[`super::media_download::MediaBody`]），所以本函数是 **`async`** 的。上游的
//!    `decryptToFile(…, src io.Reader, …)` 是同步的，因为 Go 的 `http` 响应体天生同步。
//!    语义一字未改：边读边解、只扣住尾巴。
//! 2. **`os.CreateTemp` → 本仓自己造临时文件**：`tempfile` 不在 `mc-channel` 的依赖边里
//!    （M7-0 冻结），所以唯一名字由 `uuid` + 进程 id + 一个进程内计数器拼出来。
//!    语义与上游逐条对齐：**建、`0600`、立刻 unlink**。
//! 3. **`defer out.Close()` 的落法**：Rust 的错误路径不需要调用方关文件（`Err` 里没有句柄）。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::media_crypt::{
    unpad_media, MediaAesKey, MediaCbcDecryptor, MediaCryptError, AES_BLOCK_BYTES, MEDIA_PAD_BLOCK,
};
use super::media_download::{MediaChunkSource, MediaDownloadError};

/// 一次解密读多少密文（上游 `mediaStreamChunk` 256 KiB）。
///
/// 大到相对一个 100 MiB 文件的系统调用开销消失，小到这些缓冲区在进程持有的其它东西旁边
/// 仍然是零头。
pub const MEDIA_STREAM_CHUNK: usize = 256 << 10;

/// 临时文件的名字前缀（上游 `os.CreateTemp` 的 pattern `wecom-media-*.bin`）。
const MEDIA_TEMP_PREFIX: &str = "wecom-media-";
const MEDIA_TEMP_SUFFIX: &str = ".bin";

/// 流式解密失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaStreamError {
    /// 密钥 / 填充层面的失败（上游把 `decodeMediaAESKey` 与 `unpadMedia` 的错误直接往上抛）。
    #[error(transparent)]
    Crypt(#[from] MediaCryptError),
    /// 上游的 body 读取失败（本仓是 stream 上的那一项）——**判决原样带出来**，
    /// 因为"块闸拒了 / 超了帽 / 传输失败"三种结局在上层分属不同的处理。
    #[error(transparent)]
    Download(#[from] MediaDownloadError),
    /// 上游 `fmt.Errorf("wecom: media ciphertext is not a multiple of the block size (%d trailing bytes)")`。
    #[error(
        "wecom: media ciphertext is not a multiple of the block size ({trailing} trailing bytes)"
    )]
    Misaligned { trailing: usize },
    /// 上游 `errors.New("wecom: media ciphertext is empty")`。
    #[error("wecom: media ciphertext is empty")]
    Empty,
    /// 上游 `fmt.Errorf("wecom: media temp file: %w")` —— **这条是回退的判据**
    /// （调用方据此退回缓冲路径，见 [`super::media_ingest`]）。
    #[error("wecom: media temp file could not be created")]
    TempFile,
    /// 写 / rewind 失败（上游各自 `%w` 包一层，本仓合并成一个变体：
    /// 两者都只可能是磁盘的问题，调用方的反应相同）。
    #[error("wecom: media stream i/o failed")]
    Io,
}

/// 进程内唯一名字的计数器（`os.CreateTemp` 的随机后缀的替代，见模块文档差异 2）。
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// 造一个唯一名字并**立刻 unlink** 的临时文件（上游 `os.CreateTemp` + `os.Remove`）。
///
/// 返回的句柄仍然可读可写：文件在进程放手的那一刻消失，**包括崩溃时**。没有任何装了
/// 解密后附件内容的文件被留下来给谁发现。
fn create_unlinked_temp(dir: &Path) -> Result<File, MediaStreamError> {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = format!(
        "{MEDIA_TEMP_PREFIX}{}-{sequence}-{}{MEDIA_TEMP_SUFFIX}",
        std::process::id(),
        uuid::Uuid::new_v4()
    );
    let path = dir.join(name);
    let file = open_private(&path)?;
    // unlink 失败 ⇒ 一个有名字的临时文件会留在盘上（装的还是解密后的附件内容）⇒ 这不是
    // "尽力而为"的一步，失败了就当功能不可用（调用方退回缓冲路径）。
    if std::fs::remove_file(&path).is_err() {
        return Err(MediaStreamError::TempFile);
    }
    Ok(file)
}

/// 按 `0600` 建一个文件（上游 `out.Chmod(0o600)`；非 unix 上没有这个位，退化成"只有本用户"）。
fn open_private(path: &Path) -> Result<File, MediaStreamError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path).map_err(|_| MediaStreamError::TempFile)
}

/// 上游 `decryptToFile`：把 `src` 里的密文解密进一个临时文件，返回**定位到开头**的文件句柄
/// 与明文长度。调用方负责关掉它（它已经被 unlink 了）。
///
/// CBC 必须先看到尾巴才能相信头 —— 填充在最后一块上 —— 所以最后一块被留到源耗尽、那时才
/// 反填充。它前面的一切都是边读边写。
///
/// `dir` 为空 ⇒ 系统临时目录（上游 `os.CreateTemp("")` 的语义）。
///
/// # Errors
///
/// 见 [`MediaStreamError`]；[`MediaStreamError::TempFile`] 是"退回缓冲路径"的信号。
pub async fn decrypt_to_file(
    raw_key: &str,
    src: &mut dyn MediaChunkSource,
    dir: &Path,
) -> Result<(File, i64), MediaStreamError> {
    let key = MediaAesKey::decode(raw_key)?;
    let directory: PathBuf = if dir.as_os_str().is_empty() {
        std::env::temp_dir()
    } else {
        dir.to_path_buf()
    };
    let mut out = create_unlinked_temp(&directory)?;
    let mut decryptor = MediaCbcDecryptor::new(&key);

    // `tail` 扣住明文最后 [`MEDIA_PAD_BLOCK`] 个字节：PKCS#7 的填充最多 32 字节，因此**跨两个
    // AES 块** —— 填充块与密码块在这里不是同一个大小，正是 `media_crypt` 记录的那个坑。
    // 只扣住一个 AES 块（16）只在填充 ≤ 16 时是对的，也就是大约一半的文件。
    let mut tail: Vec<u8> = Vec::with_capacity(MEDIA_PAD_BLOCK + AES_BLOCK_BYTES);
    let mut carry: Vec<u8> = Vec::new();
    let mut written: i64 = 0;

    while let Some(item) = src.next_chunk().await {
        let chunk = item?;
        carry.extend_from_slice(&chunk);
        let usable = carry.len() - carry.len() % AES_BLOCK_BYTES;
        if usable > 0 {
            let mut decrypted = carry[..usable].to_vec();
            decryptor.decrypt_blocks(&mut decrypted);
            emit(&mut out, &decrypted, &mut tail, &mut written)?;
            carry.drain(..usable);
        }
    }

    if !carry.is_empty() {
        return Err(MediaStreamError::Misaligned {
            trailing: carry.len(),
        });
    }
    if written == 0 && tail.is_empty() {
        return Err(MediaStreamError::Empty);
    }
    // 填充就在被扣住的那一段里，而 `unpadMedia` 逐字节校验它 —— 对不上就是密钥错了或 body 被
    // 截断了，而它前面那个文件同样不能被信任。
    let unpadded = unpad_media(&tail)?;
    if !unpadded.is_empty() {
        if out.write_all(&unpadded).is_err() {
            return Err(MediaStreamError::Io);
        }
        written += i64::try_from(unpadded.len()).unwrap_or(i64::MAX);
    }
    if out.seek(SeekFrom::Start(0)).is_err() {
        return Err(MediaStreamError::Io);
    }
    Ok((out, written))
}

/// 把解密出来的一段写进文件，同时**永远扣住最后 32 个字节**（上游 `emit`）。
fn emit(
    out: &mut File,
    plain: &[u8],
    tail: &mut Vec<u8>,
    written: &mut i64,
) -> Result<(), MediaStreamError> {
    tail.extend_from_slice(plain);
    if tail.len() <= MEDIA_PAD_BLOCK {
        return Ok(());
    }
    let cut = tail.len() - MEDIA_PAD_BLOCK;
    if out.write_all(&tail[..cut]).is_err() {
        return Err(MediaStreamError::Io);
    }
    *written += i64::try_from(cut).unwrap_or(i64::MAX);
    tail.drain(..cut);
    Ok(())
}

/// 上游 `peekFile`：从 `f` 的头部读最多 `n` 个字节并**倒回去**，让需要嗅内容类型的调用方
/// 不必持有整个文件。
///
/// # Errors
///
/// [`MediaStreamError::Io`]。
pub fn peek_file(file: &mut File, n: usize) -> Result<Vec<u8>, MediaStreamError> {
    let mut head = vec![0u8; n];
    let read = read_up_to(file, &mut head)?;
    head.truncate(read);
    if file.seek(SeekFrom::Start(0)).is_err() {
        return Err(MediaStreamError::Io);
    }
    Ok(head)
}

/// 读到 EOF 或读满为止（上游用 `io.ReadFull` 再吞掉 `EOF` / `ErrUnexpectedEOF` ⇒ 语义相同）。
fn read_up_to(file: &mut File, buffer: &mut [u8]) -> Result<usize, MediaStreamError> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(_) => return Err(MediaStreamError::Io),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests;
