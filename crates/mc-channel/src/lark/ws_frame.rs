//! lark 长连接**二进制帧信封**与其上的**分片重组**
//! （上游 `internal/integrations/lark/{ws_frame.go 387 行,ws_chunk_assembler.go 170 行}` = 557 行）。
//!
//! - **写者**：M7-11（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §28）。
//! - 本文件只管"字节 ↔ [`Frame`]"与"多个 Frame 拼回一个载荷"，**不含** JSON 事件解码
//!   （那是 [`super::ws_frame_decoder`]）、不含连接（那是 [`super::ws_connector`]）、
//!   不含引导（那是 [`super::ws_endpoint`]）。
//!
//! # 为什么手写 protobuf（不是 `prost` / 不是引官方 SDK）
//!
//! 上游逐字的理由：官方 Go SDK 的 `pbbp2` 包带整棵 open-platform 依赖树，而这里只需要**一个
//! 9 字段消息**。本仓多一层约束：**依赖面在 M7-0 anchor 一次定死**
//! （`crates/mc-channel/Cargo.toml` 的注释），本片**不得**新增三方包 ⇒ 手写
//! `protowire` 的等价物（varint / tag / length-delimited 三种原语，见本文件底部）。
//!
//! # 字节级兼容是**承重**的（不是洁癖）
//!
//! Lark 服务端把缺字段的帧当 `RequiredNotSetError` 丢掉；而 SDK 的 `MarshalToSizedBuffer`
//! 是 proto2 + gogo 生成的代码，它**无条件**写出 `SeqID` / `LogID` / `Service` / `Method`
//! 四个 `req` 字段（**即使值为 0**），三个 `opt` 字串（`PayloadEncoding` / `PayloadType` /
//! `LogIDNew`）也无条件写出（空值时是 tag + 长度 0），只有 `Payload` 受 `nil` 保护。
//! 所以：
//!
//! 1. [`Frame::payload`] 是 `Option<Vec<u8>>` —— **不是** `Vec<u8>`。上游 `Frame.Payload` 的
//!    `nil` 与空切片在 wire 上是两种字节序列（省略 vs `42 00`），用 `Vec<u8>` 表达会
//!    **静默**丢掉这个区别（心跳帧正是靠 `nil` 少一个字段）。
//! 2. 黄金字节用例（`ws_frame/tests.rs`）逐字节钉住上游 `ws_frame_test.go` 的五组向量。
//!    ⚠️ **不许为了迁就重构去改它们** —— 它们钉的是 SDK 行为；它们红了说明兼容性坏了。
//!
//! # 分片重组（上游 `ws_chunk_assembler.go`）
//!
//! Lark 把大载荷拆到多个 data 帧，靠三个 header 关联：`sum`（总片数）/ `seq`（本片下标，
//! 0 起）/ `message_id`（关**键**）。四条语义逐字复刻：
//!
//! - **TTL 滑动**：`sum > 1` 的部分状态活 [`DEFAULT_CHUNK_TTL`]（= SDK 的 5s），且**每来一片
//!   就续一次**（上游注释：Lark 可能把一次事件匀速铺开好几百毫秒，"收到第 0 片之后再也没动静"
//!   才是 TTL 要治的病）；
//! - **惰性 GC**：每次 [`ChunkAssembler::admit`] 顺手扫一遍过期项，不需要单独的清扫任务；
//! - **重复片幂等**：同一 `(message_id, seq)` 重复到达**静默覆盖**（Lark 保证同一键的字节稳定）。
//!   ⚠️ **重复片不产生第二次投递**：只有片数凑齐才有返回值，且凑齐后条目立即从表里删除；
//! - **畸形输入拒绝**：`message_id` 空 / `sum <= 0` / `seq < 0` / `seq >= sum` 一律**忽略**
//!   （不是错误）—— 一个坏 header 不能污染下一个事件的重组。
//!
//! # 凭据面（`docs/60` §2.3）
//!
//! 本文件**没有**凭据字段，也**没有**任何 `tracing::*`。但 [`Frame::payload`] 装的是**事件
//! JSON（可能含用户正文）** ⇒ 本文件的 `Debug` 派生**不**进日志：接线方只插值
//! `payload_len`（[`super::ws_connector`] 逐条遵守）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// =====================================================================
// 词表（上游 `ws_frame.go` 的常量）
// =====================================================================

