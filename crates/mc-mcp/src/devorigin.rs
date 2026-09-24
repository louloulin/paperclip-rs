//! 开发态 origin 白名单 + endpoint 信任判定（纯函数，**不读环境变量**）。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。
//! - **上游**：`pkg/remotemcp/devorigin.go`（77 行，`isDevOrigin` / `devTLSConfig`）+
//!   `pkg/remotemcp/client.go` 的 `ValidatePublicHTTPSEndpoint` / `hostAllowed` /
//!   `isPublicAddress`（这三条是同一个「管理员同意过的目的地」判据的三个面，放一起才看得全）。
//! - **为什么合并到一个文件**：上游把「白名单」与「地址是否公开」分在两个文件，但它们的
//!   调用点只有一处（拨号前），且**必须一起读**才能确认「dev origin 只跳过『必须在公网』
//!   这一条，没有跳过 host 白名单」。本仓按引用点拆文件（R7），所以它们同处一室。
//!
//! ## env 为什么在这里只留名字、不落在读值
//!
//! 上游 `isDevOrigin` 直接 `os.Getenv`（server 与 daemon 是两个进程，所以逐次查而不是
//! 启动时缓存）。本仓把**环境读取集中到入口层**（`mc-http` 的 `AppState` / daemon 配置），
//! 否则「同一份判定」在两处会有两份 env 解析。于是本模块收 [`EndpointPolicy`] 作为入参，
//! 只导出 [`DEV_ORIGINS_ENV`] / [`DEV_CA_ENV`] 两个名字供入口层读取。
//!
//! ## 不做什么
//!
//! - **不**做「跳过证书校验」：dev origin 换来的是「信任这张额外的 CA」，不是「不验证」。
//!   manifest 校验器要求 `https://`，放宽它等于让管理员同意过的那个 URL 不再说明连接是否加密。
//! - **不**在此处读 CA 文件：上游 `devTLSConfig` 读 `MULTICA_PLUGIN_DEV_CA` 指向的文件；
//!   本仓由入口层读成 PEM 字节后经 [`EndpointPolicy::dev_ca_pem`] 传入（同一份文件 IO 只有一处）。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Position, Url};

/// 开发态 origin 白名单的环境变量名（server 与 daemon 都读它）。
pub const DEV_ORIGINS_ENV: &str = "MULTICA_PLUGIN_DEV_ORIGINS";

/// 为这些 origin 额外信任的 CA bundle 路径（**仍然**是 HTTPS）。
pub const DEV_CA_ENV: &str = "MULTICA_PLUGIN_DEV_CA";

/// 「管理员同意过的目的地」的全部判据。
///
/// - `allowed_hosts`：同意界面上展示过的精确主机集合（可含 `*.example.com` 通配）。
///   **空集合 = 不按主机收窄**（此时唯一的收窄是 `net:` scope，与上游一致）。
/// - `dev_origins`：允许「不必在公网」的精确 origin（`scheme://host:port`）。
/// - `dev_ca_pem`：给这些 origin 额外信任的 CA（PEM 字节）。
#[derive(Debug, Clone, Default)]
pub struct EndpointPolicy {
    allowed_hosts: Vec<String>,
    dev_origins: Vec<String>,
    dev_ca_pem: Option<Vec<u8>>,
}

impl EndpointPolicy {
    /// 只给主机白名单（生产路径）。
    #[must_use]
    pub fn new(allowed_hosts: Vec<String>) -> Self {
        Self {
            allowed_hosts,
            dev_origins: Vec::new(),
            dev_ca_pem: None,
        }
    }

    /// 入口层用：把 env 里读到的原始值（逗号分隔）与 CA 字节装配成策略。
    #[must_use]
    pub fn from_values(
        allowed_hosts: &[String],
        dev_origins_config: &str,
        dev_ca_pem: Option<Vec<u8>>,
    ) -> Self {
        Self {
            allowed_hosts: allowed_hosts.to_vec(),
            dev_origins: parse_origins(dev_origins_config),
            dev_ca_pem,
        }
    }

    /// 精确 origin 命中（`scheme://host:port`）。
    #[must_use]
    pub fn is_dev_origin(&self, endpoint: &Url) -> bool {
        is_dev_origin(endpoint, &self.dev_origins)
    }

    /// 同一个 dev-origin 配置，但**去掉 host 白名单**。
    ///
    /// OAuth 那条路径（`oauth.rs`）按上游给 `allowedHosts = nil`：只按「公网 HTTPS +
    /// 地址全公网」收窄，不看插件 endpoint 的 host 白名单 —— 否则授权服务器/token endpoint
    /// 会被 endpoint 的白名单顺带收窄，那既不是上游行为，也不是运维的预期。
    /// dev origin 仍然有效（`https://` 的 dev origin 允许解析到私网地址）。
    #[must_use]
    pub fn without_host_policy(&self) -> Self {
        Self {
            allowed_hosts: Vec::new(),
            dev_origins: self.dev_origins.clone(),
            dev_ca_pem: self.dev_ca_pem.clone(),
        }
    }

