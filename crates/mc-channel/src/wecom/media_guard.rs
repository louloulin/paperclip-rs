//! `media_guard.go`（306 行）的本地落点：**媒体取回器的目标地址闸**（R-M7-9 的 SSRF 面）。
//!
//! - **写者**：M7-18（`LUM-1783` / `docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §35 的 D1）。
//! - **上游定位**（文件头逐字）：媒体取回器是被**别人**指着一个地址的。回调驮着一个预签名的
//!   COS URL，而我们要去 GET 它。这个 URL 是**从 socket 上过来的一根字符串**：是 `WeCom` 把
//!   它放在那里的，但 adapter **无法证明**这件事，而这次取回是从**部署的网络内部**发起的 ——
//!   那个网络能碰到什么，它就能碰到什么。仅在这台机器上，那个可达集合就包含一条 `Tailscale`
//!   tailnet（`100.64.0.0/10`）、一个代理的 fake-IP 段（`198.18.0.0/15`）、`Docker` 的网桥，
//!   以及后端自己的管理端点监听着的回环。
//!
//! # 闸在**连接**上，不在 URL 上（本文件的中心判断）
//!
//! 只查主机名本身**一文不值**：URL 可以重定向（一个 `302` 到 `http://169.254.169.254/`
//! 就是一行攻击者可控的响应），而一个通过了检查的主机名**下一刻**可以解析到别的东西。
//! 一个"解析目标地址并拒掉非公网地址"的拨号器跑在**每一跳重定向**上、针对**真正被连接的
//! 那个答案**，这是同时覆盖这两件事的**唯一**位置。
//!
//! 本仓没有 `DialContext`（`reqwest` 的接缝是 [`reqwest::dns::Resolve`]）⇒ 形态差异逐条
//! 登记在 `docs/32` §35 的 D3：**解析阶段就过滤**（上游是"全部地址先过滤、只拨通过的"，
//! 语义相同：混着公私两种答案的主机，公的那些会被试、内网那些会被拒），外加
//! `no_proxy()`（代理会重新解析目标 ⇒ 绕过这条保证）与重定向回调里的 scheme 闸。
//!
//! # 两组地址段：差别不是分类学，而是"运维能不能把它打开"
//!
//! | 组 | 内容 | 谁能打开 |
//! | --- | --- | --- |
//! | [`RESERVED_MEDIA_PREFIXES`] | 不是公网的空间（IANA 特别用途登记表**减去** `netip` 自己的判据已经抓住的、再减去那些**普通全球可路由单播**的块） | **运维可以**：假 IP 代理的池子就住在这里 |
//! | [`TRANSLATION_MEDIA_PREFIXES`] | **翻译空间**：这些地址不是目的地，而是**穿着 IPv6 外衣的 IPv4 目的地** | **没人能**：`::/0` 或 `2002::/16` 都不行 |
//!
//! `100.64.0.0/10` 是[`RESERVED_MEDIA_PREFIXES`] 里最要紧的那一条 —— RFC 6598 的共享地址空间，
//! **没有任何标准库判据把它报成私网**，而它正是 `Tailscale` 发出去的地址。一个 tailnet 对端
//! 就是**信任边界之内的一台机器**，凭 IP 可达、不需要任何凭据。
//!
//! 翻译空间那一组为什么是硬闸：`64:ff9b:1::a9fe:a9fe` 在 `NAT64` 翻译器看到它的那一刻**就是**
//! `169.254.169.254`，`2002:7f00:1::1` 经 `6to4` 中继**就是** `127.0.0.1`。`IsLoopback` /
//! `IsPrivate` / `IsLinkLocalUnicast` 这几条判据在这些地址上**永不触发**（在那个时刻它们就是
//! IPv6 地址），所以这一组列表是闸与**里面那个地址**之间的**唯一**东西。让它被
//! `MULTICA_WECOM_MEDIA_ALLOW_CIDRS` 覆盖，就等于 `::/0`（或者同样好用的 `2002::/16`）
//! 够到了闸存在的理由所要拒的回环与 metadata 端点 —— 而且是用一种**运维从没想过自己打开过**
//! 的拼法写出来的。代价是零：没有 COS 对象、没有代理池、也没有哪个部署住在翻译空间里。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Duration;

