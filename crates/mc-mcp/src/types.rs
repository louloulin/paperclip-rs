//! remote MCP 的 wire 类型（JSON-RPC 信封 + 工具描述 + 调用结果）。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。
//! - **上游**：`pkg/remotemcp/types.go`（55 行）—— [`Tool`] / [`Connection`] /
//!   `PluginContributionPrefix` / `DigestBytes` 四个条目逐字对应。
//! - **本仓约定**：这些类型**只用于线格式**，不是领域实体（不要给它们加 `Id`/`Timestamp`
//!   之类的领域包装，也不要把它们放进 `mc-core`）；JSON-RPC 的 `id` 用 `u64` 自增即可；
//!   未知字段**忽略**（MCP 还在演进，解析失败会误伤）。
//! - **与落库的分工**：调用结果要落 `plugin_invocation`（`status` / `latency_ms` / `error`），
//!   但**不落**原始 payload（表里没有这个列，也不该偷偷塞进 `error`）。
//! - **不做什么**：不定义工具的 UI 呈现（那是浏览器侧 `packages/plugin-sdk` 的事）。
//!
//! ## 摘要口径（`DigestBytes`）
//!
//! [`digest_bytes`] 是 `sha256:<hex>`：`mcp_approvals` 里钉住的是**管理员当时采纳的那份
//! input schema**，`tools/list` 再来时摘要不一致即视为「没采纳过」（契约漂移要重新采纳，
//! 不是静默沿用旧授权）。
//!
//! ⚠️ **本地与上游的算法实现不同、字节形态相同**：上游 `crypto/sha256`，本地这一版是
//! `mc-mcp` 内的**窄口径 SHA-256（FIPS 180-4，带 NIST 向量测试）**。原因不是取舍偏好：
//! M6-0 anchor 冻结了本 crate 的 manifest（`[dependencies]` 里没有 `sha2`），而本片
//! **不得**改 `Cargo.toml`/`Cargo.lock`（同一时段在飞的 M5-9 也写 `Cargo.lock`）。
//! 摘要是**进程内自洽**的（签发端与校验端都是本 crate），不是跨实现契约 —— 所以
//! 「形态必须逐字（`sha256:` + 64 位小写 hex）」而「实现可以本地」。
//! 待 `docs/15` §8.4 仲裁给 `mc-mcp` 加一条 `sha2` 依赖边后，本实现应整块删掉换成
//! `sha2::Sha256`（登记在交付说明里，属 M6-INT 的收口项）。
//!
//! 行预算（门 ⑩）：预计 320 行以内（含 SHA-256 已知向量测试与线格式用例）。
//! ⚠️ anchor 原写「160 行」偏小：`DigestBytes` 的本地实现与两类测试都在本文件（上游
//! `types.go` 也只有这三件事，但 Go 的 `crypto/sha256` 是一行）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 由**已安装插件**贡献的连接 id 前缀（上游 `PluginContributionPrefix`）。
///
/// 两类连接共用同一个 [`Connection`] 形状与同一个 broker，但**凭据存放位置不同、路由
/// 不同**：daemon 在拨号那一刻手上只有 `contribution_id`，所以必须由 id 自己说明它是哪一类。
/// 用**字符串形状**去猜（例如「含冒号 ⇒ 云签发」）会让一个恰好含冒号的云签发 id 落到插件
/// 路由上 —— 那是一次凭据串味的越权。
pub const PLUGIN_CONTRIBUTION_PREFIX: &str = "plugin:";

/// 连接传输档（上游 `Connection.Transport` 的取值；本波只有 `http` 一种）。
pub const TRANSPORT_HTTP: &str = "http";

/// 失败策略（上游 `Connection.FailurePolicy` 的取值）。
///
/// `optional` = 插件自带的 MCP 服务器挂了**不许拖垮整个任务**：agent 应该还能继续做
/// issue 上的活（与 http hook 失败等价于「一次工具错误」同理）。
pub const FAILURE_POLICY_OPTIONAL: &str = "optional";

/// 一个被管理员采纳、并用 schema 摘要钉住远端契约的工具。
///
/// `schema_digest` 冻结的是「当时批准的那份 input schema」——daemon broker 在任务启动时
/// 拿 [`crate::client::discover`] 的结果与它逐条比对，远端换了 schema 就拒绝启动，而不是
/// 让一个已经在跑的任务悄悄用上新契约。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    /// 上游是 `description,omitempty` ⇒ 空串**不出现在 JSON 里**。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// 上游 `json.RawMessage`：原样存 schema（本仓用 [`Value`]，规范化见
    /// [`crate::client::canonical_json`]）。
    #[serde(default)]
    pub input_schema: Value,
    /// `sha256:<hex>`，见 [`digest_bytes`]。
    pub schema_digest: String,
    /// 上游 `risk,omitempty`；本波上游没有写入点，故默认空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub risk: String,
}