    /// 主机是否在同意集合内（空集合 = 放行，见类型文档）。
    #[must_use]
    pub fn allows_host(&self, host: &str) -> bool {
        self.allowed_hosts.is_empty() || host_allowed(host, &self.allowed_hosts)
    }

    /// 给 dev origin 额外信任的 CA（`None` = 不改传输层默认值）。
    #[must_use]
    pub fn dev_ca_pem(&self) -> Option<&[u8]> {
        self.dev_ca_pem.as_deref()
    }

    /// 是否配置了 dev origin（部署里没配 ⇒ 下面所有分支都退回原判据）。
    #[must_use]
    pub fn has_dev_origins(&self) -> bool {
        !self.dev_origins.is_empty()
    }
}

/// 解析 `MULTICA_PLUGIN_DEV_ORIGINS`：逗号分隔、逐项 trim、丢掉空项。
#[must_use]
pub fn parse_origins(config: &str) -> Vec<String> {
    config
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// 主机规范化：小写 + 去掉尾部点（`Example.COM.` 与 `example.com` 是同一台机器）。
#[must_use]
pub fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_lowercase()
}

/// 主机是否被策略命中。
///
/// 通配只支持**前缀式的一层**（`*.example.com`），且 **`example.com` 本身不被命中**：
/// 否则一个 `*.example.com` 条目就等于顺手同意了根域，而同意界面上并没有列它。
#[must_use]
pub fn host_allowed(host: &str, policies: &[String]) -> bool {
    let host = normalize_host(host);
    policies.iter().any(|policy| {
        let policy = normalize_host(policy);
        if host == policy {
            return true;
        }
        match policy.strip_prefix('*') {
            Some(suffix) if !suffix.is_empty() => {
                host.ends_with(suffix) && host != suffix.trim_start_matches('.')
            }
            _ => false,
        }
    })
}

/// 这个 endpoint 是不是管理员点名的 dev origin。
///
/// 逐字比较 `scheme://host:port`：前缀或后缀比较会让
/// `http://127.0.0.1:9000` 顺带授权 `http://127.0.0.1:9000.example.com`。
#[must_use]
pub fn is_dev_origin(endpoint: &Url, dev_origins: &[String]) -> bool {
    if endpoint.host_str().is_none_or(str::is_empty) {
        return false;
    }
    let origin = endpoint_origin(endpoint);
    dev_origins.iter().any(|entry| entry.trim() == origin)
}

/// endpoint 的比较形态 `scheme://host:port`（无显式端口时不含端口）。
///
/// 判定与展示共用它：否则「比较形态」会有第二份实现。
#[must_use]
pub fn endpoint_origin(endpoint: &Url) -> String {
    format!(
        "{}://{}",
        endpoint.scheme(),
        &endpoint[Position::BeforeHost..Position::AfterPort]
    )
}