use async_trait::async_trait;

/// 一次 TCP 连接的上限（上游 `mediaDialTimeout` 10s）。它坐在
/// [`super::media_download::MEDIA_DOWNLOAD_TIMEOUT`] 里面 —— 那个限制的是整次取回。
pub const MEDIA_DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// 最多跟随几跳重定向（上游 `newMediaHTTPClient` 里那条 `len(via) >= 5`）。
pub const MAX_MEDIA_REDIRECTS: usize = 5;

// =====================================================================
// 前缀
// =====================================================================

/// 一个网络前缀（`addr` 的高 `bits` 位）。
///
/// 与 `dingtalk::media::guard::Prefix` **同形但各写一份**：`docs/60` §2.2 明确要
/// "adapter 之间零互相依赖"，跨 adapter 复用一个端口/类型会把这个方向变成依赖边
/// （与 `dingtalk` 模块文档的差异 4 同一条判例）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefix {
    addr: IpAddr,
    bits: u8,
}

const fn prefix(address: IpAddr, bits: u8) -> Prefix {
    Prefix {
        addr: address,
        bits,
    }
}

const fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

const fn v6(segments: [u16; 8]) -> IpAddr {
    IpAddr::V6(Ipv6Addr::new(
        segments[0],
        segments[1],
        segments[2],
        segments[3],
        segments[4],
        segments[5],
        segments[6],
        segments[7],
    ))
}

/// 前缀解析失败（上游 `netip.ParsePrefix` 的错误，逐条对应）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrefixError {
    /// 没有 `/` 或位宽不是数字。
    #[error("wecom: media cidr must be written as <address>/<bits>")]
    Malformed,
    /// 地址不是 IP 字面量。
    #[error("wecom: media cidr address is not an IP literal")]
    BadAddress,
    /// 位宽超出这一族的宽度（v4 ≤ 32、v6 ≤ 128）。
    #[error("wecom: media cidr bit length is out of range for its family")]
    BadBits,
    /// 前缀位之外的位不为零（上游 `netip.ParsePrefix` 拒绝这种写法）。
    #[error("wecom: media cidr has host bits set beyond its prefix length")]
    HostBitsSet,
}

impl Prefix {
    /// 一个已知前缀（常量表用）。
    #[must_use]
    pub const fn new(address: IpAddr, bits: u8) -> Self {
        prefix(address, bits)
    }

    /// 解析 `100.64.0.0/10` 形态的字符串（上游 `netip.ParsePrefix`）。
    ///
    /// 地址得是**规范形态**：前缀位之外有 1 的写法（`10.0.0.1/8`）一律拒，与
    /// `netip.ParsePrefix` 一致 —— 运维把 `10.1.2.3/8` 写进白名单时应当看到一条错误，
    /// 而不是一个悄悄变窄/变宽的范围。
    ///
    /// # Errors
    ///
    /// 见 [`PrefixError`]。
    pub fn parse(raw: &str) -> Result<Self, PrefixError> {
        let (address, bits) = raw.split_once('/').ok_or(PrefixError::Malformed)?;
        let bits: u8 = bits.trim().parse().map_err(|_| PrefixError::Malformed)?;
        let address: IpAddr = address
            .trim()
            .parse()
            .map_err(|_| PrefixError::BadAddress)?;
        match address {
            IpAddr::V4(_) if bits > 32 => return Err(PrefixError::BadBits),
            IpAddr::V6(_) if bits > 128 => return Err(PrefixError::BadBits),
            _ => {}
        }
        if mask(address, bits) != address {
            return Err(PrefixError::HostBitsSet);
        }
        Ok(prefix(address, bits))
    }

    /// 这个前缀包含这个地址吗（不同族一律不包含）。
    #[must_use]
    pub fn contains(&self, address: IpAddr) -> bool {
        match (self.addr, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                let bits = u32::from(network) ^ u32::from(address);
                self.bits == 0 || bits.leading_zeros() >= u32::from(self.bits)
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                let bits = u128::from(network) ^ u128::from(address);
                self.bits == 0 || bits.leading_zeros() >= u32::from(self.bits)
            }
            _ => false,
        }
    }

    /// 位宽（诊断用）。
    #[must_use]
    pub const fn bits(&self) -> u8 {
        self.bits
    }

    /// 网络地址（诊断用）。
    #[must_use]
    pub const fn address(&self) -> IpAddr {
        self.addr
    }
}