/// claim 期、**按任务**下发的连接元数据。
///
/// **凭据故意不在这个类型里**：daemon broker 在即将出网的那一刻才去取（`resolve_credential`
/// 回调）。放进 wire 形状等于把长期密钥塞进 claim payload、随任务记录与日志扩散。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub installation_id: String,
    /// [`PLUGIN_CONTRIBUTION_PREFIX`] 开头的 id（插件贡献）或云签发的 id。
    pub contribution_id: String,
    /// 展示名（上游 `PluginToolName(manifest.Key, hook.Key)`）。
    pub contribution_key: String,
    pub config_id: String,
    pub config_revision: i64,
    /// MCP 服务器端点（`https://`，开发态 origin 例外，见 [`crate::devorigin`]）。
    pub endpoint: String,
    /// 给用户看的配置（**不含**密钥）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_config: Option<Value>,
    pub transport: String,
    /// 本 build 支持的协议版本优先级（空 ⇒ 用
    /// [`crate::client::supported_protocol_versions`]）。
    #[serde(default)]
    pub protocol_versions: Vec<String>,
    /// 管理员同意界面上展示过的**同一份**精确主机集合；broker 拨号时再查一遍，
    /// 于是「manifest 事后改了自己的 hook」也到不了新地方。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoint_allowed_hosts: Vec<String>,
    /// 需要注入凭据时的请求头名（空 ⇒ 不需要凭据）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub credential_header: String,
    pub approved_tools: Vec<Tool>,
    /// [`crate::client::tool_set_digest`]：钉住**整集合**，不只钉单个工具。
    #[serde(default)]
    pub tool_schema_digest: String,
    pub failure_policy: String,
}

impl Connection {
    /// 插件贡献的连接 id：`plugin:<installation_id>:<hook_key>`（上游
    /// `plugin_mcp_transport.go` 的拼接处）。
    #[must_use]
    pub fn plugin_contribution_id(installation_id: &str, hook_key: &str) -> String {
        format!("{PLUGIN_CONTRIBUTION_PREFIX}{installation_id}:{hook_key}")
    }

    /// 这条连接是不是插件贡献的（**只认前缀**，不看字符串里有没有冒号）。
    #[must_use]
    pub fn is_plugin_contribution(&self) -> bool {
        self.contribution_id.starts_with(PLUGIN_CONTRIBUTION_PREFIX)
    }
}

/// 内容摘要：`sha256:<64 位小写 hex>`（上游 `DigestBytes`）。
#[must_use]
pub fn digest_bytes(content: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(content))
}

const SHA256_INITIAL_STATE: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

