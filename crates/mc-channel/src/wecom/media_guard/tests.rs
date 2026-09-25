//! `media_guard` 的用例：**地址判据的正面与反面**、白名单的边界、拨号器的过滤、
//! 以及把闸真正装进 `reqwest` 客户端之后的三条端到端行为。
//!
//! `DoD` 的硬要求在这里：**非白名单 CIDR 一律拒**是一条**反例**用例
//! （[`a_cidr_outside_the_allow_list_is_still_refused`]），不是可选项。
//!
//! 白名单是**进程全局**的（上游也是）⇒ 碰它的用例串行（[`ALLOW_LOCK`]）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

/// 白名单是进程全局的（上游也是）⇒ 碰它的用例串行。
///
/// 用 `tokio` 的互斥量：异步用例只能带着它**跨 await**（`#[tokio::test]` 里
/// `blocking_lock()` 会 panic）；纯 `#[test]` 那头用 `blocking_lock()`（那里没有运行时）。
static ALLOW_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 用一份白名单跑一段断言，然后恢复成空白名单（默认态）。
fn with_allow_list(cidrs: &[&str], body: impl FnOnce()) {
    let _guard = ALLOW_LOCK.blocking_lock();
    let owned: Vec<String> = cidrs.iter().map(|entry| (*entry).to_owned()).collect();
    let errors = set_media_allowed_prefixes(&owned);
    assert!(errors.is_empty(), "test setup: {errors:?}");
    body();
    let errors = set_media_allowed_prefixes(&[]);
    assert!(errors.is_empty(), "test teardown: {errors:?}");
}

fn addr(text: &str) -> IpAddr {
    text.parse().expect("ip literal")
}

// =====================================================================
// 前缀
// =====================================================================

#[test]
fn prefixes_parse_and_contain() {
    let cgnat = Prefix::parse("100.64.0.0/10").expect("parse");
    assert_eq!(cgnat.bits(), 10);
    assert_eq!(cgnat.address(), addr("100.64.0.0"));
    assert!(cgnat.contains(addr("100.64.0.1")));
    assert!(cgnat.contains(addr("100.127.255.255")));
    assert!(!cgnat.contains(addr("100.128.0.1")));
    assert!(!cgnat.contains(addr("10.0.0.1")));
    // 跨族永不包含。
    assert!(!cgnat.contains(addr("::1")));
    assert!(!cgnat.contains(addr("::ffff:100.64.0.1")));

    let docs = Prefix::parse("2001:db8::/32").expect("parse");
    assert!(docs.contains(addr("2001:db8::1")));
    assert!(!docs.contains(addr("2001:db9::1")));

    // /0 与 /32 两个端点。
    assert!(Prefix::parse("0.0.0.0/0")
        .expect("parse")
        .contains(addr("203.0.113.9")));
    let host = Prefix::parse("93.184.216.34/32").expect("parse");
    assert!(host.contains(addr("93.184.216.34")));
    assert!(!host.contains(addr("93.184.216.35")));
}

#[test]
fn prefixes_refuse_malformed_input_and_host_bits() {
    assert_eq!(Prefix::parse("100.64.0.0"), Err(PrefixError::Malformed));
    assert_eq!(Prefix::parse("100.64.0.0/"), Err(PrefixError::Malformed));
    assert_eq!(Prefix::parse("100.64.0.0/x"), Err(PrefixError::Malformed));
    assert_eq!(Prefix::parse("nope/8"), Err(PrefixError::BadAddress));
    assert_eq!(Prefix::parse("100.64.0.0/33"), Err(PrefixError::BadBits));
    assert_eq!(Prefix::parse("2001:db8::/129"), Err(PrefixError::BadBits));
    // `netip.ParsePrefix` 也拒这一条：前缀位之外有 1。
    assert_eq!(Prefix::parse("10.0.0.1/8"), Err(PrefixError::HostBitsSet));
    assert_eq!(
        Prefix::parse("2001:db8::1/32"),
        Err(PrefixError::HostBitsSet)
    );
}

