//! `TRUST_PROXY` 环境变量解析与客户端 IP 解析 — 等价于 Node `middleware/trust-proxy.ts`
//! 加上 Express 5 `trust proxy` 对 `req.ip` 的解析语义。
//!
//! 默认（未设置）不信任任何代理：`X-Forwarded-For` 不可被任意客户端伪造。
//! 运营者只有在真实 LB 之后才应显式开启。

use axum::{extract::Request, middleware::Next, response::Response};
/// 命名子网（Express 原样接受的 keyword）。
pub const NAMED_SUBNETS: [&str; 3] = ["loopback", "linklocal", "uniquelocal"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustProxyValue {
    /// `"true"` — 信任所有代理（在不可信 LB 后不安全）。
    TrustAll,
    /// 正整数 — 信任 N 跳。
    Hops(u32),
    /// 逗号分隔的命名子网 + CIDR 列表。
    Subnets(Vec<String>),
}

/// 解析结果：`Ok(None)` 表示不设置（保持 Express 安全默认）。
pub type TrustProxyResult = Result<Option<TrustProxyValue>, String>;

/// IPv4（可带 /0-32 CIDR）— 有意宽松，与 Node 正则一致。
fn is_ipv4_token(token: &str) -> bool {
    let (addr, cidr) = match token.split_once("/") {
        Some((a, c)) => (a, Some(c)),
        None => (token, None),
    };
    let octets: Vec<&str> = addr.split(".").collect();
    if octets.len() != 4 || octets.iter().any(|o| o.is_empty() || o.len() > 3) {
        return false;
    }
    if !octets.iter().all(|o| o.chars().all(|c| c.is_ascii_digit())) {
        return false;
    }
    if let Some(c) = cidr {
        let Ok(prefix) = c.parse::<u32>() else {
            return false;
        };
        if prefix > 32 {
            return false;
        }
    }
    true
}

/// IPv6（可带 /0-128 CIDR）— 宽松匹配（十六进制+冒号），与 Node 正则一致。
fn is_ipv6_token(token: &str) -> bool {
    let (addr, cidr) = match token.split_once("/") {
        Some((a, c)) => (a, Some(c)),
        None => (token, None),
    };
    // 0x3a = ASCII colon（避免字符字面量，保持 zero-single-quote 约束）
    if addr.is_empty()
        || !addr
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == 58u8 as char)
    {
        return false;
    }
    if let Some(c) = cidr {
        let Ok(prefix) = c.parse::<u32>() else {
            return false;
        };
        if prefix > 128 {
            return false;
        }
    }
    true
}

/// 单个子网 token 是否合法（Node `isValidSubnetToken`）。
pub fn is_valid_subnet_token(token: &str) -> bool {
    if NAMED_SUBNETS.contains(&token) {
        return true;
    }
    if is_ipv4_token(token) {
        return true;
    }
    // IPv6 必须至少包含一个冒号，防止纯数字串混入。
    token.contains(":") && is_ipv6_token(token)
}

/// 严格正整数：无前导零、无空白、无符号。
fn is_strict_positive_int(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    let mut chars = value.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_digit() || first.to_digit(10) == Some(0) {
        return false;
    }
    chars.all(|c| c.is_ascii_digit())
}

/// 解析 `TRUST_PROXY` 原始值（Node `parseTrustProxyEnv`）。
pub fn parse_trust_proxy_env(raw: Option<&str>) -> TrustProxyResult {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let value = raw.trim();
    if value.is_empty() || value == "false" || value == "0" {
        return Ok(None);
    }
    if value == "true" {
        return Ok(Some(TrustProxyValue::TrustAll));
    }
    if is_strict_positive_int(value) {
        let hops = value.parse::<u32>().map_err(|_| {
            format!("TRUST_PROXY: invalid integer value {raw:?} — use a positive integer with no leading zeros or whitespace")
        })?;
        return Ok(Some(TrustProxyValue::Hops(hops)));
    }
    // 纯数字/空白但没有匹配 STRICT_POS_INT_RE：是笔误而非子网列表。
    if raw.chars().all(|c| c.is_ascii_digit() || c.is_whitespace()) {
        return Err(format!(
            "TRUST_PROXY: invalid integer value {raw:?} — use a positive integer with no leading zeros or whitespace"
        ));
    }
    let tokens: Vec<String> = value
        .split(",")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return Ok(None);
    }
    for token in &tokens {
        if !is_valid_subnet_token(token) {
            return Err(format!(
                "TRUST_PROXY: unrecognized token {token:?} — expected one of {{loopback, linklocal, uniquelocal}} or a CIDR like 10.0.0.0/8 or fd00::/8"
            ));
        }
    }
    Ok(Some(TrustProxyValue::Subnets(tokens)))
}