impl std::fmt::Display for Prefix {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.addr, self.bits)
    }
}

/// 把地址前缀位之外的位清零（`HostBitsSet` 的判据）。
fn mask(address: IpAddr, bits: u8) -> IpAddr {
    match address {
        IpAddr::V4(address) => {
            let shift = 32u32.saturating_sub(u32::from(bits));
            let raw = if bits == 0 {
                0
            } else {
                u32::from(address) & (u32::MAX << shift)
            };
            IpAddr::V4(Ipv4Addr::from(raw))
        }
        IpAddr::V6(address) => {
            let shift = 128u32.saturating_sub(u32::from(bits));
            let raw = if bits == 0 {
                0
            } else {
                u128::from(address) & (u128::MAX << shift)
            };
            IpAddr::V6(Ipv6Addr::from(raw))
        }
    }
}

// =====================================================================
// 两张常量表
// =====================================================================

/// 不是公网的空间（上游 `reservedMediaPrefixes`，**逐条**）。
///
/// 这一组是 [`media_allowed_prefixes`] **可以**重开的，因为里面住着一个真实的部署形态：
/// 一个 fake-IP 代理给每个公网主机名发出 `198.18.0.0/15` 里的地址，`WeCom` 自己的 COS 主机
/// 也不例外。知道自家代理池的运维可以把它声明出来。
pub const RESERVED_MEDIA_PREFIXES: &[Prefix] = &[
    // ---- IPv4 ----
    prefix(v4(0, 0, 0, 0), 8),       // "this network"
    prefix(v4(100, 64, 0, 0), 10),   // RFC 6598 CGNAT —— Tailscale 住在这里
    prefix(v4(192, 0, 0, 0), 24),    // IETF protocol assignments
    prefix(v4(192, 0, 2, 0), 24),    // TEST-NET-1
    prefix(v4(198, 18, 0, 0), 15),   // benchmarking —— 也是代理的 fake-IP 段
    prefix(v4(198, 51, 100, 0), 24), // TEST-NET-2
    prefix(v4(203, 0, 113, 0), 24),  // TEST-NET-3
    prefix(v4(240, 0, 0, 0), 4),     // reserved，含 255.255.255.255
    prefix(v4(192, 88, 99, 0), 24),  // 已废弃的 6to4 中继 anycast —— 2002::/16 的 v4 端
    // ---- IPv6 ----
    prefix(v6([0x100, 0, 0, 0, 0, 0, 0, 0]), 64), // discard-only
    prefix(v6([0x100, 0, 0, 1, 0, 0, 0, 0]), 64), // dummy prefix，也是 discard-only
    // 整个 IETF protocol-assignments 块，而不是它的十几个子条目：Teredo（2001::/32）、
    // benchmarking（2001:2::/48 —— 上面 198.18.0.0/15 的孪生）、AMT、AS112-v6、
    // ORCHID/ORCHIDv2、DET，以及 PCP / TURN / DNS-SD 的 anycast 地址全在里面。它们**没有
    // 一个**是 COS 对象住的地方，而一条前缀比八条更容易保持为真（v4 那边用
    // 192.0.0.0/24 做的是同一个取舍）。文档空间是 2001:db8::/32，在这个 /23 之外，单列。
    prefix(v6([0x2001, 0, 0, 0, 0, 0, 0, 0]), 23),
    prefix(v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0]), 32), // documentation
    prefix(v6([0x3fff, 0, 0, 0, 0, 0, 0, 0]), 20),     // documentation，RFC 9637
    prefix(v6([0x5f00, 0, 0, 0, 0, 0, 0, 0]), 16),     // SRv6 SID，RFC 9602 —— 路由标签，不是主机
    // Site-local。RFC 3879 废弃了它、IANA 也把它下架了，所以没有任何判据、也没有登记表的
    // 哪一行覆盖它 —— 但 2004 年之前从它里面编过号的网络**仍然**在内网路由它，而它只有一行。
    prefix(v6([0xfec0, 0, 0, 0, 0, 0, 0, 0]), 10),
];