// =====================================================================
// 地址判据
// =====================================================================

/// 🔴 **`DoD` 的核心反例**：白名单里**没有**的那个 CIDR 一律拒。
///
/// 四种拼法都被试过：白名单空着、白名单指向**别的**段、白名单 `::/0`、白名单精确到同族
/// 但不覆盖的邻段。
#[test]
fn a_cidr_outside_the_allow_list_is_still_refused() {
    // ① 空白名单：Tailscale 的 CGNAT 与代理的 fake-IP 池都被拒。
    with_allow_list(&[], || {
        assert!(!public_addr_only(addr("100.64.0.1")));
        assert!(!public_addr_only(addr("198.18.0.5")));
    });
    // ② 白名单指向**别的**段：这两条仍然被拒。
    with_allow_list(&["203.0.113.0/24"], || {
        assert!(!public_addr_only(addr("100.64.0.1")));
        assert!(!public_addr_only(addr("198.18.0.5")));
        // 而它声明的那一段确实开了。
        assert!(public_addr_only(addr("203.0.113.9")));
    });
    // ③ `::/0` 够不到 IPv4 的保留段（不同族），也够不到翻译空间。
    with_allow_list(&["::/0"], || {
        assert!(!public_addr_only(addr("100.64.0.1")));
        assert!(!public_addr_only(addr("198.18.0.5")));
    });
    // ④ 别的保留段不覆盖（`/15` 本身横跨两个 /16 ⇒ 拿一段**不相邻**的来当对照组）。
    with_allow_list(&["198.18.0.0/15"], || {
        assert!(public_addr_only(addr("198.18.0.5")));
        assert!(public_addr_only(addr("198.19.255.255")));
        assert!(!public_addr_only(addr("100.64.0.1")));
        assert!(!public_addr_only(addr("203.0.113.9")));
    });
}

/// 一个真实部署形态：假 IP 代理把每个公网主机名都答成池子里的地址。
#[test]
fn the_allow_list_reopens_a_fake_ip_proxy_pool() {
    with_allow_list(&[], || {
        assert!(!public_addr_only(addr("198.18.7.7")));
    });
    with_allow_list(&["198.18.0.0/15"], || {
        assert!(public_addr_only(addr("198.18.7.7")));
        // 同一条声明的两端。
        assert!(public_addr_only(addr("198.18.0.0")));
        assert!(public_addr_only(addr("198.19.255.255")));
    });
}

/// 标准库那几条判据覆盖的段：**任何**白名单都不能重开它们。
#[test]
fn loopback_private_and_link_local_cannot_be_reopened_by_any_allow_list() {
    for cidrs in [
        vec!["::/0"],
        vec!["0.0.0.0/0", "::/0"],
        vec!["127.0.0.0/8", "10.0.0.0/8", "169.254.0.0/16", "fe80::/10"],
    ] {
        with_allow_list(&cidrs, || {
            for text in [
                "127.0.0.1",
                "10.1.2.3",
                "172.16.1.1",
                "192.168.1.1",
                "169.254.169.254",
                "::1",
                "fc00::1",
                "fe80::1",
            ] {
                assert!(
                    !public_addr_only(addr(text)),
                    "{text} 不该被 {} 打开",
                    cidrs.join(",")
                );
            }
        });
    }
}

/// 翻译空间是**硬闸**：`::/0` 或 `2002::/16` 都打不开它。
#[test]
fn translation_space_cannot_be_reopened() {
    with_allow_list(
        &["::/0", "2002::/16", "64:ff9b::/96", "64:ff9b:1::/48"],
        || {
            // 64:ff9b:1::a9fe:a9fe 在 NAT64 翻译器眼里就是 169.254.169.254。
            assert!(!public_addr_only(addr("64:ff9b:1::a9fe:a9fe")));
            // 64:ff9b::7f00:1 是 127.0.0.1。
            assert!(!public_addr_only(addr("64:ff9b::7f00:1")));
            // 2002:7f00:1::1 经 6to4 中继就是 127.0.0.1。
            assert!(!public_addr_only(addr("2002:7f00:1::1")));
        },
    );
}