/// 控制帧的 `Method` 取值（上游 `FrameMethodControl`）：心跳与"服务端下发 config"。
pub const FRAME_METHOD_CONTROL: i32 = 0;
/// 数据帧的 `Method` 取值（上游 `FrameMethodData`）：装事件载荷，**要求 ACK**。
pub const FRAME_METHOD_DATA: i32 = 1;

/// header 键 —— 帧类型判别式（上游 `FrameHeaderTypeKey`）。
pub const FRAME_HEADER_TYPE_KEY: &str = "type";
/// `type` 取值：事件帧。
pub const FRAME_HEADER_TYPE_EVENT: &str = "event";
/// `type` 取值：卡片交互帧。
pub const FRAME_HEADER_TYPE_CARD: &str = "card";
/// `type` 取值：服务端心跳。
pub const FRAME_HEADER_TYPE_PING: &str = "ping";
/// `type` 取值：心跳应答。
pub const FRAME_HEADER_TYPE_PONG: &str = "pong";

/// header 键 —— 去重 / 分片键（上游 `FrameHeaderMessageIDKey`）：**ACK 必须原样回声**。
pub const FRAME_HEADER_MESSAGE_ID_KEY: &str = "message_id";
/// header 键 —— 分片总数（上游 `FrameHeaderSumKey`）。
pub const FRAME_HEADER_SUM_KEY: &str = "sum";
/// header 键 —— 本片下标（上游 `FrameHeaderSeqKey`）。
pub const FRAME_HEADER_SEQ_KEY: &str = "seq";

/// 分片部分状态的默认寿命（上游 `newChunkAssembler` 的 SDK 默认值）。
pub const DEFAULT_CHUNK_TTL: Duration = Duration::from_secs(5);

// =====================================================================
// header 与帧（上游 `FrameHeader` / `Frame`）
// =====================================================================

/// `Frame.Headers` 里的一对 `(key, value)`（上游 `FrameHeader`，等价 SDK 的 `pbbp2.Header`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameHeader {
    /// header 名。
    pub key: String,
    /// header 值。
    pub value: String,
}

impl FrameHeader {
    /// 便捷构造。
    #[must_use]
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// 长连接的二进制帧信封（上游 `Frame`，等价 SDK 的 `pbbp2.Frame`）。
///
/// 字段编号与 SDK proto **逐个对齐** ⇒ [`Frame::marshal`] 的字节与官方 SDK 逐字节相同
/// （见模块文档）。`payload` 是 `Option`：`None` = 上游的 `nil` = **省略字段 8**。
///
/// ⚠️ 派生 `Debug` 只为用例与诊断；`payload` 里是事件 JSON ⇒ **不要把它插进日志**。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    /// proto 字段 1（`req`）：本连接内的序号。
    pub seq_id: u64,
    /// proto 字段 2（`req`）：日志 id。
    pub log_id: u64,
    /// proto 字段 3（`req`）：服务 id（出站帧必须等于引导响应里的 `service_id`）。
    pub service: i32,
    /// proto 字段 4（`req`）：[`FRAME_METHOD_CONTROL`] 或 [`FRAME_METHOD_DATA`]。
    pub method: i32,
    /// proto 字段 5（`rep`）。
    pub headers: Vec<FrameHeader>,
    /// proto 字段 6（`opt`，**无条件写出**，空值 = tag + 长度 0）。
    pub payload_encoding: String,
    /// proto 字段 7（`opt`，无条件写出）。
    pub payload_type: String,
    /// proto 字段 8（`opt`，**只在 `Some` 时写出**；`Some(vec![])` 写 `42 00`）。
    pub payload: Option<Vec<u8>>,
    /// proto 字段 9（`opt`，无条件写出）。
    pub log_id_new: String,
}