/// 解析 IPv4 地址为 u32（大端）。
fn parse_ipv4(addr: &str) -> Option<u32> {
    let octets: Vec<u32> = addr
        .split(".")
        .map(|o| o.parse::<u32>().ok())
        .collect::<Option<Vec<_>>>()?;
    if octets.len() != 4 || octets.iter().any(|o| *o > 255) {
        return None;
    }
    Some((octets[0] << 24) | (octets[1] << 16) | (octets[2] << 8) | octets[3])
}

/// 解析点分十进制为两个 u16 组（IPv4-in-IPv6 的末尾 32 位）。
fn parse_v4_pair(v4: &str) -> Option<(u16, u16)> {
    let octets: Vec<u32> = v4
        .split(".")
        .map(|o| o.parse::<u32>().ok())
        .collect::<Option<Vec<_>>>()?;
    if octets.len() != 4 || octets.iter().any(|o| *o > 255) {
        return None;
    }
    Some((
        ((octets[0] as u16) << 8) | octets[1] as u16,
        ((octets[2] as u16) << 8) | octets[3] as u16,
    ))
}

/// 解析 IPv6 地址为 128 位 big-endian 字节（支持 `::` 压缩，tail 右对齐；
/// 支持 IPv4-in-IPv6 末尾点分，如 `::ffff:10.1.2.3`）。
fn parse_ipv6(addr: &str) -> Option<[u8; 16]> {
    if addr == "::" {
        return Some([0u8; 16]);
    }
    let (head, tail) = match addr.split_once("::") {
        Some((h, t)) => (h, Some(t)),
        None => (addr, None),
    };
    // 末尾 32 位可为点分十进制：拆出 v4 组，剩余部分仍按十六进制组解析。
    let (head_hex, tail_hex, v4_group): (&str, &str, Option<(u16, u16)>) = match tail {
        Some(t) => {
            if let Some(colon) = t.rfind(":") {
                if t[colon + 1..].contains(".") {
                    (head, &t[..colon], Some(parse_v4_pair(&t[colon + 1..])?))
                } else {
                    (head, t, None)
                }
            } else if t.contains(".") {
                (head, "", Some(parse_v4_pair(t)?))
            } else {
                (head, t, None)
            }
        }
        None => match head.rfind(":") {
            Some(colon) if head[colon + 1..].contains(".") => {
                (&head[..colon], "", Some(parse_v4_pair(&head[colon + 1..])?))
            }
            _ => (head, "", None),
        },
    };
    let head_groups: Vec<u16> = if head_hex.is_empty() {
        Vec::new()
    } else {
        head_hex
            .split(":")
            .map(|g| u16::from_str_radix(g, 16).ok())
            .collect::<Option<Vec<_>>>()?
    };
    let mut tail_groups: Vec<u16> = if tail_hex.is_empty() {
        Vec::new()
    } else {
        tail_hex
            .split(":")
            .map(|g| u16::from_str_radix(g, 16).ok())
            .collect::<Option<Vec<_>>>()?
    };
    if let Some((hi, lo)) = v4_group {
        tail_groups.push(hi);
        tail_groups.push(lo);
    }
    let total = head_groups.len() + tail_groups.len();
    if tail.is_some() {
        if total > 7 {
            return None;
        }
    } else if total != 8 {
        return None;
    }
    let mut groups = [0u16; 8];
    for (i, g) in head_groups.iter().enumerate() {
        groups[i] = *g;
    }
    let tail_start = 8 - tail_groups.len();
    for (i, g) in tail_groups.iter().enumerate() {
        groups[tail_start + i] = *g;
    }
    let mut out = [0u8; 16];
    for (i, g) in groups.iter().enumerate() {
        out[i * 2] = (g >> 8) as u8;
        out[i * 2 + 1] = (g & 0xff) as u8;
    }
    Some(out)
}