/// **翻译空间**（上游 `translationMediaPrefixes`）：没有任何配置能重开它（模块文档有完整理由）。
pub const TRANSLATION_MEDIA_PREFIXES: &[Prefix] = &[
    prefix(v6([0x64, 0xff9b, 0, 0, 0, 0, 0, 0]), 96), // NAT64，well-known prefix
    prefix(v6([0x64, 0xff9b, 1, 0, 0, 0, 0, 0]), 48), // NAT64，local-use prefix（RFC 8215）
    prefix(v6([0x2002, 0, 0, 0, 0, 0, 0, 0]), 16),    // 6to4 —— 最后 112 位以一个 IPv4 地址开头
];

// =====================================================================
// 白名单（运维声明）
// =====================================================================

/// 运维声明为"媒体取回可以拨"的范围（上游 `mediaAllowedPrefixes`）。
///
/// 默认**空**。把它加宽是一个**有代价**的决定：写在这里的段可以被**别人控制的一个 URL**
/// 够到，而那正是这道闸存在的理由。它是按部署 opt-in 的，只在运维知道那个段属于自家代理时
/// 才值得设。
///
/// 不管多宽，它**打不开**：回环、私网与链路本地（在查这张表之前就被拒了），以及
/// [`TRANSLATION_MEDIA_PREFIXES`]（晚一步、同样理由被拒）。
static MEDIA_ALLOWED_PREFIXES: LazyLock<RwLock<Vec<Prefix>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// 声明媒体闸可以拨的范围（上游 `SetMediaAllowedPrefixes`）：由宿主在启动时从
/// `MULTICA_WECOM_MEDIA_ALLOW_CIDRS` 读出来之后调。
///
/// **一条解析不了的条目被报告并跳过**，而不是悄悄加宽或悄悄收窄这道闸 —— 所以返回的错误
/// 非空时，那张表里只有**解析成功**的那几条。
///
/// 🔴 环境变量本身**不在本文件读**：本仓的唯一读取口是 `mc_http::state::ChannelKeys`
/// （`docs/60` §2.3 的判据 4；`mc-channel` 与 route 层不得各自 `std::env::var`）。
/// [`parse_media_allow_cidrs`] 是那条读法**唯一**要用到的纯函数。
#[must_use]
pub fn set_media_allowed_prefixes(cidrs: &[String]) -> Vec<PrefixError> {
    let mut parsed = Vec::with_capacity(cidrs.len());
    let mut errors = Vec::new();
    for raw in cidrs {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        match Prefix::parse(trimmed) {
            Ok(prefix) => parsed.push(prefix),
            Err(error) => errors.push(error),
        }
    }
    *MEDIA_ALLOWED_PREFIXES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = parsed;
    errors
}

/// 当前生效的白名单（诊断与用例用）。
#[must_use]
pub fn media_allowed_prefixes() -> Vec<Prefix> {
    MEDIA_ALLOWED_PREFIXES
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// 把 `MULTICA_WECOM_MEDIA_ALLOW_CIDRS` 的值切成一串 CIDR（逗号分隔，两头空白吃掉）。
///
/// 上游那侧的 env 形态**在代码里观察不到**（`SetMediaAllowedPrefixes` 唯一的调用点在仓库外），
/// 所以切片规则由本仓定：空串 ⇒ 空表，逗号分隔，空白去掉但**不**在文件里读 env
/// （登记 `docs/32` §35 的 D4）。
#[must_use]
pub fn parse_media_allow_cidrs(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

// =====================================================================
// 判据
// =====================================================================

/// IPv4-mapped 的 IPv6 解包成 IPv4（上游 `Addr.Unmap()`）。
///
/// `::ffff:127.0.0.1` 在解包之前**一条** IPv4 判据都不报 —— 这正是那个把戏。
#[must_use]
pub fn unmap(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6_address) => match v6_address.to_ipv4_mapped() {
            Some(v4_address) => IpAddr::V4(v4_address),
            None => IpAddr::V6(v6_address),
        },
        other @ IpAddr::V4(_) => other,
    }
}

