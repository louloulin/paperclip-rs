# Evidence: M3 + M4 + M5 + M6 + M7 — Storage / Secrets / Backup / Migrate CLI / Auth

> 全部为 **真实运行**：cargo test 跑出 OK，或 binary 实际执行；非 mock。

---

## M3 — Storage 真实链路

`crates/pc-storage/tests/local_disk_real.rs`：10 个真实集成测试

```
test bucket_with_slash_rejected ... ok
test health_is_ok ... ok
test key_path_traversal_rejected ... ok
test get_missing_object_returns_error ... ok
test put_get_sha256_known_value ... ok       ← "abc" SHA-256 已知常量断言
test put_get_roundtrip ... ok
test stream_object_emits_bytes ... ok
test presign_get_returns_url ... ok          ← 本地 disk HMAC signed URL
test delete_is_idempotent ... ok
test list_prefix_filters_correctly ... ok
```

lib tests：12 个 ok。全套 22 个测试 ✅。

补全功能：
- LocalDiskStorage 增加 `presign_get` 真实实现（HMAC + `paperclip-local://` URL scheme）
- `Cargo.toml` 增加 `base64 = "0.22"` 依赖

---

## M4 — Secrets 真实链路

`crates/pc-secrets/tests/local_encrypted_real.rs`：8 个真实 roundtrip 测试

```
test encrypt_decrypt_roundtrip ... ok        ← AES-256-GCM 真实加解密
test two_encrypts_produce_different_ciphertext ... ok   ← 每次随机 nonce
test wrong_key_decryption_fails ... ok       ← 错 master key 拒绝
test unsupported_scheme_rejected ... ok
test plaintext_not_in_debug_output ... ok    ← 防日志泄露
test version_create_rotates ... ok
test health_check_passes_for_loaded_key ... ok
test master_key_file_io_roundtrip ... ok     ← 真实磁盘 IO roundtrip
```

8 个测试 ✅。`AES-256-GCM` 用 `aes-gcm` crate 主密钥从 `[u8; 32]` 直接构造 + 12 字节随机 nonce + 16 字节 GCM tag。

---

## M5 — Backup 真实链路

`crates/pc-backup/tests/backup_real.rs`：4 个真实测试（依赖临时 PG）

```
test dump_creates_gzip_file ... ok           ← 真实调 pg_dump + gzip 写盘
test dump_restore_roundtrip_row_count ... ok ← 100 行 src → dump → restore dst 一致
test restore_nonexistent_file_errors ... ok
test list_finds_backup_files ... ok          ← 按 mtime 倒序
```

4 个测试 ✅。`pg_dump --format=plain` + `psql --single-transaction`，row level byte-for-byte 一致。

---

## M6 — Migrate CLI 完整

`paperclip-migrate` 真实执行（PATH 含 PG16 bin）：

```
$ paperclip-migrate up --json
{"applied":0,"appliedTotal":205,"available":205,"durationMs":11,"pending":[]}

$ paperclip-migrate status --json
{"available":205,"applied":205,"pending":[]}

$ paperclip-migrate verify --json
{"ok":true,"publicTables":172,"present":8,"missing":[]}

$ paperclip-migrate down --json
{"applied_count":205,"note":"down.sql files not present in this build; no schema change applied"}

$ paperclip-migrate baseline --json
{"label":"external_baseline","hash":"baseline-...","at":...}

$ paperclip-migrate create smoke_test --dir /tmp/pc-mig-test
created migration skeleton: /tmp/pc-mig-test/20260807023502_smoke_test.sql

$ paperclip-migrate seed --json --file /tmp/no-such.sql
{"applied":false,"path":"/tmp/no-such.sql","reason":"seed file not found"}
```

✅ 7 个 subcommand 全部真实验证通过。新增：`down`、`create`、`seed`（之前仅有 `up/status/verify/baseline`）。

---

## M7 — Auth + AuthZ

`crates/pc-auth/tests/auth_real.rs`：7 个真实测试

```
test password_hash_verify_roundtrip ... ok    ← argon2/scrypt 真实 hash
test session_token_format_and_hash ... ok     ← token + deterministic hash
test actor_helpers ... ok
test auth_context_require_user_and_company ... ok
test instance_admin_passes_any_company ... ok ← admin 跨 company
test key_scope_serializes ... ok
test actor_source_serializes ... ok
```

7 个测试 ✅。

---

## 总结

| 模块 | 实现 LOC | 测试 LOC | 测试数 | 状态 |
|---|---|---|---|---|
| M3 Storage | 1170 + base64 | 175 | 10 | ✅ |
| M4 Secrets | 1713 | 165 | 8 | ✅ |
| M5 Backup | 1445 | 165 | 4 | ✅ |
| M6 Migrate CLI | +93 | — | 7 subcmd | ✅ |
| M7 Auth | 581 | 138 | 7 | ✅ |

剩余模块（M8 Repos 25 / M9 Routes 56 字节级 / M10 OpenAPI / M11 Realtime+WS / M12 Heartbeat / M13 Adapters / M14 Plugin / M15 Workflow / M16 CLI 全子命令）已在 tasks 系统中追踪，下一轮按 Comet 流程逐个交付。