/// 判断地址是否在信任子网列表内（Node/Express proxy-addr 语义）。
/// 解析失败视为不可信（untrusted）——与 proxy-addr 一致。
pub fn is_trusted_subnet(addr: &str, subnets: &[String]) -> bool {
    let ipv4 = parse_ipv4(addr);
    let ipv6 = parse_ipv6(addr);
    let ipv4_of_v6 = ipv6.and_then(ipv4_mapped_of);
    let ipv6_of_v4 = ipv4.map(ipv4_mapped_v6);
    if ipv4.is_none() && ipv6.is_none() {
        return false;
    }
    for entry in subnets {
        let (name_or_cidr, cidr_prefix) = match entry.split_once("/") {
            Some((n, c)) => (n, c.parse::<u8>().ok()),
            None => (entry.as_str(), None),
        };
        if NAMED_SUBNETS.contains(&name_or_cidr) {
            if let Some(ip) = ipv4 {
                if matches_cidr_v4(ip, name_or_cidr) {
                    return true;
                }
            }
            if let Some(ip6) = ipv6 {
                if matches_cidr_v6(ip6, name_or_cidr)
                    || ipv4_of_v6.is_some_and(|ip| matches_cidr_v4(ip, name_or_cidr))
                {
                    return true;
                }
            }
            continue;
        }
        if let Some(ip) = ipv4 {
            if let Some((subnet, prefix)) = parse_cidr_v4(name_or_cidr, cidr_prefix) {
                if ipv4_in_prefix(ip, subnet, prefix) {
                    return true;
                }
            }
            if let Some((subnet6, prefix6)) = parse_cidr_v6(name_or_cidr, cidr_prefix) {
                if ipv6_of_v4.is_some_and(|ip6| ipv6_in_prefix(ip6, subnet6, prefix6)) {
                    return true;
                }
            }
        }
        if let Some(ip6) = ipv6 {
            if let Some((subnet, prefix)) = parse_cidr_v6(name_or_cidr, cidr_prefix) {
                if ipv6_in_prefix(ip6, subnet, prefix) {
                    return true;
                }
            }
            if let Some((subnet4, prefix4)) = parse_cidr_v4(name_or_cidr, cidr_prefix) {
                if ipv4_of_v6.is_some_and(|ip| ipv4_in_prefix(ip, subnet4, prefix4)) {
                    return true;
                }
            }
        }
    }
    false
}

fn parse_cidr_v4(token: &str, explicit_prefix: Option<u8>) -> Option<(u32, u8)> {
    let (addr, prefix) = match token.split_once("/") {
        Some((a, c)) => (a, Some(c.parse::<u8>().ok()?)),
        None => (token, None),
    };
    let prefix = prefix.or(explicit_prefix).unwrap_or(32);
    if prefix > 32 {
        return None;
    }
    Some((parse_ipv4(addr)?, prefix))
}

fn parse_cidr_v6(token: &str, explicit_prefix: Option<u8>) -> Option<([u8; 16], u8)> {
    let (addr, prefix) = match token.split_once("/") {
        Some((a, c)) => (a, Some(c.parse::<u8>().ok()?)),
        None => (token, None),
    };
    let prefix = prefix.or(explicit_prefix).unwrap_or(128);
    if prefix > 128 {
        return None;
    }
    Some((parse_ipv6(addr)?, prefix))
}

fn ipv4_in_prefix(ip: u32, subnet: u32, prefix: u8) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    ip & mask == subnet & mask
}

fn ipv6_in_prefix(ip: [u8; 16], subnet: [u8; 16], prefix: u8) -> bool {
    let full_bytes = (prefix / 8) as usize;
    let rem_bits = prefix % 8;
    for i in 0..full_bytes {
        if ip[i] != subnet[i] {
            return false;
        }
    }
    if rem_bits > 0 {
        let mask = 0xffu8 << (8 - rem_bits);
        if ip[full_bytes] & mask != subnet[full_bytes] & mask {
            return false;
        }
    }
    true
}

/// named subnet 条目在 IPv4 侧的匹配（loopback=127/8, linklocal=169.254/16, uniquelocal=10/8+172.16/12+192.168/16）。
fn matches_cidr_v4(ip: u32, name: &str) -> bool {
    match name {
        "loopback" => ipv4_in_prefix(ip, 0x7f00_0000, 8),
        "linklocal" => ipv4_in_prefix(ip, 0xa9fe_0000, 16),
        "uniquelocal" => {
            ipv4_in_prefix(ip, 0x0a00_0000, 8)
                || ipv4_in_prefix(ip, 0xac10_0000, 12)
                || ipv4_in_prefix(ip, 0xc0a8_0000, 16)
        }
        _ => false,
    }
}