/// 地址是否在公网（上游 `isPublicAddress`）。
///
/// 判据是**逐个 IANA 特殊用途段**写死的，不用 `std` 的 `is_global_unicast` 之类
/// （本仓 `rust-version = 1.80`，而 `Ipv6Addr::is_unique_local` 等要到 1.84）：
/// 安全边界上「枚举出来」比「相信标准库的版本」更经得起复查。
#[must_use]
pub fn is_public_address(address: IpAddr) -> bool {
    match unmap_address(address) {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

/// 上游 `netip.Addr.Unmap()`：v4-mapped 的 v6 地址还原成 v4，其余原样。
///
/// `is_public_address` 与拨号前的地址过滤都走它 —— 「什么时候可以退化成 v4」只此一处。
#[must_use]
pub fn unmap_address(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        IpAddr::V4(v4) => IpAddr::V4(v4),
    }
}

fn is_public_v4(address: Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();
    if address.is_loopback() || address.is_private() || address.is_link_local() {
        return false;
    }
    if address.is_multicast() || address.is_unspecified() || address.is_broadcast() {
        return false;
    }
    // 上游 client.go `isPublicAddress` 的 `blocked` 列表（v4 部分，逐段写开）。
    // 组播/广播已在上面判掉，这里也逐条列出 —— 对账时看得见「上游这一行落在哪」。
    let blocked = matches!(
        (first, second, third),
        (100, 64..=127, _)      // 100.64.0.0/10    运营商级 NAT
            | (192, 0, 0 | 2)   // 192.0.0.0/24 + 192.0.2.0/24（IETF 协议分配 + TEST-NET-1）
            | (198, 18..=19, _) // 198.18.0.0/15   基准测试
            | (198, 51, 100)    // 198.51.100.0/24 TEST-NET-2
            | (203, 0, 113)     // 203.0.113.0/24  TEST-NET-3
            | (224..=255, _, _) // 224.0.0.0/4 组播 + 240.0.0.0/4 保留
    );
    !blocked
}

fn is_public_v6(address: Ipv6Addr) -> bool {
    if address.is_loopback() || address.is_unspecified() || address.is_multicast() {
        return false;
    }
    let segments = address.segments();
    // 上游 `blocked` 里对 v6 的三段：2001:db8::/32、fc00::/7、fe80::/10。
    !matches!(
        segments[0],
        0x2001 if segments[1] == 0x0db8
    ) && !(0xfc00..=0xfdff).contains(&segments[0])
        && !(0xfe80..=0xfebf).contains(&segments[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    fn addr(raw: &str) -> IpAddr {
        raw.parse().unwrap()
    }

    #[test]
    fn dev_origin_matches_exact_scheme_host_port_only() {
        let origins = parse_origins(" http://127.0.0.1:9000 , ,https://dev.internal ");
        assert_eq!(origins, ["http://127.0.0.1:9000", "https://dev.internal"]);

        assert!(is_dev_origin(&url("http://127.0.0.1:9000/rpc"), &origins));
        // 端口不同 ⇒ 不是同一个 origin。
        assert!(!is_dev_origin(&url("http://127.0.0.1:9001/rpc"), &origins));
        // 后缀命中是经典陷阱：`dev.internal.example.com` 不是 `dev.internal`。
        assert!(!is_dev_origin(
            &url("https://dev.internal.example.com/rpc"),
            &origins
        ));
        // scheme 也必须一致。
        assert!(!is_dev_origin(&url("https://127.0.0.1:9000/rpc"), &origins));
        // 没配就是空策略 —— 生产部署正是这一支。
        assert!(!is_dev_origin(&url("http://127.0.0.1:9000/rpc"), &[]));
    }

    #[test]
    fn host_policy_does_not_widen_a_wildcard_to_the_root_domain() {
        let policies = parse_origins("*.example.com, api.internal, Example.NET.");
        assert!(host_allowed("mcp.example.com", &policies));
        assert!(host_allowed("MCP.Example.com", &policies));
        // `*.example.com` 不包含 `example.com` 本身。
        assert!(!host_allowed("example.com", &policies));
        // 前缀拼接骗不过后缀比较。
        assert!(!host_allowed("evilexample.com", &policies));
        assert!(host_allowed("api.internal.", &policies));
        assert!(host_allowed("example.net", &policies));
        assert!(!host_allowed("other.net", &policies));
    }

    #[test]
    fn empty_host_list_means_no_host_narrowing() {
        let policy = EndpointPolicy::default();
        assert!(policy.allows_host("anything.example.com"));
        assert!(!policy.has_dev_origins());

        let policy = EndpointPolicy::from_values(&["mcp.example.com".into()], "", None);
        assert!(policy.allows_host("mcp.example.com"));
        assert!(!policy.allows_host("other.example.com"));
        assert!(policy.dev_ca_pem().is_none());
    }

    #[test]
    fn public_address_filter_covers_the_iana_ranges() {
        for public in [
            "8.8.8.8",
            "1.1.1.1",
            "100.128.0.1", // 100.64.0.0/10 之外
            "198.20.0.1",  // 198.18.0.0/15 之外
            "2606:4700::1111",
            "::ffff:8.8.8.8", // v4-mapped 公开地址照旧放行
        ] {
            assert!(is_public_address(addr(public)), "{public} 应判为公网");
        }

        assert_eq!(unmap_address(addr("::ffff:127.0.0.1")), addr("127.0.0.1"));
        assert_eq!(unmap_address(addr("2001:db8::1")), addr("2001:db8::1"));

        for private in [
            "0.0.0.0",
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.1.1",      // link-local
            "100.64.0.1",       // 运营商 NAT
            "100.127.255.255",  // /10 的上界
            "192.0.0.5",        // IETF 协议分配
            "192.0.2.5",        // TEST-NET-1
            "198.18.0.1",       // 基准测试
            "198.51.100.5",     // TEST-NET-2
            "203.0.113.9",      // TEST-NET-3
            "224.0.0.1",        // 组播
            "240.0.0.1",        // 保留
            "255.255.255.255",  // 广播
            "::1",              // v6 回环
            "::",               // 未指定
            "fe80::1",          // v6 link-local
            "fc00::1",          // v6 唯一本地
            "fd12:3456::1",     // /7 的另一半
            "2001:db8::1",      // v6 文档段
            "::ffff:127.0.0.1", // v4-mapped 回环（先 unmap 再判）
            "::ffff:192.168.0.1",
        ] {
            assert!(!is_public_address(addr(private)), "{private} 不该判为公网");
        }
    }
}