/// 帧解码失败的原因。
///
/// **不回显字节**：变体只带字段名 / wire 类型，错误文案里永远不会出现载荷内容。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// 空缓冲区（上游：`ws frame: empty buffer`）。
    #[error("lark ws frame: empty buffer")]
    Empty,
    /// varint / length 前缀被截断（上游逐字：`consume <field>`）。
    #[error("lark ws frame: truncated {field}")]
    Truncated {
        /// 出问题的字段名（不含取值）。
        field: &'static str,
    },
    /// 字段的 wire 类型与 proto 定义不符（上游逐字：`field N expects ...`）。
    #[error("lark ws frame: field {field} has an unexpected wire type {wire}")]
    WireType {
        /// proto 字段号。
        field: u32,
        /// 实际读到的 wire 类型（0..=7）。
        wire: u8,
    },
    /// 编号为 0 的字段（proto 里非法）。
    #[error("lark ws frame: field number 0 is invalid")]
    ZeroField,
}

impl Frame {
    /// 取第一个匹配 header 的值，缺席 ⇒ 空串（上游 `HeaderValue`：重复键"先到先赢"）。
    #[must_use]
    pub fn header_value(&self, key: &str) -> &str {
        self.headers
            .iter()
            .find(|header| header.key == key)
            .map_or("", |header| header.value.as_str())
    }

    /// 是否存在某个 header 键（诊断 / 路由用）。
    #[must_use]
    pub fn has_header(&self, key: &str) -> bool {
        self.headers.iter().any(|header| header.key == key)
    }

    /// `type` header 的值（`event` / `card` / `ping` / `pong` / 空）。
    #[must_use]
    pub fn frame_type(&self) -> &str {
        self.header_value(FRAME_HEADER_TYPE_KEY)
    }

    /// 是不是控制帧（`Method == 0`）。
    #[must_use]
    pub fn is_control(&self) -> bool {
        self.method == FRAME_METHOD_CONTROL
    }

    /// 载荷字节（缺席 ⇒ 空切片；只用于"喂给解码器"这一条路）。
    #[must_use]
    pub fn payload_bytes(&self) -> &[u8] {
        self.payload.as_deref().unwrap_or(&[])
    }

    /// 编码成 Lark 期望的 wire 字节（上游 `Frame.Marshal`）。
    ///
    /// 字段**写出顺序**与 SDK 的 `MarshalToSizedBuffer` 反向构建后的最终字节序一致
    /// （1→9）。⚠️ 别改"哪些字段无条件写出"——那是 SDK 生成的代码说了算，divergence
    /// 在 Lark 服务端丢帧之前是**看不见**的。
    #[must_use]
    pub fn marshal(&self) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::with_capacity(64 + self.payload_bytes().len());

        // —— proto2 `req`：即使为 0 也写 tag + varint（SDK 生成代码逐字如此）——
        append_varint_field(&mut buf, 1, self.seq_id);
        append_varint_field(&mut buf, 2, self.log_id);
        append_varint_field(&mut buf, 3, varint_from_i32(self.service));
        append_varint_field(&mut buf, 4, varint_from_i32(self.method));

        // —— 重复字段：每条 header 一个 length-delimited 项；空列表什么都不写 ——
        for header in &self.headers {
            let mut entry: Vec<u8> = Vec::with_capacity(8 + header.key.len() + header.value.len());
            append_bytes_field(&mut entry, 1, header.key.as_bytes());
            append_bytes_field(&mut entry, 2, header.value.as_bytes());
            append_bytes_field(&mut buf, 5, &entry);
        }