/// 判据表：上游那两张表 + 标准库判据覆盖到的地址逐条被拒，公网地址逐条通过。
#[test]
fn the_guard_refuses_every_non_public_range_and_allows_the_public_internet() {
    with_allow_list(&[], || {
        for text in [
            "0.0.0.0",
            "0.1.2.3",
            "100.64.0.1",
            "100.127.255.255",
            "192.0.0.1",
            "192.0.2.1",
            "192.88.99.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.1",
            "203.0.113.1",
            "240.0.0.1",
            "255.255.255.255",
            "::ffff:127.0.0.1",
            "::ffff:100.64.0.1",
            "100::1",
            "100:0:0:1::1",
            "2001::1",
            "2001:2::1",
            "2001:db8::1",
            "3fff::1",
            "5f00::1",
            "fec0::1",
        ] {
            assert!(!public_addr_only(addr(text)), "{text} 应当被拒");
        }
        for text in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "203.0.114.1",
            "2606:4700:4700::1111",
            "2a00:1450:4001:800::200e",
        ] {
            assert!(public_addr_only(addr(text)), "{text} 应当是公网");
        }
    });
}

/// IPv4-mapped 的 IPv6 必须**先解包**再判 —— 否则 `::ffff:127.0.0.1` 一条判据都不报。
#[test]
fn ipv4_mapped_addresses_are_unmapped_before_being_judged() {
    assert_eq!(unmap(addr("::ffff:127.0.0.1")), addr("127.0.0.1"));
    assert_eq!(unmap(addr("::ffff:10.0.0.1")), addr("10.0.0.1"));
    assert_eq!(unmap(addr("2001:db8::1")), addr("2001:db8::1"));
    assert_eq!(unmap(addr("1.2.3.4")), addr("1.2.3.4"));
    with_allow_list(&[], || {
        assert!(!public_addr_only(addr("::ffff:169.254.169.254")));
        // 映射过来的**公网**地址仍然是公网。
        assert!(public_addr_only(addr("::ffff:93.184.216.34")));
    });
}

// =====================================================================
// 白名单的解析与装载
// =====================================================================

#[test]
fn unparseable_entries_are_reported_and_skipped_not_silently_applied() {
    let _guard = ALLOW_LOCK.blocking_lock();
    let errors = set_media_allowed_prefixes(&[
        "198.18.0.0/15".to_owned(),
        "not-a-cidr".to_owned(),
        "999.1.1.1/8".to_owned(),
        "   ".to_owned(),
        "10.0.0.1/8".to_owned(),
    ]);
    // 三个坏条目各自报一条、**逐条区分**；空白条目**不是**错误（上游 `raw == ""` 直接 continue）。
    assert_eq!(
        errors,
        vec![
            PrefixError::Malformed,
            PrefixError::BadAddress,
            PrefixError::HostBitsSet,
        ]
    );
    assert_eq!(media_allowed_prefixes().len(), 1);
    assert!(public_addr_only(addr("198.18.0.5")));
    // 悄悄加宽也是错的：**别的**保留段仍然拒。
    assert!(!public_addr_only(addr("100.64.0.1")));
    assert!(!public_addr_only(addr("203.0.113.9")));
    let errors = set_media_allowed_prefixes(&[]);
    assert!(errors.is_empty());
    assert!(media_allowed_prefixes().is_empty());
}

#[test]
fn the_env_value_is_split_on_commas() {
    assert_eq!(parse_media_allow_cidrs(""), Vec::<String>::new());
    assert_eq!(parse_media_allow_cidrs("  "), Vec::<String>::new());
    assert_eq!(
        parse_media_allow_cidrs("198.18.0.0/15, ::/0 ,,"),
        vec!["198.18.0.0/15".to_owned(), "::/0".to_owned()]
    );
}

