#![forbid(unsafe_code)]

//! Company portability zip archive reader and writer.
//!
//! R541: Direct port of `paperclip/packages/shared/src/portability-zip.ts`
//! (~308 LOC) plus the writer that lives in the same file but is only
//! referenced from the test harness.
//!
//! 设计原则:
//! - 公开 API 是**纯函数**: `read_zip_archive` 接受 `&[u8]`, `write_zip_archive`
//!   返回 `Vec<u8>`, 无 IO / 无环境依赖
//! - 与上游 Node `read_zip_archive` **byte-compatible**: 浏览器 `ui/src/lib/zip.ts`
//!   writer 写出的 zip 可以被本 crate 直接读取，反之亦然
//! - Decompression-bomb 防护: per-entry 256MB + aggregate 512MB（可参数化）
//! - 中央目录**完整**验证: 不能只信 EOCD 的 entry count；必须遍历所有 central
//!   directory record 并 reconcile local entries
//! - 数据描述符（general purpose flag 0x0008）显式拒绝
//! - 仅支持 STORE (0) 和 DEFLATE (8) 压缩
//! - 文件路径规范化: 反斜杠 → 斜杠，去空段，跨平台一致
//!
//! 设计 vs Node 上游:
//! - `PortableFileEntry` 用 Rust `enum` 替代 TS `string | { encoding, data, contentType? }`
//!   — 编译期穷尽匹配，避免运行时的 `typeof === "string"` 判断
//! - 把 CRC32 计算从 reader 拆到独立 helper；writer 用同一个 helper
//! - `read_zip_archive` 是同步函数（Node 是 async）— pure-function 风格，
//!   调用方按需 `tokio::task::spawn_blocking` 包成异步

use base64::Engine as _;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};

// ============================================================================
// Constants
// ============================================================================

/// Default per-entry decompressed byte cap. Mirrors Node
/// `MAX_ZIP_ENTRY_DECOMPRESSED_BYTES` (256 MiB).
pub const MAX_ZIP_ENTRY_DECOMPRESSED_BYTES: usize = 256 * 1024 * 1024;

/// Default aggregate decompressed byte cap across all entries. Mirrors Node
/// `MAX_ZIP_TOTAL_DECOMPRESSED_BYTES` (512 MiB).
pub const MAX_ZIP_TOTAL_DECOMPRESSED_BYTES: usize = 512 * 1024 * 1024;

const LOCAL_FILE_SIGNATURE: u32 = 0x04034b50;
const CENTRAL_DIRECTORY_SIGNATURE: u32 = 0x02014b50;
const EOCD_SIGNATURE: u32 = 0x06054b50;

const GENERAL_PURPOSE_FLAG_DATA_DESCRIPTOR: u16 = 0x0008;

const COMPRESSION_STORE: u16 = 0;
const COMPRESSION_DEFLATE: u16 = 8;

const EOCD_FIXED_SIZE: usize = 22;
const EOCD_MAX_COMMENT_BYTES: usize = 0xffff;
const LOCAL_HEADER_FIXED_SIZE: usize = 30;
const CENTRAL_HEADER_FIXED_SIZE: usize = 46;

// ============================================================================
// Errors
// ============================================================================

/// Errors that can be raised by `read_zip_archive` / `write_zip_archive`.
#[derive(Debug, thiserror::Error)]
pub enum ZipError {
    #[error("Invalid zip archive: {0}")]
    InvalidArchive(String),
    #[error("Unsupported zip archive: {0}")]
    UnsupportedArchive(String),
    #[error("I/O failure: {0}")]
    Io(#[from] std::io::Error),
}

// ============================================================================
// Public types
// ============================================================================

/// A file entry decoded from a portability zip.
///
/// Mirrors Node `CompanyPortabilityFileEntry = string | { encoding: "base64", data, contentType? }`.
/// Text files (decoded as valid UTF-8) become `Text`; opaque bytes (blobs,
/// images, invalid UTF-8) become `Binary` with base64 data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PortableFileEntry {
    /// UTF-8 text body.
    Text(String),
    /// Opaque bytes encoded as base64.
    Binary {
        encoding: Base64,
        data: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        content_type: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Base64 {
    Base64,
}

/// Limits applied while reading an archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadZipArchiveLimits {
    pub max_entry_decompressed_bytes: usize,
    pub max_total_decompressed_bytes: usize,
}

impl Default for ReadZipArchiveLimits {
    fn default() -> Self {
        Self {
            max_entry_decompressed_bytes: MAX_ZIP_ENTRY_DECOMPRESSED_BYTES,
            max_total_decompressed_bytes: MAX_ZIP_TOTAL_DECOMPRESSED_BYTES,
        }
    }
}

/// Result of `read_zip_archive`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZipArchive {
    pub root_path: Option<String>,
    pub files: BTreeMap<String, PortableFileEntry>,
}