        // —— 三个 `opt` 字串无条件写出（空值 = tag + 长度 0），Payload 受 nil 保护 ——
        append_bytes_field(&mut buf, 6, self.payload_encoding.as_bytes());
        append_bytes_field(&mut buf, 7, self.payload_type.as_bytes());
        if let Some(payload) = &self.payload {
            append_bytes_field(&mut buf, 8, payload);
        }
        append_bytes_field(&mut buf, 9, self.log_id_new.as_bytes());
        buf
    }

    /// 解一帧（上游 `UnmarshalFrame`）。
    ///
    /// 未知字段按 proto3 语义**跳过**（服务端加字段不该把我们打挂）；截断 / 非法 wire 返回
    /// [`FrameError`]，调用方按"坏帧告警并继续"处置（[`super::ws_connector`] 逐条遵守）。
    ///
    /// # Errors
    ///
    /// 空缓冲 / varint 截断 / wire 类型不符 / 编号 0 ⇒ [`FrameError`]。
    pub fn unmarshal(bytes: &[u8]) -> Result<Self, FrameError> {
        if bytes.is_empty() {
            return Err(FrameError::Empty);
        }
        let mut frame = Self::default();
        let mut rest = bytes;
        while !rest.is_empty() {
            let (key, key_len) = consume_varint(rest, "tag")?;
            rest = &rest[key_len..];
            let number = u32::try_from(key >> 3).map_err(|_| FrameError::ZeroField)?;
            if number == 0 {
                return Err(FrameError::ZeroField);
            }
            let wire = (key & 0x07) as u8;
            let consumed = read_field(&mut frame, number, wire, rest)?;
            rest = &rest[consumed..];
        }
        Ok(frame)
    }
}

/// 读一个已知编号的字段，返回消费掉的字节数（未知编号走 `skip_field`）。
fn read_field(frame: &mut Frame, number: u32, wire: u8, rest: &[u8]) -> Result<usize, FrameError> {
    match number {
        1..=4 => {
            expect_wire(number, wire, WIRE_VARINT)?;
            let (value, len) = consume_varint(rest, field_name(number))?;
            match number {
                1 => frame.seq_id = value,
                2 => frame.log_id = value,
                3 => frame.service = i32_from_varint(value),
                _ => frame.method = i32_from_varint(value),
            }
            Ok(len)
        }
        5 => {
            expect_wire(number, wire, WIRE_BYTES)?;
            let (entry, len) = consume_bytes(rest, "header")?;
            frame.headers.push(unmarshal_header(entry)?);
            Ok(len)
        }
        6..=9 => {
            expect_wire(number, wire, WIRE_BYTES)?;
            let (raw, len) = consume_bytes(rest, field_name(number))?;
            match number {
                6 => frame.payload_encoding = decode_utf8(raw, 6)?,
                7 => frame.payload_type = decode_utf8(raw, 7)?,
                8 => frame.payload = Some(raw.to_vec()),
                _ => frame.log_id_new = decode_utf8(raw, 9)?,
            }
            Ok(len)
        }
        _ => skip_field(number, wire, rest),
    }
}

/// 解一条 header 子消息（上游 `unmarshalHeader`）。
fn unmarshal_header(bytes: &[u8]) -> Result<FrameHeader, FrameError> {
    let mut header = FrameHeader::default();
    let mut rest = bytes;
    while !rest.is_empty() {
        let (key, key_len) = consume_varint(rest, "header tag")?;
        rest = &rest[key_len..];
        let number = u32::try_from(key >> 3).map_err(|_| FrameError::ZeroField)?;
        if number == 0 {
            return Err(FrameError::ZeroField);
        }
        let wire = (key & 0x07) as u8;
        match number {
            1 | 2 => {
                expect_wire(number, wire, WIRE_BYTES)?;
                let (raw, len) = consume_bytes(rest, "header field")?;
                let text = decode_utf8(raw, number)?;
                if number == 1 {
                    header.key = text;
                } else {
                    header.value = text;
                }
                rest = &rest[len..];
            }
            _ => {
                let skipped = skip_field(number, wire, rest)?;
                rest = &rest[skipped..];
            }
        }
    }
    Ok(header)
}

/// 跳过一个不认识的字段（proto3 语义），返回消费掉的字节数。
fn skip_field(number: u32, wire: u8, rest: &[u8]) -> Result<usize, FrameError> {
    match wire {
        WIRE_VARINT => consume_varint(rest, "unknown varint").map(|(_, len)| len),
        WIRE_FIXED64 => rest
            .get(..8)
            .map(|_| 8)
            .ok_or(FrameError::Truncated { field: "unknown" }),
        WIRE_BYTES => consume_bytes(rest, "unknown bytes").map(|(_, len)| len),
        WIRE_FIXED32 => rest
            .get(..4)
            .map(|_| 4)
            .ok_or(FrameError::Truncated { field: "unknown" }),
        _ => Err(FrameError::WireType {
            field: number,
            wire,
        }),
    }
}