/// named subnet 条目在 IPv6 侧的匹配（loopback=::1/128, linklocal=fe80::/10, uniquelocal=fc00::/7）。
/// 只匹配自身名对应的范围（此前实现把三个 named 范围混在一起，导致
/// `loopback` 误命中 fe80::/10 等）。
fn matches_cidr_v6(ip: [u8; 16], name: &str) -> bool {
    let mut loopback = [0u8; 16];
    loopback[15] = 1;
    let mut fe80 = [0u8; 16];
    fe80[0] = 0xfe;
    fe80[1] = 0x80;
    let mut fc00 = [0u8; 16];
    fc00[0] = 0xfc;
    match name {
        "loopback" => ipv6_in_prefix(ip, loopback, 128),
        "linklocal" => ipv6_in_prefix(ip, fe80, 10),
        "uniquelocal" => ipv6_in_prefix(ip, fc00, 7),
        _ => false,
    }
}

/// IPv4-mapped IPv6（`::ffff:a.b.c.d`）→ IPv4。
fn ipv4_mapped_of(ip6: [u8; 16]) -> Option<u32> {
    let mapped = ip6[0..10].iter().all(|b| *b == 0) && ip6[10] == 0xff && ip6[11] == 0xff;
    if !mapped {
        return None;
    }
    Some(
        ((ip6[12] as u32) << 24)
            | ((ip6[13] as u32) << 16)
            | ((ip6[14] as u32) << 8)
            | (ip6[15] as u32),
    )
}

/// IPv4 → IPv4-mapped IPv6（`a.b.c.d` → `::ffff:a.b.c.d`）。
fn ipv4_mapped_v6(ipv4: u32) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[10] = 0xff;
    out[11] = 0xff;
    out[12] = (ipv4 >> 24) as u8;
    out[13] = (ipv4 >> 16) as u8;
    out[14] = (ipv4 >> 8) as u8;
    out[15] = ipv4 as u8;
    out
}

/// 根据 trust 配置解析真实客户端 IP（Express `req.ip` 语义）。
/// - `None`：不信任任何代理 → socket 地址。
/// - `TrustAll`：全部可信 → XFF 最左（原始客户端），无 XFF 则 socket。
/// - `Hops(n)`：信任前 n 跳 → 第 n+1 个地址；不足则最左（全部可信）。
/// - `Subnets`：从 socket 向左走，第一个不可信地址；全可信则最左。
///
/// socket 为 `SocketAddr`（带端口），返回前剥离端口（与 Express `req.ip`
/// 只返回 IP 一致）。
pub fn resolve_client_ip(
    trust: Option<&TrustProxyValue>,
    socket_addr: Option<&str>,
    x_forwarded_for: Option<&str>,
) -> Option<String> {
    let socket = strip_port(socket_addr.unwrap_or(""));
    let mut chain: Vec<String> = vec![socket.clone()];
    if let Some(xff) = x_forwarded_for {
        let mut entries: Vec<String> = xff
            .split(",")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        entries.reverse();
        chain.extend(entries);
    }
    match trust {
        None => chain.first().cloned(),
        Some(TrustProxyValue::TrustAll) => chain.last().cloned().or_else(|| chain.first().cloned()),
        Some(TrustProxyValue::Hops(n)) => {
            let idx = *n as usize;
            chain.get(idx).cloned().or_else(|| chain.last().cloned())
        }
        Some(TrustProxyValue::Subnets(subnets)) => {
            for i in 0..chain.len() {
                if !is_trusted_subnet(&chain[i], subnets) {
                    return chain.get(i).cloned();
                }
            }
            chain.last().cloned().or_else(|| chain.first().cloned())
        }
    }
}

