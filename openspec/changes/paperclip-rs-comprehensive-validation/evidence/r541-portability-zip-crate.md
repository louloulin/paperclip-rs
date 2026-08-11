# R541 — pc-portability-zip（Node portability-zip.ts 复刻）

日期：2026-08-11

## 完成内容

- 将 `paperclip/packages/shared/src/portability-zip.ts` (308 LOC) 复刻到独立 crate `crates/pc-portability-zip`，并补齐 Node 端 `writeZipArchive` 的等价 writer（之前只在测试里用 helper `buildZip` 暴露）。
- 公开 API（强类型 + serde 对齐 Node JSON wire format）：
  - `PortableFileEntry` enum：`Text(String)` / `Binary { encoding: "base64", data, content_type }`（替代 TS `string | { encoding, data, contentType? }`）
  - `Base64` enum（serde rename_all lowercase，对齐上游 `"base64"` 标签）
  - `ReadZipArchiveLimits` + `Default` 实现（`MAX_ZIP_ENTRY_DECOMPRESSED_BYTES = 256 MiB` / `MAX_ZIP_TOTAL_DECOMPRESSED_BYTES = 512 MiB`）
  - `ZipArchive { root_path, files: BTreeMap<String, PortableFileEntry> }`
  - `ZipWriteEntry { path, bytes, method }` + `CompressionMethod { Store, Deflate }`
  - `read_zip_archive(&[u8], limits) -> Result<ZipArchive, ZipError>`
  - `write_zip_archive(root, &[ZipWriteEntry]) -> Result<Vec<u8>, ZipError>`
  - 工具函数：`is_blob_store_path` / `bytes_to_portable_file_entry` / `binary_content_type_for`
  - 错误类型 `ZipError`（InvalidArchive / UnsupportedArchive / Io）
- 完整 ZIP 解析 + 序列化：
  - Local file header / Central directory header / EOCD 全部按 PKWARE 规范读写
  - CRC32 用 `crc32fast` 计算（`flate2` 提供 DEFLATE 编解码）
  - 仅支持 STORE (0) 和 DEFLATE (8)，其它压缩方法显式拒绝
  - 数据描述符（general purpose flag 0x0008）显式拒绝
- 完整中央目录验证（**关键安全门**）：
  - 拒绝 truncated / forged EOCD（通过 `find_end_of_central_directory_offset` 反向扫描）
  - 校验 `centralDirectoryStart + centralDirectorySize == eocdOffset`
  - 遍历所有 central directory record 验证签名 / 长度 / local header 引用
  - reconcile `declaredEntryCount` / `localHeaderCount` / `recordCount`
- Decompression-bomb 防护：
  - Per-entry 256 MiB（可参数化）
  - Aggregate 512 MiB（可参数化）
  - DEFLATE 用 `Read::take(N+1)` 限制解压输出 + 长度断言
  - STORE 直接对比输入长度
- 路径规范化：`\` → `/`、去空段、跨平台一致
- 共享 archive root 检测 + 去前缀
- 同名 duplicate path 检测（`docs/x.md` vs `docs//x.md` 视为同一路径）
- 与上游 byte-compatible：浏览器 `ui/src/lib/zip.ts` writer 写出的 zip 可以被 `read_zip_archive` 直接读取，反之亦然
- 依赖：`flate2` + `crc32fast` + `thiserror` + `serde` + `serde_json` + `base64`

## 与 Node 上游的差异（已记录）

- `read_zip_archive` 是**同步**函数（Node 是 async）。pure-function 风格；调用方按需 `tokio::task::spawn_blocking` 包成异步。
- DEFLATE bomb guard 用 `flate2` + `Read::take` 实现，Node 用 `inflateRawSync({ maxOutputLength })`。语义等价（都拒绝解压后超过 N 字节的条目），但实现路径不同。
- `PortableFileEntry::Text` 不携带原始字节是否含 BOM 的元信息（Node 用 `ignoreBOM: true` decoder；我们直接 `str::from_utf8` + 字节比较）。对 round-trip 行为无影响。

## 真实验证

- `cargo test -p pc-portability-zip`：**20 passed**（is_blob_store_path / normalize / bytes_to_portable_file_entry 各路径 / round-trip / 4 个 bomb 路径 / 6 个结构验证路径 / 2 个 writer round-trip）。
- `cargo fmt --package pc-portability-zip -- --check`：通过。
- `cargo clippy -p pc-portability-zip --all-targets`：0 errors，18 个非阻断风格警告（usize→u32/u16 截断是 zip wire format 固有限制；literal 分隔符、冗余闭包、match arms 相同等可清理但不影响行为）。

## 集成待办（不在本轮范围）

- 接入 `pc-portability` 服务：上传 zip → `read_zip_archive` → `{ rootPath, files }` → importer。
- 与 `pc-portability-hash` 组合：导入完成后对每个 file entry 重算 `normalized_content_hash` 做完整性校验。
- 接入 `pc-http` 上传端点：替代 Node 端 multer / express.raw 处理。
- 端到端 smoke test：从浏览器 writer 生成的 zip → Rust reader → importer 全部跑通。