// =====================================================================
// protowire 原语（`google.golang.org/protobuf/encoding/protowire` 的三条）
// =====================================================================

const WIRE_VARINT: u8 = 0;
const WIRE_FIXED64: u8 = 1;
const WIRE_BYTES: u8 = 2;
const WIRE_FIXED32: u8 = 5;

fn append_tag(buf: &mut Vec<u8>, number: u32, wire: u8) {
    append_raw_varint(buf, (u64::from(number) << 3) | u64::from(wire));
}

#[allow(clippy::cast_possible_truncation)] // 低 7 位恒 ≤ 0x7F：截断在这里是**定义**而不是意外
fn append_raw_varint(buf: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        buf.push((value as u8) | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

fn append_varint_field(buf: &mut Vec<u8>, number: u32, value: u64) {
    append_tag(buf, number, WIRE_VARINT);
    append_raw_varint(buf, value);
}

fn append_bytes_field(buf: &mut Vec<u8>, number: u32, bytes: &[u8]) {
    append_tag(buf, number, WIRE_BYTES);
    append_raw_varint(buf, bytes.len() as u64);
    buf.extend_from_slice(bytes);
}

fn consume_varint(bytes: &[u8], field: &'static str) -> Result<(u64, usize), FrameError> {
    let mut value: u64 = 0;
    let mut shift: u32 = 0;
    for (index, byte) in bytes.iter().enumerate() {
        value |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
        shift += 7;
        if shift >= 64 {
            return Err(FrameError::Truncated { field });
        }
    }
    Err(FrameError::Truncated { field })
}

fn consume_bytes<'a>(
    bytes: &'a [u8],
    field: &'static str,
) -> Result<(&'a [u8], usize), FrameError> {
    let (len, prefix) = consume_varint(bytes, field)?;
    let len = usize::try_from(len).map_err(|_| FrameError::Truncated { field })?;
    let end = prefix
        .checked_add(len)
        .ok_or(FrameError::Truncated { field })?;
    let payload = bytes
        .get(prefix..end)
        .ok_or(FrameError::Truncated { field })?;
    Ok((payload, end))
}

fn expect_wire(number: u32, wire: u8, expected: u8) -> Result<(), FrameError> {
    if wire == expected {
        Ok(())
    } else {
        Err(FrameError::WireType {
            field: number,
            wire,
        })
    }
}

/// 字段号 → 名字（只给错误文案用）。
fn field_name(number: u32) -> &'static str {
    match number {
        1 => "seq_id",
        2 => "log_id",
        3 => "service",
        4 => "method",
        6 => "payload_encoding",
        7 => "payload_type",
        8 => "payload",
        9 => "log_id_new",
        _ => "field",
    }
}

/// `&[u8]` → `String`（非 UTF-8 ⇒ 截断错误；错误文案不带内容）。
fn decode_utf8(raw: &[u8], number: u32) -> Result<String, FrameError> {
    std::str::from_utf8(raw)
        .map(ToString::to_string)
        .map_err(|_| FrameError::WireType {
            field: number,
            wire: WIRE_BYTES,
        })
}

/// `i32` → varint 的 u64 位形态（上游 `uint64(uint32(v))`：负数编成 10 字节）。
///
/// 用 `to_ne_bytes` 做**位重解释**而不是 `as u32`：这是有意的（clippy 的
/// `cast_sign_loss` 想拦的就是"看起来像数值转换"的写法，而这里要的正是位形态）。
fn varint_from_i32(value: i32) -> u64 {
    u64::from(u32::from_ne_bytes(value.to_ne_bytes()))
}