// =====================================================================
// 拨号器
// =====================================================================

/// 一个脚本化的解析器：每次调用交出一组地址（用尽 ⇒ `Resolve`）。
#[derive(Debug, Default)]
struct ScriptedLookup {
    answers: Mutex<Vec<Vec<IpAddr>>>,
    calls: AtomicUsize,
}

impl ScriptedLookup {
    fn new(answers: Vec<Vec<IpAddr>>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl MediaLookup for ScriptedLookup {
    async fn lookup(&self, _host: &str) -> Result<Vec<IpAddr>, MediaGuardError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut answers = self
            .answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if answers.is_empty() {
            return Err(MediaGuardError::Resolve);
        }
        Ok(answers.remove(0))
    }
}

/// 一个混着公私答案的主机：公的那些被留下、内网那些被拒（**不是**凭一个好答案整体放行）。
#[tokio::test]
async fn the_dialer_keeps_only_the_addresses_that_passed() {
    let refused: Arc<Mutex<Vec<IpAddr>>> = Arc::new(Mutex::new(Vec::new()));
    let hook_sink = Arc::clone(&refused);

    let live = ALLOW_LOCK.lock().await;
    assert!(set_media_allowed_prefixes(&[]).is_empty());
    drop(live);
    let lookup = ScriptedLookup::new(vec![vec![addr("93.184.216.34"), addr("10.0.0.1")]]);
    let guard = MediaGuard::new()
        .with_lookup(lookup)
        .with_refuse_hook(Arc::new(move |_host, address| {
            hook_sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(address);
        }));
    let allowed = guard.lookup_allowed("media.example").await.expect("mixed");
    assert_eq!(allowed, vec![addr("93.184.216.34")]);
    assert_eq!(
        *refused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![addr("10.0.0.1")]
    );
}

/// 每一个答案都被拒 ⇒ `BlockedAddress`（上游 `ErrMediaAddrBlocked`）。
#[tokio::test]
async fn a_host_that_only_resolves_internally_is_blocked() {
    let live = ALLOW_LOCK.lock().await;
    assert!(set_media_allowed_prefixes(&[]).is_empty());
    drop(live);
    let lookup = ScriptedLookup::new(vec![vec![addr("100.64.0.1"), addr("198.18.0.5")]]);
    let guard = MediaGuard::new().with_lookup(lookup);
    assert_eq!(
        guard.lookup_allowed("cos.example.com.cn").await,
        Err(MediaGuardError::BlockedAddress)
    );
}

/// 闸的判决用的是**当轮**那张表：空白名单时拒，声明那段之后放行。
#[tokio::test]
async fn the_dialer_consults_the_live_allow_list() {
    let _guard = ALLOW_LOCK.lock().await;
    assert!(set_media_allowed_prefixes(&[]).is_empty());
    let lookup = ScriptedLookup::new(vec![vec![addr("198.18.0.5")]]);
    let guard = MediaGuard::new().with_lookup(lookup);
    assert_eq!(
        guard.lookup_allowed("cos.example.com.cn").await,
        Err(MediaGuardError::BlockedAddress)
    );

    let errors = set_media_allowed_prefixes(&["198.18.0.0/15".to_owned()]);
    assert!(errors.is_empty());
    let lookup = ScriptedLookup::new(vec![vec![addr("198.18.0.5")]]);
    let guard = MediaGuard::new().with_lookup(lookup);
    assert_eq!(
        guard.lookup_allowed("cos.example.com.cn").await,
        Ok(vec![addr("198.18.0.5")])
    );
    assert!(set_media_allowed_prefixes(&[]).is_empty());
}

/// **重绑定**：同一个主机名两次解析可以给出**不同**的答案，而每次连接都要重新过闸 ——
/// 于是"先答公网、后答内网"那次拿不到连接。
#[tokio::test]
async fn a_rebinding_host_is_checked_on_every_connection() {
    let live = ALLOW_LOCK.lock().await;
    assert!(set_media_allowed_prefixes(&[]).is_empty());
    drop(live);
    let lookup = ScriptedLookup::new(vec![
        vec![addr("93.184.216.34")],
        vec![addr("169.254.169.254")],
    ]);
    let counter = Arc::clone(&lookup);
    let guard = MediaGuard::new().with_lookup(lookup);
    assert_eq!(
        guard.lookup_allowed("rebind.example").await,
        Ok(vec![addr("93.184.216.34")])
    );
    assert_eq!(
        guard.lookup_allowed("rebind.example").await,
        Err(MediaGuardError::BlockedAddress),
        "第二次解析出来的内网地址必须被拒 —— 缓存的判断正是重绑定的窗口"
    );
    assert_eq!(counter.calls(), 2, "每次连接都重新解析");
}

/// 空主机名与解析失败是两个不同的错误。
#[tokio::test]
async fn empty_hosts_and_failed_lookups_are_distinct() {
    let live = ALLOW_LOCK.lock().await;
    assert!(set_media_allowed_prefixes(&[]).is_empty());
    drop(live);
    let guard = MediaGuard::new().with_lookup(ScriptedLookup::new(vec![]));
    assert_eq!(
        guard.lookup_allowed("   ").await,
        Err(MediaGuardError::EmptyHost)
    );
    assert_eq!(
        guard.lookup_allowed("gone.example").await,
        Err(MediaGuardError::Resolve)
    );
}

#[test]
fn redirects_off_the_web_are_refused_and_the_hop_budget_is_bounded() {
    assert_eq!(redirect_refused("https", 0), None);
    assert_eq!(redirect_refused("HTTP", 1), None);
    for scheme in ["file", "gopher", "ftp", "data", "ws"] {
        assert!(redirect_refused(scheme, 0).is_some(), "{scheme} 不该被跟随");
    }
    assert!(redirect_refused("https", MAX_MEDIA_REDIRECTS).is_some());
    assert_eq!(redirect_refused("https", MAX_MEDIA_REDIRECTS - 1), None);
}

// =====================================================================
// 装进 reqwest 客户端之后
// =====================================================================

/// 一个脚本化的一次性 HTTP/1.1 服务端：每条连接吐一条响应，然后关掉。
async fn spawn_stub(responses: Vec<(u16, String)>) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        for (status, extra) in responses {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let _ = read_head(&mut socket).await;
            // 200 的 body 就是 `extra`（别的状态码把 `extra` 用在别的地方）。
            let body = if status == 200 {
                extra.clone()
            } else {
                format!("{status} {extra}")
            };
            let response = match status {
                200 => format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                ),
                302 => format!(
                    "HTTP/1.1 302 Found\r\nlocation: {extra}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                ),
                _ => format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                ),
            };
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
            let _ = socket.shutdown().await;
        }
    });
    port
}