/// 标准库那几条判据（`IsLoopback` / `IsPrivate` / `IsLinkLocal*` / `IsMulticast` /
/// `IsUnspecified` / `IsInterfaceLocalMulticast`）。
///
/// **手写而不是调 `std`**：`IpAddr` 的 `is_private` / `is_global` 等一批判据在本仓的
/// `rust-version` 下仍是 unstable，而按族写全这几条本来就是几行常量比较
/// （`dingtalk::media::guard` 用的是同一手法）。
fn standard_predicate_refuses(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let raw = u32::from(address);
            address.is_loopback()                       // 127.0.0.0/8
                || address.is_private()                 // 10/8、172.16/12、192.168/16
                || address.is_link_local()              // 169.254/16
                || address.is_unspecified()             // 0.0.0.0
                || (raw & 0xf000_0000) == 0xe000_0000   // 组播 224/4
                || (raw & 0xffff_ff00) == 0xe000_0000 // 链路本地组播 224.0.0/24
        }
        // ⚠️ `is_documentation()` / `is_broadcast()` **刻意不在这里**：上游的 `publicAddrOnly`
        // 只走 netip 那几条判据，而 TEST-NET / 文档段 / 广播地址都只出现在
        // [`RESERVED_MEDIA_PREFIXES`] 里 ⇒ 它们对运维是**可以打开的**。把它们提到这一层会让
        // 白名单白写（第一个版本就是这么错的，[`tests`] 的反例用例当场抓住）。
        IpAddr::V6(address) => {
            address.is_loopback()                       // ::1
                || address.is_unspecified()             // ::
                || address.is_unique_local()            // fc00::/7
                || address.is_unicast_link_local()      // fe80::/10
                || address.is_multicast()               // ff00::/8，含接口本地组播 ff01::/16
                // 🔴 比上游**更严**的一格（登记 `docs/32` §35 的 D5）：IPv4-compatible 的
                // `::/96`（已废弃）上游那两张表都没覆盖、`netip` 的判据也不报它，而它正是
                // 那道"穿 IPv6 外衣的 IPv4"闸要防的拼法之一。没有任何 COS 对象住在那里。
                || address.segments()[..6].iter().all(|segment| *segment == 0)
        }
    }
}

/// 生产策略（上游 `publicAddrOnly`）：**不是全球可路由公网的一切都拒**。
#[must_use]
pub fn public_addr_only(address: IpAddr) -> bool {
    let address = unmap(address);
    if standard_predicate_refuses(address) {
        return false;
    }
    // 翻译空间在最前面，而且**在查白名单之前**：这些地址里包着的那个 IPv4 地址是上面那些
    // 判据一眼就该拒的，它们之所以没触发，唯一的原因是拼法。没有任何运维配置能重开它。
    for candidate in TRANSLATION_MEDIA_PREFIXES {
        if candidate.contains(address) {
            return false;
        }
    }
    for candidate in RESERVED_MEDIA_PREFIXES {
        if candidate.contains(address) {
            // 运维可能已经声明这个段是自家的 —— fake-IP 代理的池子就是它存在的理由。
            // 只对**闸本来就会拒**的地址查这张表，所以一份空白名单让闸与以前一样严。
            return media_allowed_prefixes()
                .iter()
                .any(|allowed| allowed.contains(address));
        }
    }
    true
}

// =====================================================================
// 拨号器
// =====================================================================

/// 一个地址通不通的策略（上游 `addrPolicy`）。
///
/// 生产用 [`public_addr_only`]；**用例**换成一个允许它们自己那台服务器所在回环的策略，
/// 这样被测试的是闸的判决、而不是测试脚手架的地址。
pub type AddrPolicy = Arc<dyn Fn(IpAddr) -> bool + Send + Sync>;

/// 把普通函数包成策略（`#[must_use]` 的构造口）。
#[must_use]
pub fn addr_policy(policy: fn(IpAddr) -> bool) -> AddrPolicy {
    Arc::new(policy)
}