const SHA256_ROUND_CONSTANTS: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// 小写十六进制数字表 —— 哈希输出只走这一条编码路径。
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// `SHA-256(content)` 的原始 32 字节。
///
/// 见本文件头部「摘要口径」：这不是为了炫技，而是因为本 crate 的 manifest 被 anchor
/// 冻结、且本片不得改 `Cargo.lock`。算法照 FIPS 180-4 逐字实现，并用 NIST 的
/// 已知向量 + 本仓 `mc-core::hash` 的 `sha256("hello")` 交叉向量钉住。
///
/// 两个用点共用这里：工具集摘要（[`digest_bytes`]，十六进制）与 OAuth PKCE
/// （`oauth::build_authorization_url`，base64url）。
#[must_use]
pub(crate) fn sha256(content: &[u8]) -> [u8; 32] {
    let mut padded = Vec::with_capacity(content.len() + 72);
    padded.extend_from_slice(content);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&(content.len() as u64).wrapping_mul(8).to_be_bytes());

    let mut state = SHA256_INITIAL_STATE;
    let mut schedule = [0_u32; 64];
    let (blocks, _tail) = padded.as_chunks::<64>();
    for block in blocks {
        for (index, word) in schedule.iter_mut().take(16).enumerate() {
            let start = index * 4;
            *word = u32::from_be_bytes([
                block[start],
                block[start + 1],
                block[start + 2],
                block[start + 3],
            ]);
        }
        for index in 16..64 {
            let sigma0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let sigma1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(sigma0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(sigma1);
        }
        compress_block(&mut state, &schedule);
    }

    let mut out = [0_u8; 32];
    for (index, word) in state.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// `hex(sha256(content))`（无前缀；前缀由 [`digest_bytes`] 加）。
#[must_use]
fn sha256_hex(content: &[u8]) -> String {
    let mut out = String::with_capacity(64);
    for byte in sha256(content) {
        out.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// 一轮压缩：把 64 字的消息排程揉进 8 字状态。
#[allow(clippy::many_single_char_names)] // FIPS 180-4 的 a…h 命名，照抄比自造名好读
fn compress_block(state: &mut [u32; 8], schedule: &[u32; 64]) {
    let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h) = (
        state[0], state[1], state[2], state[3], state[4], state[5], state[6], state[7],
    );
    for index in 0..64 {
        let big_sigma1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choose = (e & f) ^ ((!e) & g);
        let temp1 = h
            .wrapping_add(big_sigma1)
            .wrapping_add(choose)
            .wrapping_add(SHA256_ROUND_CONSTANTS[index])
            .wrapping_add(schedule[index]);
        let big_sigma0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = big_sigma0.wrapping_add(majority);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }
    for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *slot = slot.wrapping_add(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_published_vectors() {
        // FIPS 180-4 / NIST 例：空串、`abc`、两块的 56 字节与 112 字节输入。
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            sha256_hex(
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmno\
                  ijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
                    .iter()
                    .copied()
                    .filter(|byte| *byte != b' ')
                    .collect::<Vec<u8>>()
                    .as_slice()
            ),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
        // 长输入（>1 块）与三种 padding 边界各自走不同分支。
        assert_eq!(
            sha256_hex(&vec![b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        assert_eq!(sha256_hex(&[b'a'; 55]).len(), 64);
        assert_eq!(sha256_hex(&[b'a'; 56]).len(), 64);
        assert_eq!(
            sha256_hex(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn sha256_agrees_with_the_repo_cross_implementation_vector() {
        // 同一向量在 `mc-core::hash` 与 `mc-plugin-host::token` 里也各有一份。
        assert_eq!(
            digest_bytes(b"hello"),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn digest_prefix_is_part_of_the_pinned_form() {
        // `mcp_approvals` 里存的就是带前缀的形态 ⇒ 前缀是契约的一部分。
        assert!(digest_bytes(b"{}").starts_with("sha256:"));
        assert_eq!(digest_bytes(b"{}").len(), "sha256:".len() + 64);
    }

    #[test]
    fn tool_json_round_trips_the_upstream_field_names() {
        let tool = Tool {
            name: "pull_issue".into(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
            schema_digest: digest_bytes(b"{}"),
            risk: String::new(),
        };
        let encoded = serde_json::to_value(&tool).unwrap();
        assert_eq!(encoded["name"], "pull_issue");
        assert_eq!(encoded["input_schema"]["type"], "object");
        // `omitempty` 的两个字段不出现（上游 wire 形状逐字）。
        assert!(encoded.get("description").is_none());
        assert!(encoded.get("risk").is_none());
    }

    #[test]
    fn connection_ignores_unknown_fields_and_plugin_prefix_is_explicit() {
        // 未知字段忽略（MCP/claim payload 都还在演进）。
        let raw = serde_json::json!({
            "installation_id": "11111111-2222-3333-4444-555555555555",
            "contribution_id": "plugin:11111111-2222-3333-4444-555555555555:sync",
            "contribution_key": "acme.sync",
            "config_id": "",
            "config_revision": 3,
            "endpoint": "https://mcp.example.com/rpc",
            "transport": "http",
            "protocol_versions": ["2025-03-26"],
            "approved_tools": [],
            "tool_schema_digest": "",
            "failure_policy": "optional",
            "something_new": {"nested": true}
        });
        let connection: Connection = serde_json::from_value(raw).unwrap();
        assert_eq!(connection.config_revision, 3);
        assert!(connection.is_plugin_contribution());
        assert_eq!(connection.endpoint_allowed_hosts, Vec::<String>::new());

        // 只认前缀：一个含冒号的**云签发** id 不能被当成插件贡献。
        let cloud = Connection {
            contribution_id: "cloud:workspace:42".into(),
            ..connection.clone()
        };
        assert!(!cloud.is_plugin_contribution());
    }

    #[test]
    fn plugin_contribution_id_shape_is_the_upstream_one() {
        assert_eq!(
            Connection::plugin_contribution_id("inst-1", "sync"),
            "plugin:inst-1:sync"
        );
    }
}