/// 读到请求头结束（`\r\n\r\n`）为止。
async fn read_head(socket: &mut tokio::net::TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
        }
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

/// 只用回环策略（上游那个测试钩子的用法：被测试的是闸的判决，不是脚手架的地址）。
fn loopback_policy() -> AddrPolicy {
    addr_policy(|address| {
        matches!(unmap(address), IpAddr::V4(v4) if v4.is_loopback())
            || matches!(unmap(address), IpAddr::V6(v6) if v6.is_loopback())
    })
}

/// 闸**真的**装在了客户端上：允许回环时 `localhost` 通得过，默认（只许公网）时被拒。
///
/// 用主机名而不是 IP 字面量是**故意**的：`reqwest` 对 IP 字面量根本不调解析器
/// （差异登记 `docs/32` §35 的 D3；字面量那一步由 `media_download::check_media_url` 兜）。
#[tokio::test]
async fn the_guard_is_wired_into_the_client() {
    let port = spawn_stub(vec![(200, "ok".to_owned())]).await;
    let client =
        new_media_http_client(MediaGuard::new().with_policy(loopback_policy())).expect("client");
    let response = client
        .get(format!("http://localhost:{port}/ok"))
        .send()
        .await
        .expect("guarded dial");
    assert_eq!(response.status().as_u16(), 200);

    // 同一个地址、同一把客户端，只是闸换回生产策略 ⇒ 连不上。
    let refused = new_media_http_client(MediaGuard::new()).expect("client");
    assert!(
        refused
            .get(format!("http://localhost:{port}/ok"))
            .send()
            .await
            .is_err(),
        "生产策略下 localhost 必须连不上"
    );
}