/// 地址闸的失败（上游 `ErrMediaAddrBlocked` 与解析失败）。
///
/// **刻意与拨号失败区分开**：调用方给两者的日志完全不同，而只有这一个意味着"有人发了一个
/// 他不该发的 URL"。变体**不带地址**（上游的哨兵也不带）⇒ 这条错误整体可以进日志。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaGuardError {
    /// 上游 `ErrMediaAddrBlocked`：一个媒体主机解析出来的**每一个**地址都是部署不该被指向的
    /// 那种。
    #[error("wecom: media host resolves to a non-public address")]
    BlockedAddress,
    /// 主机名解析不出来（上游 `fmt.Errorf("…: resolve %s: %w", host, err)` 的那一半）。
    #[error("wecom: media dial: resolve failed")]
    Resolve,
    /// 主机名是空的。
    #[error("wecom: media dial: empty host")]
    EmptyHost,
    /// `reqwest` 的客户端构造失败（本仓新增的一层；上游 `newMediaHTTPClient` 没有失败面）。
    #[error("wecom: media http client could not be built")]
    Client,
}

/// 主机名 → 地址（上游 `hostResolver`，`net.DefaultResolver` 的那个方法）。
///
/// 做成 trait 的理由与上游逐字相同：**用例要能对某个名字交出一个自己挑的地址**，
/// 这样"重绑定"这件事才是被测试的、而不是被争论的。
#[async_trait]
pub trait MediaLookup: Send + Sync {
    /// 解析一个主机名。
    ///
    /// # Errors
    ///
    /// [`MediaGuardError::Resolve`]（**不带主机名**：它来自 wire）。
    async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, MediaGuardError>;
}

/// 生产解析器（上游 `net.DefaultResolver`）。
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemLookup;

#[async_trait]
impl MediaLookup for SystemLookup {
    async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, MediaGuardError> {
        if let Ok(literal) = host.parse::<IpAddr>() {
            return Ok(vec![literal]);
        }
        let resolved = tokio::net::lookup_host((host, 0))
            .await
            .map_err(|_| MediaGuardError::Resolve)?;
        let addresses: Vec<IpAddr> = resolved.map(|address| address.ip()).collect();
        if addresses.is_empty() {
            return Err(MediaGuardError::Resolve);
        }
        Ok(addresses)
    }
}

/// 一个地址被拒时打一声招呼（上游 `onRefuse` 那个**测试钩子**；生产为 `None`）。
pub type RefuseHook = Arc<dyn Fn(&str, IpAddr) + Send + Sync>;

/// 媒体客户端连接时要过的闸（上游 `mediaGuard`）。
#[derive(Clone)]
pub struct MediaGuard {
    allow: Option<AddrPolicy>,
    lookup: Option<Arc<dyn MediaLookup>>,
    on_refuse: Option<RefuseHook>,
}

impl std::fmt::Debug for MediaGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MediaGuard")
            .field("allow", &self.allow.is_some())
            .field("lookup", &self.lookup.is_some())
            .field("on_refuse", &self.on_refuse.is_some())
            .finish()
    }
}

impl Default for MediaGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaGuard {
    /// 生产形态：公网地址策略 + 系统解析器。
    #[must_use]
    pub fn new() -> Self {
        Self {
            allow: None,
            lookup: None,
            on_refuse: None,
        }
    }

    /// 换掉地址策略（用例）。
    #[must_use]
    pub fn with_policy(mut self, policy: AddrPolicy) -> Self {
        self.allow = Some(policy);
        self
    }

    /// 换掉解析器（用例）。
    #[must_use]
    pub fn with_lookup(mut self, lookup: Arc<dyn MediaLookup>) -> Self {
        self.lookup = Some(lookup);
        self
    }

    /// 装上"被拒时打一声招呼"的钩子（用例）。
    #[must_use]
    pub fn with_refuse_hook(mut self, hook: RefuseHook) -> Self {
        self.on_refuse = Some(hook);
        self
    }

    fn policy(&self) -> AddrPolicy {
        self.allow
            .clone()
            .unwrap_or_else(|| addr_policy(public_addr_only))
    }