/// varint 的 u64 → `i32`（上游 `int32(v)`：截低 32 位后按位重解释）。
fn i32_from_varint(value: u64) -> i32 {
    let bytes = value.to_le_bytes();
    i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

// =====================================================================
// 出站帧的三个构造（上游 `NewPingFrame` / `NewPongFrame` / `NewAckFrame`）
// =====================================================================

/// 客户端保活帧。
///
/// ⚠️ Lark 的长连接用的是**应用层** ping（`Method=Control` + `type=ping` 的二进制帧），
/// **不是** WebSocket 协议层的 PING —— gorilla 的 `WriteControl` / `tungstenite` 的
/// `Message::Ping` 都会被 Lark 服务端无视（上游逐字）。
#[must_use]
pub fn new_ping_frame(service_id: i32) -> Frame {
    Frame {
        service: service_id,
        headers: vec![FrameHeader::new(
            FRAME_HEADER_TYPE_KEY,
            FRAME_HEADER_TYPE_PING,
        )],
        ..Frame::default()
    }
}

/// 对服务端 ping 的应答帧（Lark 可能在任何时刻推 ping，我们照原样回）。
#[must_use]
pub fn new_pong_frame(service_id: i32) -> Frame {
    Frame {
        service: service_id,
        headers: vec![FrameHeader::new(
            FRAME_HEADER_TYPE_KEY,
            FRAME_HEADER_TYPE_PONG,
        )],
        ..Frame::default()
    }
}

/// 入站 data 帧的 ACK（上游 `NewAckFrame`）。
///
/// 上游逐字：ACK **原样复用入站帧的 headers**（服务端靠 `message_id` 配对）与
/// `method` / `service`，载荷是 SDK `Response` 的 JSON 形态。`code_ok = false` 时是 500
/// （服务端会重投这条事件）。载荷里 `headers` / `data` 都是 **JSON `null`** —— 这是 SDK
/// stdlib `encoding/json` 对 `nil` map / `nil` slice 的输出，服务端就期待这个形状。
#[must_use]
pub fn new_ack_frame(inbound: &Frame, code_ok: bool) -> Frame {
    let code = if code_ok { 200 } else { 500 };
    Frame {
        seq_id: 0,
        log_id: 0,
        service: inbound.service,
        method: inbound.method,
        headers: inbound.headers.clone(),
        payload_encoding: String::new(),
        payload_type: String::new(),
        payload: Some(format!(r#"{{"code":{code},"headers":null,"data":null}}"#).into_bytes()),
        log_id_new: String::new(),
    }
}

// =====================================================================
// 分片重组（上游 `ws_chunk_assembler.go`）
// =====================================================================

/// 可注入的时钟（用例用它做**确定性**的 TTL 测试；生产就是 [`Instant::now`]）。
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// 生产时钟。
#[must_use]
pub fn system_clock() -> Clock {
    Arc::new(Instant::now)
}

/// 一个 `message_id` 的部分状态。
#[derive(Debug)]
struct ChunkEntry {
    /// 按下标排的片；`None` = 还没到。
    chunks: Vec<Option<Vec<u8>>>,
    /// 已到的片数（`chunks` 里 `Some` 的个数）。
    received: usize,
    /// 本条消息的截止时刻（**每来一片就续**，见模块文档）。
    deadline: Instant,
}

/// 多帧事件的**分片重组器**（上游 `chunkAssembler`）。
///
/// 线程安全（内部 `Mutex`）：一个实例服务所有监管任务；状态**只在进程内**存活 —— 分片不会
/// 跨进程重启到达（重连后 Lark 从第 0 片重发整条事件），所以不需要持久化。
pub struct ChunkAssembler {
    ttl: Duration,
    now: Clock,
    inner: Mutex<HashMap<String, ChunkEntry>>,
}

impl std::fmt::Debug for ChunkAssembler {
    /// 手写：注入的时钟是 `dyn Fn`（不可打印）⇒ 只报寿命与当前缓冲条数。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChunkAssembler")
            .field("ttl", &self.ttl)
            .field("pending", &self.pending_count())
            .finish_non_exhaustive()
    }
}

impl ChunkAssembler {
    /// 造一个重组器；非正 `ttl` 回落 [`DEFAULT_CHUNK_TTL`]（上游 `newChunkAssembler` 逐字）。
    #[must_use]
    pub fn new(ttl: Duration, now: Clock) -> Self {
        let ttl = if ttl.is_zero() {
            DEFAULT_CHUNK_TTL
        } else {
            ttl
        };
        Self {
            ttl,
            now,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 默认形态（[`DEFAULT_CHUNK_TTL`] + 系统时钟）。
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_CHUNK_TTL, system_clock())
    }

    /// 本实例的部分状态寿命。
    #[must_use]
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// 收一片，返回：
    ///
    /// - `Some(payload)` —— 片齐了，载荷按 `seq` 升序拼接，条目**已删除**（⇒ 同一条消息
    ///   不会因为重复片而被投递两次）；
    /// - `None` —— 还缺片（调用方**不得** emit，也**不得** ACK 本帧；上游逐字：ACK 只在
    ///   整条装配完之后发，服务端才好重投整条事件）。
    ///
    /// 畸形输入（空 `message_id` / `sum <= 0` / `seq < 0` / `seq >= sum`）一律忽略并返回
    /// `None`。
    pub fn admit(&self, message_id: &str, sum: i32, seq: i32, payload: &[u8]) -> Option<Vec<u8>> {
        if message_id.is_empty() || sum <= 0 || seq < 0 || seq >= sum {
            return None;
        }
        let sum = usize::try_from(sum).ok()?;
        let seq = usize::try_from(seq).ok()?;

        let mut entries = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.gc_expired_locked(&mut entries);

        let now = (self.now)();
        let entry = entries
            .entry(message_id.to_string())
            .or_insert_with(|| ChunkEntry {
                chunks: vec![None; sum],
                received: 0,
                deadline: now + self.ttl,
            });
        // 越界 = 同一个 `message_id` 被两条 `sum` 不同的事件复用。上游在这里会 panic
        // （`entry.chunks[seq]` 越界索引），本仓**失败关闭**：忽略这一片，不污染既有状态。
        if seq >= entry.chunks.len() {
            return None;
        }
        // 同一个 (message_id, seq) 重复到达（网络重投 / Lark 乱序重发）：静默覆盖。
        // Lark 保证同一键的字节稳定 ⇒ 覆盖不会改变最终拼接结果，也**不会**多算一片。
        if entry.chunks[seq].is_none() {
            entry.received += 1;
        }
        entry.chunks[seq] = Some(payload.to_vec());
        // 滑动截止：每来一片都续一次（见模块文档）。
        entry.deadline = now + self.ttl;

        if entry.received < entry.chunks.len() {
            return None;
        }
        let mut out: Vec<u8> = Vec::new();
        for chunk in entry.chunks.iter().flatten() {
            out.extend_from_slice(chunk);
        }
        entries.remove(message_id);
        Some(out)
    }

    /// 部分状态里**已到**的片数（诊断；`None` 槽位不算）。
    #[must_use]
    pub fn received_count(&self, message_id: &str) -> usize {
        let entries = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.get(message_id).map_or(0, |entry| entry.received)
    }

    /// 清掉过期条目，返回清掉的条数（上游 `gcExpired`）。
    pub fn gc_expired(&self) -> usize {
        let mut entries = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.gc_expired_locked(&mut entries)
    }

    /// 当前缓冲着的"半成品"条数（上游 `pendingCount`；用例与运维面板都用它）。
    #[must_use]
    pub fn pending_count(&self) -> usize {
        let entries = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.len()
    }

    fn gc_expired_locked(&self, entries: &mut HashMap<String, ChunkEntry>) -> usize {
        let now = (self.now)();
        let before = entries.len();
        entries.retain(|_, entry| entry.deadline >= now);
        before - entries.len()
    }
}

/// 从一帧的 headers 里取出分片元数据（上游 `parseChunkHeaders`）。
///
/// 缺席 / 解不出整数 ⇒ `sum = 0`（调用方读作"单帧事件"并**绕过**重组器）。返回
/// `(sum, seq, message_id)`。
#[must_use]
pub fn parse_chunk_headers(frame: &Frame) -> (i32, i32, String) {
    let sum = frame
        .header_value(FRAME_HEADER_SUM_KEY)
        .parse::<i32>()
        .unwrap_or(0);
    let seq = frame
        .header_value(FRAME_HEADER_SEQ_KEY)
        .parse::<i32>()
        .unwrap_or(0);
    (
        sum,
        seq,
        frame.header_value(FRAME_HEADER_MESSAGE_ID_KEY).to_string(),
    )
}

#[cfg(test)]
mod tests;