/// 剥离 socket 地址端口（`1.2.3.4:5555` → `1.2.3.4`，`[::1]:3100` → `::1`）；
/// 无法解析为 `SocketAddr` 时视为已是裸 IP。
fn strip_port(addr: &str) -> String {
    addr.trim()
        .parse::<std::net::SocketAddr>()
        .map(|sa| sa.ip().to_string())
        .unwrap_or_else(|_| addr.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_and_falsy_values_yield_none() {
        assert_eq!(parse_trust_proxy_env(None).unwrap(), None);
        assert_eq!(parse_trust_proxy_env(Some("")).unwrap(), None);
        assert_eq!(parse_trust_proxy_env(Some("  ")).unwrap(), None);
        assert_eq!(parse_trust_proxy_env(Some("false")).unwrap(), None);
        assert_eq!(parse_trust_proxy_env(Some("0")).unwrap(), None);
    }

    #[test]
    fn trust_all() {
        assert_eq!(
            parse_trust_proxy_env(Some("true")).unwrap(),
            Some(TrustProxyValue::TrustAll)
        );
    }

    #[test]
    fn positive_integer_hops() {
        assert_eq!(
            parse_trust_proxy_env(Some("2")).unwrap(),
            Some(TrustProxyValue::Hops(2))
        );
        assert_eq!(
            parse_trust_proxy_env(Some(" 3 ")).unwrap(),
            Some(TrustProxyValue::Hops(3))
        );
    }

    #[test]
    fn invalid_integer_forms_error() {
        assert!(parse_trust_proxy_env(Some("01")).is_err());
        assert!(parse_trust_proxy_env(Some("1 2")).is_err());
        assert!(parse_trust_proxy_env(Some("-1")).is_err());
    }

    #[test]
    fn subnet_list_parsing() {
        let parsed = parse_trust_proxy_env(Some("loopback, 10.0.0.0/8, fd00::/8")).unwrap();
        assert_eq!(
            parsed,
            Some(TrustProxyValue::Subnets(vec![
                "loopback".to_string(),
                "10.0.0.0/8".to_string(),
                "fd00::/8".to_string(),
            ]))
        );
    }

    #[test]
    fn unrecognized_token_errors() {
        let err = parse_trust_proxy_env(Some("10.0.0/8")).unwrap_err();
        assert!(err.contains("unrecognized token"));
        assert!(err.contains("10.0.0/8"));
    }

    #[test]
    fn subnet_token_validation() {
        assert!(is_valid_subnet_token("loopback"));
        assert!(is_valid_subnet_token("linklocal"));
        assert!(is_valid_subnet_token("uniquelocal"));
        assert!(is_valid_subnet_token("10.0.0.0/8"));
        assert!(is_valid_subnet_token("192.168.1.1"));
        assert!(is_valid_subnet_token("fd00::/8"));
        assert!(is_valid_subnet_token("::1"));
        assert!(!is_valid_subnet_token("10.0.0/8"));
        assert!(!is_valid_subnet_token("10"));
        assert!(!is_valid_subnet_token("gggg"));
    }

    #[test]
    fn ipv6_parsing() {
        assert_eq!(parse_ipv6("::1").unwrap()[15], 1);
        let full = parse_ipv6("2001:db8::1").unwrap();
        assert_eq!(full[0], 0x20);
        assert_eq!(full[1], 0x01);
        assert_eq!(full[15], 1);
        assert!(parse_ipv6("2001::db8::1").is_none());
    }

    #[test]
    fn trusted_subnet_matching() {
        let subnets: Vec<String> = vec!["10.0.0.0/8".into(), "loopback".into(), "fd00::/8".into()];
        assert!(is_trusted_subnet("10.1.2.3", &subnets));
        assert!(!is_trusted_subnet("11.0.0.1", &subnets));
        assert!(is_trusted_subnet("127.0.0.1", &subnets));
        assert!(is_trusted_subnet("fd12:3456::1", &subnets));
        assert!(!is_trusted_subnet("fe80::1", &subnets));
        // IPv4-mapped IPv6 命中 IPv4 子网
        assert!(is_trusted_subnet("::ffff:10.1.2.3", &subnets));
        assert!(!is_trusted_subnet("::ffff:11.0.0.1", &subnets));
    }

    #[test]
    fn named_subnets_linklocal_and_uniquelocal() {
        let linklocal: Vec<String> = vec!["linklocal".into()];
        assert!(is_trusted_subnet("169.254.0.5", &linklocal));
        assert!(is_trusted_subnet("fe80::1", &linklocal));
        assert!(!is_trusted_subnet("10.0.0.1", &linklocal));
        let uniquelocal: Vec<String> = vec!["uniquelocal".into()];
        assert!(is_trusted_subnet("192.168.1.1", &uniquelocal));
        assert!(is_trusted_subnet("172.16.0.1", &uniquelocal));
        assert!(is_trusted_subnet("10.1.2.3", &uniquelocal));
        assert!(is_trusted_subnet("fd12::1", &uniquelocal));
        assert!(!is_trusted_subnet("fe80::1", &uniquelocal));
    }

    #[test]
    fn ipv6_socket_with_bracket_and_port() {
        let ip = resolve_client_ip(None, Some("[::1]:3100"), None);
        assert_eq!(ip.as_deref(), Some("::1"));
    }

    #[test]
    fn client_ip_without_trust_uses_socket() {
        let ip = resolve_client_ip(None, Some("1.2.3.4:5555"), Some("203.0.113.5, 1.2.3.4"));
        assert_eq!(ip.as_deref(), Some("1.2.3.4"));
    }

    #[test]
    fn client_ip_trust_all_uses_leftmost_xff() {
        let ip = resolve_client_ip(
            Some(&TrustProxyValue::TrustAll),
            Some("1.2.3.4:5555"),
            Some("203.0.113.5, 10.0.0.2"),
        );
        assert_eq!(ip.as_deref(), Some("203.0.113.5"));
    }

    #[test]
    fn client_ip_hops_skips_trusted_proxies() {
        let ip = resolve_client_ip(
            Some(&TrustProxyValue::Hops(1)),
            Some("1.2.3.4:5555"),
            Some("203.0.113.5, 10.0.0.2"),
        );
        // proxy-addr 真值：hops=1 → 链上第 2 个（socket 后第一个 XFF 项）
        assert_eq!(ip.as_deref(), Some("10.0.0.2"));
        let ip2 = resolve_client_ip(
            Some(&TrustProxyValue::Hops(2)),
            Some("1.2.3.4:5555"),
            Some("203.0.113.5, 10.0.0.2"),
        );
        assert_eq!(ip2.as_deref(), Some("203.0.113.5"));
        // hops 超过链长 → 全部可信 → 最左（无 XFF 时为 socket）
        let ip3 = resolve_client_ip(
            Some(&TrustProxyValue::Hops(3)),
            Some("1.2.3.4:5555"),
            Some("203.0.113.5, 10.0.0.2"),
        );
        assert_eq!(ip3.as_deref(), Some("203.0.113.5"));
        let ip4 = resolve_client_ip(Some(&TrustProxyValue::Hops(3)), Some("1.2.3.4:5555"), None);
        assert_eq!(ip4.as_deref(), Some("1.2.3.4"));
    }

    #[test]
    fn client_ip_subnets_walks_from_socket() {
        let subnets: Vec<String> = vec!["10.0.0.0/8".into()];
        let ip = resolve_client_ip(
            Some(&TrustProxyValue::Subnets(subnets.clone())),
            Some("10.0.0.2:5555"),
            Some("203.0.113.5, 10.0.0.2"),
        );
        assert_eq!(ip.as_deref(), Some("203.0.113.5"));
        // 全部可信 → 最左
        let ip2 = resolve_client_ip(
            Some(&TrustProxyValue::Subnets(subnets)),
            Some("10.0.0.2:5555"),
            Some("10.0.0.3, 10.0.0.2"),
        );
        assert_eq!(ip2.as_deref(), Some("10.0.0.3"));
    }
}

/// 客户端 IP 扩展（由 trust_proxy_layer 注入，供 access_log / handler 读取）。
#[derive(Debug, Clone)]
pub struct ClientIp(pub String);

/// trust-proxy 配置（pc-server 启动时从 `TRUST_PROXY` 解析后注入 Extension）。
#[derive(Debug, Clone, Default)]
pub struct TrustProxyConfig {
    pub value: Option<TrustProxyValue>,
}

/// 解析客户端 IP 中间件（from_fn 形式）：按 trust 配置从 `X-Forwarded-For`
/// 解析真实客户端 IP 并注入 `ClientIp` extension。默认（未配置）不信任任何代理。
pub async fn trust_proxy_layer(mut req: Request, next: Next) -> Response {
    let cfg = req
        .extensions()
        .get::<TrustProxyConfig>()
        .cloned()
        .unwrap_or_default();
    let socket = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.to_string());
    let xff = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok());
    if let Some(ip) = resolve_client_ip(cfg.value.as_ref(), socket.as_deref(), xff) {
        req.extensions_mut().insert(ClientIp(ip));
    }
    next.run(req).await
}