/// 到非 web scheme 的重定向：**两条路都不跟随**，而两条路分属两层。
///
/// - `gopher://…` 能被 `http::Uri` 表示 ⇒ 走到重定向策略 ⇒ **我们**拒它（`Err`）；
/// - `file:///…` 连 `http::Uri` 都表示不了（`tower-http` 的 `resolve_uri` 交 `None`）
///   ⇒ 响应原样交回，**根本不会被跟随**：调用方拿到的是一个 302，而
///   [`super::super::media_download`] 把非 2xx 判成失败。
///
/// 两层都要钉住：只看 `Err` 会把"302 原样回来"读成"跟随了"。
#[tokio::test]
async fn a_redirect_off_the_web_is_never_followed() {
    let port = spawn_stub(vec![
        (302, "gopher://169.254.169.254:70/1".to_owned()),
        (200, "ok".to_owned()),
    ])
    .await;
    let client =
        new_media_http_client(MediaGuard::new().with_policy(loopback_policy())).expect("client");
    let error = client
        .get(format!("http://localhost:{port}/redirect"))
        .send()
        .await
        .expect_err("gopher 重定向必须被拒");
    assert!(
        error.to_string().contains("redirect") || error.is_redirect(),
        "{error}"
    );

    // `file://` 那一半：请求回到 302 本身，而且**没有**第二次请求（替身的第二条脚本没被消耗）。
    let port = spawn_stub(vec![
        (302, "file:///etc/passwd".to_owned()),
        (200, "should never be fetched".to_owned()),
    ])
    .await;
    let client =
        new_media_http_client(MediaGuard::new().with_policy(loopback_policy())).expect("client");
    let response = client
        .get(format!("http://localhost:{port}/redirect"))
        .send()
        .await
        .expect("302 原样交回");
    assert_eq!(response.status().as_u16(), 302);
    assert_eq!(
        response
            .headers()
            .get("location")
            .map(|value| value.to_str().unwrap_or_default()),
        Some("file:///etc/passwd")
    );
}

/// 反面对照：**同 scheme 的**重定向照常跟随（闸没有把正常回路过严地拒掉）。
#[tokio::test]
async fn a_same_scheme_redirect_is_followed() {
    let port = spawn_stub(vec![(302, "/ok".to_owned()), (200, "ok".to_owned())]).await;
    let client =
        new_media_http_client(MediaGuard::new().with_policy(loopback_policy())).expect("client");
    let response = client
        .get(format!("http://localhost:{port}/start"))
        .send()
        .await
        .expect("followed");
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.text().await.expect("body"), "ok");
}

/// 自指重定向在跳数预算上被拒（`file://` 那条之外的第二条路）。
#[tokio::test]
async fn too_many_redirects_are_refused_by_the_client() {
    let mut script = Vec::new();
    for _ in 0..MAX_MEDIA_REDIRECTS + 2 {
        script.push((302, "/loop".to_owned()));
    }
    let port = spawn_stub(script).await;
    let client =
        new_media_http_client(MediaGuard::new().with_policy(loopback_policy())).expect("client");
    assert!(
        client
            .get(format!("http://localhost:{port}/loop"))
            .send()
            .await
            .is_err(),
        "自指重定向必须停在跳数预算上"
    );
}