    fn resolver(&self) -> Arc<dyn MediaLookup> {
        self.lookup
            .clone()
            .unwrap_or_else(|| Arc::new(SystemLookup))
    }

    /// 解析一个主机名并**只**交回通过闸的地址（上游 `dial` 的前半段）。
    ///
    /// `每一个`地址都在**任何**地址被拨之前检查，而连接是打给通过了的那个字面量的。
    /// 一个混着公网与内网答案的主机：公的那些会被试、内网那些会被拒，而不是凭一个**好**答案
    /// 把整个名字放过去。
    ///
    /// # Errors
    ///
    /// [`MediaGuardError::EmptyHost`] / [`MediaGuardError::Resolve`] /
    /// [`MediaGuardError::BlockedAddress`]（**一个**地址都没通过时）。
    pub async fn lookup_allowed(&self, host: &str) -> Result<Vec<IpAddr>, MediaGuardError> {
        let host = host.trim();
        if host.is_empty() {
            return Err(MediaGuardError::EmptyHost);
        }
        let policy = self.policy();
        let addresses = self.resolver().lookup(host).await?;
        let mut allowed = Vec::with_capacity(addresses.len());
        for address in addresses {
            if policy(address) {
                allowed.push(address);
            } else if let Some(hook) = self.on_refuse.as_ref() {
                hook(host, address);
            }
        }
        if allowed.is_empty() {
            return Err(MediaGuardError::BlockedAddress);
        }
        Ok(allowed)
    }
}

// =====================================================================
// 客户端
// =====================================================================

/// `reqwest` 的解析器接缝：把闸装进**每一次**解析（含重定向的每一跳）。
#[derive(Debug, Clone)]
struct GuardedResolver {
    guard: Arc<MediaGuard>,
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let guard = Arc::clone(&self.guard);
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addresses = guard.lookup_allowed(&host).await?;
            let iterator: Box<dyn Iterator<Item = SocketAddr> + Send> = Box::new(
                addresses
                    .into_iter()
                    // 端口在这里是占位的：`reqwest` 只取 `ip()`，真正的端口来自 URL。
                    .map(|address| SocketAddr::new(address, 0)),
            );
            Ok(iterator)
        })
    }
}

/// 重定向该不该跟（上游 `newMediaHTTPClient` 的 `CheckRedirect`）。
///
/// **两道闸，管的是不同的东西**：拨号器拒一个**目的地**，这一条拒一个 **scheme** ——
/// 一个到 `file://` 或 `gopher://` 的重定向**根本不会走到拨号器**，而 transport 会高高兴兴
/// 把它交给一个协议处理器。
///
/// # Errors
///
/// [`MediaGuardError::Client`] 表示"这一跳不跟"（`reqwest` 的 `Policy` 把任何错误当成停止）。
#[must_use]
pub fn redirect_refused(scheme: &str, hops: usize) -> Option<MediaGuardError> {
    if hops >= MAX_MEDIA_REDIRECTS {
        return Some(MediaGuardError::Client);
    }
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Some(MediaGuardError::Client);
    }
    None
}

/// 建媒体下载走的那把客户端（上游 `newMediaHTTPClient`）。
///
/// # Errors
///
/// [`MediaGuardError::Client`]（TLS / 构建器失败）。
pub fn new_media_http_client(guard: MediaGuard) -> Result<reqwest::Client, MediaGuardError> {
    let guard = Arc::new(guard);
    let resolver = GuardedResolver {
        guard: Arc::clone(&guard),
    };
    reqwest::Client::builder()
        .connect_timeout(MEDIA_DIAL_TIMEOUT)
        // 一次 COS 对象的取回**不需要**代理，而在这里认 `HTTP_PROXY` 会把这次取回送到一个
        // 地址闸**从来看不见**的地方去。
        .no_proxy()
        .dns_resolver(Arc::new(resolver))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if redirect_refused(attempt.url().scheme(), attempt.previous().len()).is_some() {
                return attempt.error(MediaGuardError::Client);
            }
            attempt.follow()
        }))
        .build()
        .map_err(|_| MediaGuardError::Client)
}

#[cfg(test)]
mod tests;