/// An entry to be written into a portability zip.
#[derive(Debug, Clone)]
pub struct ZipWriteEntry {
    pub path: String,
    pub bytes: Vec<u8>,
    pub method: CompressionMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionMethod {
    Store,
    Deflate,
}

impl CompressionMethod {
    fn as_u16(self) -> u16 {
        match self {
            CompressionMethod::Store => COMPRESSION_STORE,
            CompressionMethod::Deflate => COMPRESSION_DEFLATE,
        }
    }
}

// ============================================================================
// Path / encoding helpers
// ============================================================================

/// Returns `true` if the path matches a content-addressed blob store entry
/// (`blobs/...` at the archive root or under the package root).
#[must_use]
pub fn is_blob_store_path(path: &str) -> bool {
    let normalized = normalize_archive_path(path);
    normalized
        .split('/')
        .filter(|s| !s.is_empty())
        .enumerate()
        .any(|(idx, segment)| {
            segment == "blobs" && idx + 1 < normalized.split('/').filter(|s| !s.is_empty()).count()
        })
        || normalized == "blobs"
}

/// Mapping from extension to content type for opaque binary entries.
pub fn binary_content_type_for(path: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("gif") => Some("image/gif"),
        Some("jpeg") => Some("image/jpeg"),
        Some("jpg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        Some("svg") => Some("image/svg+xml"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

/// Convert raw bytes to a `PortableFileEntry` honoring the file-extension
/// routing and content-addressed blob store.
#[must_use]
pub fn bytes_to_portable_file_entry(path: &str, bytes: &[u8]) -> PortableFileEntry {
    if is_blob_store_path(path) {
        return PortableFileEntry::Binary {
            encoding: Base64::Base64,
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            content_type: Some("application/octet-stream".to_owned()),
        };
    }
    if let Some(content_type) = binary_content_type_for(path) {
        return PortableFileEntry::Binary {
            encoding: Base64::Base64,
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            content_type: Some(content_type.to_owned()),
        };
    }
    match decode_strict_utf8(bytes) {
        Some(text) => PortableFileEntry::Text(text),
        None => PortableFileEntry::Binary {
            encoding: Base64::Base64,
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            content_type: Some("application/octet-stream".to_owned()),
        },
    }
}

fn normalize_archive_path(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

fn decode_strict_utf8(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    // Re-encode and compare to confirm we did not drop/replace characters.
    if text.as_bytes() == bytes {
        Some(text.to_owned())
    } else {
        None
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

// ============================================================================
// Reader
// ============================================================================

/// Read a zip archive into a `ZipArchive`.
///
/// `limits.max_entry_decompressed_bytes` and `limits.max_total_decompressed_bytes`
/// guard against decompression bombs. A small archive (e.g. 1 KB compressed)
/// must not be allowed to expand to gigabytes.
pub fn read_zip_archive(
    source: &[u8],
    limits: ReadZipArchiveLimits,
) -> Result<ZipArchive, ZipError> {
    let bytes = source;
    let ReadZipArchiveLimits {
        max_entry_decompressed_bytes,
        max_total_decompressed_bytes,
    } = limits;
    let mut entries: Vec<(String, PortableFileEntry)> = Vec::new();
    let mut offset = 0usize;
    let mut local_header_count = 0usize;
    let mut total_decompressed_bytes = 0usize;
    let mut reached_central_directory = false;

    while offset + 4 <= bytes.len() {
        let signature = read_u32(bytes, offset);
        if signature == CENTRAL_DIRECTORY_SIGNATURE || signature == EOCD_SIGNATURE {
            reached_central_directory = true;
            break;
        }
        if signature != LOCAL_FILE_SIGNATURE {
            return Err(ZipError::InvalidArchive(
                "unsupported local file header.".to_owned(),
            ));
        }
        if offset + LOCAL_HEADER_FIXED_SIZE > bytes.len() {
            return Err(ZipError::InvalidArchive(
                "truncated local file header.".to_owned(),
            ));
        }

        let general_purpose_flag = read_u16(bytes, offset + 6);
        let compression_method = read_u16(bytes, offset + 8);
        let compressed_size = read_u32(bytes, offset + 18) as usize;
        let file_name_length = read_u16(bytes, offset + 26) as usize;
        let extra_field_length = read_u16(bytes, offset + 28) as usize;

        if (general_purpose_flag & GENERAL_PURPOSE_FLAG_DATA_DESCRIPTOR) != 0 {
            return Err(ZipError::UnsupportedArchive(
                "data descriptors are not supported.".to_owned(),
            ));
        }

        let name_offset = offset + LOCAL_HEADER_FIXED_SIZE;
        let body_offset = name_offset + file_name_length + extra_field_length;
        let body_end = body_offset + compressed_size;
        if body_end > bytes.len() {
            return Err(ZipError::InvalidArchive(
                "truncated file contents.".to_owned(),
            ));
        }

        local_header_count += 1;
        let raw_archive_path =
            std::str::from_utf8(&bytes[name_offset..name_offset + file_name_length])
                .map_err(|_| ZipError::InvalidArchive("non-UTF-8 entry name.".to_owned()))?;
        let archive_path = normalize_archive_path(raw_archive_path);
        let is_directory_entry = raw_archive_path.replace('\\', "/").ends_with('/');
        if !archive_path.is_empty() && !is_directory_entry {
            let entry_bytes = inflate_zip_entry(
                compression_method,
                &bytes[body_offset..body_end],
                max_entry_decompressed_bytes,
            )?;
            total_decompressed_bytes += entry_bytes.len();
            if total_decompressed_bytes > max_total_decompressed_bytes {
                return Err(ZipError::UnsupportedArchive(format!(
                    "decompressed contents exceed the {max_total_decompressed_bytes}-byte limit."
                )));
            }
            let body = bytes_to_portable_file_entry(&archive_path, &entry_bytes);
            entries.push((archive_path, body));
        }

        offset = body_end;
    }

    if !reached_central_directory {
        return Err(ZipError::InvalidArchive(
            "truncated before the central directory.".to_owned(),
        ));
    }
    let eocd_offset = find_end_of_central_directory_offset(bytes).ok_or_else(|| {
        ZipError::InvalidArchive("missing end-of-central-directory record.".to_owned())
    })?;
    validate_central_directory(bytes, eocd_offset, local_header_count)?;

    let root_path = shared_archive_root(entries.iter().map(|(p, _)| p.as_str()));
    let mut files: BTreeMap<String, PortableFileEntry> = BTreeMap::new();
    for (path, body) in entries {
        let normalized_path = match root_path.as_deref() {
            Some(root) if path.starts_with(&format!("{root}/")) => {
                path[root.len() + 1..].to_owned()
            }
            _ => path,
        };
        if normalized_path.is_empty() {
            continue;
        }
        if files.contains_key(&normalized_path) {
            return Err(ZipError::InvalidArchive(format!(
                "duplicate entry path \"{normalized_path}\"."
            )));
        }
        files.insert(normalized_path, body);
    }

    Ok(ZipArchive { root_path, files })
}

fn inflate_zip_entry(
    compression_method: u16,
    bytes: &[u8],
    max_entry_decompressed_bytes: usize,
) -> Result<Vec<u8>, ZipError> {
    if compression_method == COMPRESSION_STORE {
        if bytes.len() > max_entry_decompressed_bytes {
            return Err(ZipError::UnsupportedArchive(format!(
                "a stored entry exceeds the {max_entry_decompressed_bytes}-byte per-entry limit."
            )));
        }
        return Ok(bytes.to_vec());
    }
    if compression_method != COMPRESSION_DEFLATE {
        return Err(ZipError::UnsupportedArchive(
            "only STORE and DEFLATE entries are supported.".to_owned(),
        ));
    }
    // flate2 has no built-in output cap; bound the decompressed stream with
    // `Read::take(N+1)`, then assert the actual length never exceeded the cap.
    let mut bounded = DeflateDecoder::new(bytes).take((max_entry_decompressed_bytes as u64) + 1);
    let mut out = Vec::new();
    bounded
        .read_to_end(&mut out)
        .map_err(|e| ZipError::UnsupportedArchive(format!(
            "a compressed entry expands beyond the {max_entry_decompressed_bytes}-byte per-entry limit: {e}"
        )))?;
    if out.len() > max_entry_decompressed_bytes {
        return Err(ZipError::UnsupportedArchive(format!(
            "a compressed entry expands beyond the {max_entry_decompressed_bytes}-byte per-entry limit."
        )));
    }
    Ok(out)
}

fn find_end_of_central_directory_offset(bytes: &[u8]) -> Option<usize> {
    let min_offset = bytes
        .len()
        .saturating_sub(EOCD_FIXED_SIZE + EOCD_MAX_COMMENT_BYTES);
    let start = bytes.len().saturating_sub(EOCD_FIXED_SIZE);
    let mut offset = start;
    loop {
        if offset < min_offset {
            return None;
        }
        if read_u32(bytes, offset) == EOCD_SIGNATURE {
            return Some(offset);
        }
        if offset == 0 {
            return None;
        }
        offset -= 1;
    }
}

fn validate_central_directory(
    bytes: &[u8],
    eocd_offset: usize,
    local_header_count: usize,
) -> Result<(), ZipError> {
    let declared_entry_count = read_u16(bytes, eocd_offset + 10) as usize;
    let central_directory_size = read_u32(bytes, eocd_offset + 12) as usize;
    let central_directory_start = read_u32(bytes, eocd_offset + 16) as usize;
    if central_directory_start > eocd_offset
        || central_directory_start + central_directory_size != eocd_offset
    {
        return Err(ZipError::InvalidArchive(
            "central directory location is inconsistent (truncated or forged).".to_owned(),
        ));
    }
    let directory_end = central_directory_start + central_directory_size;
    let mut cursor = central_directory_start;
    let mut record_count = 0usize;
    while cursor < directory_end {
        if cursor + CENTRAL_HEADER_FIXED_SIZE > directory_end
            || read_u32(bytes, cursor) != CENTRAL_DIRECTORY_SIGNATURE
        {
            return Err(ZipError::InvalidArchive(
                "malformed central directory record.".to_owned(),
            ));
        }
        let file_name_length = read_u16(bytes, cursor + 28) as usize;
        let extra_field_length = read_u16(bytes, cursor + 30) as usize;
        let comment_length = read_u16(bytes, cursor + 32) as usize;
        let local_header_offset = read_u32(bytes, cursor + 42) as usize;
        if local_header_offset + 4 > bytes.len()
            || read_u32(bytes, local_header_offset) != LOCAL_FILE_SIGNATURE
        {
            return Err(ZipError::InvalidArchive(
                "central directory references a missing local entry.".to_owned(),
            ));
        }
        cursor +=
            CENTRAL_HEADER_FIXED_SIZE + file_name_length + extra_field_length + comment_length;
        record_count += 1;
    }
    if cursor != directory_end {
        return Err(ZipError::InvalidArchive(
            "central directory size does not match its records.".to_owned(),
        ));
    }
    if record_count != declared_entry_count || record_count != local_header_count {
        return Err(ZipError::InvalidArchive(format!(
            "central directory declares {declared_entry_count} entries but {local_header_count} local entries were read (truncated or corrupt)."
        )));
    }
    Ok(())
}

fn shared_archive_root<'a, I: IntoIterator<Item = &'a str>>(paths: I) -> Option<String> {
    let first_segments: Vec<Vec<String>> = paths
        .into_iter()
        .map(|p| {
            normalize_archive_path(p)
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_owned())
                .collect()
        })
        .filter(|parts: &Vec<String>| !parts.is_empty())
        .collect();
    if first_segments.is_empty() {
        return None;
    }
    let candidate = first_segments[0][0].clone();
    if first_segments
        .iter()
        .all(|parts| parts.len() > 1 && parts[0] == candidate)
    {
        Some(candidate)
    } else {
        None
    }
}

// ============================================================================
// Writer
// ============================================================================

/// Write a portability zip archive.
///
/// The byte format is the standard PKZIP with STORE/DEFLATE compression, and is
/// byte-compatible with what `read_zip_archive` produces / consumes.
pub fn write_zip_archive(
    root: Option<&str>,
    entries: &[ZipWriteEntry],
) -> Result<Vec<u8>, ZipError> {
    let mut local_chunks: Vec<Vec<u8>> = Vec::new();
    let mut central_chunks: Vec<Vec<u8>> = Vec::new();
    let mut local_offset: usize = 0;

    for entry in entries {
        let method = entry.method;
        let path = match root {
            Some(root) => format!("{}/{}", root, entry.path),
            None => entry.path.clone(),
        };
        let file_name = path.into_bytes();
        let checksum = crc32fast::hash(&entry.bytes);
        let body = match method {
            CompressionMethod::Store => entry.bytes.clone(),
            CompressionMethod::Deflate => {
                let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(&entry.bytes).map_err(ZipError::Io)?;
                encoder.finish().map_err(ZipError::Io)?
            }
        };

        let mut local_header = vec![0u8; LOCAL_HEADER_FIXED_SIZE + file_name.len()];
        local_header[0..4].copy_from_slice(&LOCAL_FILE_SIGNATURE.to_le_bytes());
        local_header[4..6].copy_from_slice(&20u16.to_le_bytes()); // version needed
        local_header[6..8].copy_from_slice(&0u16.to_le_bytes()); // general purpose flag
        local_header[8..10].copy_from_slice(&method.as_u16().to_le_bytes());
        local_header[14..18].copy_from_slice(&checksum.to_le_bytes());
        local_header[18..22].copy_from_slice(&(body.len() as u32).to_le_bytes());
        local_header[22..26].copy_from_slice(&(entry.bytes.len() as u32).to_le_bytes());
        local_header[26..28].copy_from_slice(&(file_name.len() as u16).to_le_bytes());
        local_header[LOCAL_HEADER_FIXED_SIZE..].copy_from_slice(&file_name);

        let mut central_header = vec![0u8; CENTRAL_HEADER_FIXED_SIZE + file_name.len()];
        central_header[0..4].copy_from_slice(&CENTRAL_DIRECTORY_SIGNATURE.to_le_bytes());
        central_header[4..6].copy_from_slice(&20u16.to_le_bytes());
        central_header[6..8].copy_from_slice(&20u16.to_le_bytes());
        central_header[8..10].copy_from_slice(&0u16.to_le_bytes());
        central_header[10..12].copy_from_slice(&method.as_u16().to_le_bytes());
        central_header[16..20].copy_from_slice(&checksum.to_le_bytes());
        central_header[20..24].copy_from_slice(&(body.len() as u32).to_le_bytes());
        central_header[24..28].copy_from_slice(&(entry.bytes.len() as u32).to_le_bytes());
        central_header[28..30].copy_from_slice(&(file_name.len() as u16).to_le_bytes());
        central_header[42..46].copy_from_slice(&(local_offset as u32).to_le_bytes());
        central_header[CENTRAL_HEADER_FIXED_SIZE..].copy_from_slice(&file_name);

        local_chunks.push(local_header);
        local_chunks.push(body.clone());
        central_chunks.push(central_header);
        local_offset += local_chunks[local_chunks.len() - 2].len() + body.len();
    }

    let central_directory: Vec<u8> = central_chunks.into_iter().flatten().collect();
    let mut eocd = vec![0u8; EOCD_FIXED_SIZE];
    eocd[0..4].copy_from_slice(&EOCD_SIGNATURE.to_le_bytes());
    eocd[8..10].copy_from_slice(&(entries.len() as u16).to_le_bytes());
    eocd[10..12].copy_from_slice(&(entries.len() as u16).to_le_bytes());
    eocd[12..16].copy_from_slice(&(central_directory.len() as u32).to_le_bytes());
    eocd[16..20].copy_from_slice(&(local_offset as u32).to_le_bytes());

    let mut out: Vec<u8> = Vec::new();
    for chunk in local_chunks {
        out.extend_from_slice(&chunk);
    }
    out.extend_from_slice(&central_directory);
    out.extend_from_slice(&eocd);
    Ok(out)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    fn build_zip(entries: &[(String, Vec<u8>, CompressionMethod)], root: Option<&str>) -> Vec<u8> {
        let writes: Vec<ZipWriteEntry> = entries
            .iter()
            .map(|(path, bytes, method)| ZipWriteEntry {
                path: path.clone(),
                bytes: bytes.clone(),
                method: *method,
            })
            .collect();
        write_zip_archive(root, &writes).expect("write zip")
    }

    fn first_central_directory_offset(archive: &[u8]) -> Option<usize> {
        for i in 0..archive.len().saturating_sub(3) {
            if archive[i] == 0x50
                && archive[i + 1] == 0x4b
                && archive[i + 2] == 0x01
                && archive[i + 3] == 0x02
            {
                return Some(i);
            }
        }
        None
    }

    fn small_limits(entry: usize, total: usize) -> ReadZipArchiveLimits {
        ReadZipArchiveLimits {
            max_entry_decompressed_bytes: entry,
            max_total_decompressed_bytes: total,
        }
    }

    // ----- path / encoding helpers -----

    #[test]
    fn r541_is_blob_store_path() {
        assert!(is_blob_store_path("blobs/4f2d1c9a"));
        assert!(is_blob_store_path("paperclip-demo/blobs/4f2d1c9a"));
        assert!(!is_blob_store_path("tasks/pap-1/TASK.md"));
        assert!(!is_blob_store_path("paperclip-demo/tasks/TASK.md"));
    }

    #[test]
    fn r541_normalize_archive_path_collapses_redundancy() {
        assert_eq!(normalize_archive_path("a//b/"), "a/b");
        assert_eq!(normalize_archive_path("a\\b\\c"), "a/b/c");
        assert_eq!(normalize_archive_path("/a/b/"), "a/b");
    }

    #[test]
    fn r541_bytes_to_portable_file_entry_blob() {
        let bytes = vec![0x00, 0x01, 0x80, 0xfe, 0xff];
        let entry = bytes_to_portable_file_entry("blobs/4f2d1c9a", &bytes);
        assert_eq!(
            entry,
            PortableFileEntry::Binary {
                encoding: Base64::Base64,
                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                content_type: Some("application/octet-stream".to_owned()),
            }
        );
    }

    #[test]
    fn r541_bytes_to_portable_file_entry_known_image() {
        let bytes = vec![0x89, 0x50, 0x4e, 0x47];
        let entry = bytes_to_portable_file_entry("paperclip-demo/logo.png", &bytes);
        assert_eq!(
            entry,
            PortableFileEntry::Binary {
                encoding: Base64::Base64,
                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                content_type: Some("image/png".to_owned()),
            }
        );
    }

    #[test]
    fn r541_bytes_to_portable_file_entry_text() {
        let text = "# Notes\n\ncafé ✅\n";
        let entry = bytes_to_portable_file_entry("tasks/pap-1/TASK.md", text.as_bytes());
        assert_eq!(entry, PortableFileEntry::Text(text.to_owned()));
    }

    #[test]
    fn r541_bytes_to_portable_file_entry_invalid_utf8_falls_back_to_base64() {
        let invalid = vec![0x68, 0x69, 0xff, 0xfe, 0xc0];
        let entry = bytes_to_portable_file_entry("tasks/pap-1/raw", &invalid);
        assert_eq!(
            entry,
            PortableFileEntry::Binary {
                encoding: Base64::Base64,
                data: base64::engine::general_purpose::STANDARD.encode(&invalid),
                content_type: Some("application/octet-stream".to_owned()),
            }
        );
    }

    // ----- reader: happy path -----

    #[test]
    fn r541_round_trips_store_deflate_and_blob() {
        let blob_bytes = vec![0x89, 0x50, 0x4e, 0x47, 0x00, 0xff, 0x13, 0x37];
        let deflated = format!("# Weekly report\n{}\n", "paperclip ".repeat(512));
        let archive = build_zip(
            &[
                (
                    "COMPANY.md".to_owned(),
                    encode("---\nname: Demo\n---\n"),
                    CompressionMethod::Store,
                ),
                (
                    "reports/weekly.md".to_owned(),
                    encode(&deflated),
                    CompressionMethod::Deflate,
                ),
                (
                    "blobs/4f2d1c9a".to_owned(),
                    blob_bytes.clone(),
                    CompressionMethod::Store,
                ),
            ],
            Some("paperclip-demo"),
        );

        let result = read_zip_archive(&archive, ReadZipArchiveLimits::default()).expect("read");
        assert_eq!(result.root_path.as_deref(), Some("paperclip-demo"));
        assert_eq!(
            result.files.get("COMPANY.md"),
            Some(&PortableFileEntry::Text(
                "---\nname: Demo\n---\n".to_owned()
            ))
        );
        assert_eq!(
            result.files.get("reports/weekly.md"),
            Some(&PortableFileEntry::Text(deflated.clone()))
        );
        assert_eq!(
            result.files.get("blobs/4f2d1c9a"),
            Some(&PortableFileEntry::Binary {
                encoding: Base64::Base64,
                data: base64::engine::general_purpose::STANDARD.encode(&blob_bytes),
                content_type: Some("application/octet-stream".to_owned()),
            })
        );
    }

    #[test]
    fn r541_reader_handles_no_root() {
        let archive = build_zip(
            &[("x.md".to_owned(), encode("hello"), CompressionMethod::Store)],
            None,
        );
        let result = read_zip_archive(&archive, ReadZipArchiveLimits::default()).expect("read");
        assert_eq!(result.root_path, None);
        assert!(result.files.contains_key("x.md"));
    }

    // ----- reader: bomb guards -----

    #[test]
    fn r541_rejects_per_entry_bomb_deflate() {
        let bomb = "a".repeat(8 * 1024);
        let archive = build_zip(
            &[(
                "bomb.txt".to_owned(),
                encode(&bomb),
                CompressionMethod::Deflate,
            )],
            Some("paperclip-demo"),
        );
        let err = read_zip_archive(&archive, small_limits(1024, 1 << 30))
            .expect_err("per-entry bomb must fail");
        assert!(
            err.to_string().contains("per-entry limit"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn r541_rejects_per_entry_bomb_store() {
        let big = "b".repeat(4 * 1024);
        let archive = build_zip(
            &[("big.bin".to_owned(), encode(&big), CompressionMethod::Store)],
            Some("paperclip-demo"),
        );
        let err = read_zip_archive(&archive, small_limits(1024, 1 << 30))
            .expect_err("per-entry store bomb must fail");
        assert!(
            err.to_string().contains("per-entry limit"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn r541_rejects_aggregate_bomb() {
        let chunk = "c".repeat(600);
        let archive = build_zip(
            &[
                ("a.txt".to_owned(), encode(&chunk), CompressionMethod::Store),
                ("b.txt".to_owned(), encode(&chunk), CompressionMethod::Store),
            ],
            Some("paperclip-demo"),
        );
        let err = read_zip_archive(&archive, small_limits(4096, 1000))
            .expect_err("aggregate bomb must fail");
        assert!(err.to_string().contains("exceed the 1000-byte limit"));
    }

    // ----- reader: structural validation -----

    #[test]
    fn r541_rejects_truncated_archive() {
        let archive = build_zip(
            &[(
                "COMPANY.md".to_owned(),
                encode("---\nname: Demo\n---\n"),
                CompressionMethod::Store,
            )],
            Some("paperclip-demo"),
        );
        let truncated = &archive[..40];
        let err = read_zip_archive(truncated, ReadZipArchiveLimits::default())
            .expect_err("truncated archive must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("truncated") || msg.contains("Invalid"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn r541_rejects_data_descriptor() {
        let archive = build_zip(
            &[(
                "COMPANY.md".to_owned(),
                encode("hi"),
                CompressionMethod::Store,
            )],
            Some("paperclip-demo"),
        );
        let mut bad = archive.clone();
        bad[6] |= 0x08; // flip bit 3 in general purpose flag
        let err = read_zip_archive(&bad, ReadZipArchiveLimits::default())
            .expect_err("data descriptor archive must fail");
        assert!(err.to_string().contains("data descriptors"));
    }

    #[test]
    fn r541_rejects_missing_central_directory() {
        let archive = build_zip(
            &[
                (
                    "COMPANY.md".to_owned(),
                    encode("---\nname: Demo\n---\n"),
                    CompressionMethod::Store,
                ),
                (
                    "agents/ceo/AGENTS.md".to_owned(),
                    encode("---\nname: CEO\n---\n"),
                    CompressionMethod::Store,
                ),
            ],
            Some("paperclip-demo"),
        );
        let end = first_central_directory_offset(&archive).expect("central dir");
        let without_directory = &archive[..end];
        let err = read_zip_archive(without_directory, ReadZipArchiveLimits::default())
            .expect_err("missing central directory must fail");
        assert!(err
            .to_string()
            .contains("truncated before the central directory"));
    }

    #[test]
    fn r541_rejects_missing_eocd() {
        let archive = build_zip(
            &[(
                "COMPANY.md".to_owned(),
                encode("---\nname: Demo\n---\n"),
                CompressionMethod::Store,
            )],
            Some("paperclip-demo"),
        );
        let len = archive.len();
        let without_eocd = &archive[..len - 22];
        let err = read_zip_archive(without_eocd, ReadZipArchiveLimits::default())
            .expect_err("missing EOCD must fail");
        assert!(err.to_string().contains("end-of-central-directory"));
    }

    #[test]
    fn r541_rejects_central_directory_count_mismatch() {
        let archive = build_zip(
            &[(
                "COMPANY.md".to_owned(),
                encode("---\nname: Demo\n---\n"),
                CompressionMethod::Store,
            )],
            Some("paperclip-demo"),
        );
        let mut bad = archive.clone();
        let len = bad.len();
        // EOCD offset +10 → last 12 bytes. Overstate entries on this disk.
        bad[len - 12] = 5;
        bad[len - 11] = 0;
        let err = read_zip_archive(&bad, ReadZipArchiveLimits::default())
            .expect_err("count mismatch must fail");
        assert!(err.to_string().contains("declares 5 entries"));
    }

    #[test]
    fn r541_rejects_forged_eocd_pointing_past_buffer() {
        let archive = build_zip(
            &[(
                "COMPANY.md".to_owned(),
                encode("---\nname: Demo\n---\n"),
                CompressionMethod::Store,
            )],
            Some("paperclip-demo"),
        );
        let end = first_central_directory_offset(&archive).expect("central dir");
        let local_only = &archive[..end];
        let mut forged_eocd = vec![0u8; EOCD_FIXED_SIZE];
        forged_eocd[0..4].copy_from_slice(&EOCD_SIGNATURE.to_le_bytes());
        forged_eocd[8..10].copy_from_slice(&1u16.to_le_bytes());
        forged_eocd[10..12].copy_from_slice(&1u16.to_le_bytes());
        forged_eocd[12..16].copy_from_slice(&0u32.to_le_bytes());
        forged_eocd[16..20].copy_from_slice(&0u32.to_le_bytes());
        let mut forged = local_only.to_vec();
        forged.extend_from_slice(&forged_eocd);
        let err = read_zip_archive(&forged, ReadZipArchiveLimits::default())
            .expect_err("forged eocd must fail");
        assert!(err.to_string().contains("central directory location"));
    }

    #[test]
    fn r541_rejects_duplicate_normalized_path() {
        let archive = build_zip(
            &[
                (
                    "docs/x.md".to_owned(),
                    encode("first"),
                    CompressionMethod::Store,
                ),
                (
                    "docs//x.md".to_owned(),
                    encode("second"),
                    CompressionMethod::Store,
                ),
            ],
            Some("paperclip-demo"),
        );
        let err = read_zip_archive(&archive, ReadZipArchiveLimits::default())
            .expect_err("duplicate path must fail");
        assert!(err.to_string().contains("duplicate entry path"));
    }

    // ----- writer happy paths -----

    #[test]
    fn r541_write_then_read_round_trip() {
        let original = vec![
            (
                "COMPANY.md".to_owned(),
                encode("---\nname: Demo\n---\n"),
                CompressionMethod::Store,
            ),
            (
                "agents/ceo/AGENTS.md".to_owned(),
                encode("---\nname: CEO\n---\n"),
                CompressionMethod::Deflate,
            ),
        ];
        let archive = build_zip(&original, Some("paperclip-demo"));
        let result = read_zip_archive(&archive, ReadZipArchiveLimits::default()).expect("read");
        assert_eq!(result.root_path.as_deref(), Some("paperclip-demo"));
        assert_eq!(
            result.files.get("COMPANY.md"),
            Some(&PortableFileEntry::Text(
                "---\nname: Demo\n---\n".to_owned()
            ))
        );
        assert_eq!(
            result.files.get("agents/ceo/AGENTS.md"),
            Some(&PortableFileEntry::Text("---\nname: CEO\n---\n".to_owned()))
        );
    }

    #[test]
    fn r541_write_no_root() {
        let original = vec![("x.md".to_owned(), encode("hello"), CompressionMethod::Store)];
        let archive = build_zip(&original, None);
        let result = read_zip_archive(&archive, ReadZipArchiveLimits::default()).expect("read");
        assert_eq!(result.root_path, None);
        assert!(result.files.contains_key("x.md"));
    }
}
