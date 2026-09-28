# syntax=docker/dockerfile:1
#
# deploy/Dockerfile — M10-7（LUM-2109）的**唯一**发布制品。
#
# 为什么只有这一个文件：`plan1.md` §5 的「发布」原本是 helm chart + 前端镜像 + CLI
# 分发一整套。本仓把它们逐条裁掉了（理由见 docs/64 §9.14：那些制品在本仓既没有
# 分发基础设施、也没有人能验证），**只留一个能被门禁判真假的制品**：`multica-server`
# 的多阶段镜像。判它的门是 `scripts/gates.sh --only image`（**不进默认集合**，
# 理由见该脚本顶部「已知坑」）。
#
# 四条可核对的事实（都来自本仓实测，不是从别处抄的惯例）：
#
# 1. **二进制叫 `multica-server`，不叫 `mc-server`。** crate 名是 `mc-server`
#    （apps/mc-server/Cargo.toml），但 `[[bin]] name = "multica-server"`
#    （同文件）。镜像里的路径必须跟 `[[bin]]` 对齐，否则 ENTRYPOINT 指向不存在的文件。
# 2. **不需要 OpenSSL。** 全部 TLS 走 rustls：sqlx 是 `runtime-tokio-rustls`、
#    reqwest 是 `rustls-tls`、tungstenite 是 `rustls-tls-webpki-roots`
#    （Cargo.toml 实测）。所以 builder 与 runtime 都不装 `libssl-dev`，
#    runtime 也不装 `openssl`。
# 3. **runtime 不需要单独的 `/migrations` 目录。** `sqlx` 带 `migrate` feature，
#    迁移在**编译期**被 `sqlx::migrate!` embed 进二进制 ⇒ 不 COPY 任何迁移目录。
# 4. **builder 直接 `COPY . .`，不用「假 manifest 预热依赖层」那套技巧。** 本仓的
#    workspace 有 37 个 crate，造假 manifest 需要逐个复刻每个 crate 的依赖边，
#    一旦漏一个就在 `cargo metadata` 阶段炸出一句极难读的错。依赖缓存改用 BuildKit
#    的 cache mount（见下），它**不需要**伪造任何文件。
# 5. **`/src/target` 绝不能挂 cache mount。** 第 1 版把编译缓存也挂成
#    `--mount=type=cache,target=/src/target` ⇒ 产物写进 cache 而不是 builder
#    层的文件系统 ⇒ `COPY --from=builder /src/target/release/multica-server` 报
#    `not found`，**而 `cargo build` 那一步是绿的**（门 ⑪ 第 1 次跑就死在这里，
#    由 CI 的 `image` job 抓到）。BuildKit 的 cache mount 只在**同一 builder
#    内部**复用，跨 stage 的 `COPY --from` 看不到它。凡是「要 COPY 出去」的路径，
#    就必须留在真实层里。registry / git 两个 mount 不受影响（没人从它们 COPY）。
#
# 门 ⑧ 的扫描范围（`scripts/file_size_check.py` 的 SCOPE）只含
# `crates/**/*.rs` / `apps/**/*.rs` / `scripts/**/*.{py,sh}` / `.github/workflows/*.yml`
# ⇒ 本文件**不在**门 ⑧ 的范围内（`scripts/gates.sh` 与 `ci.yml` 在）。

# ---- builder ----------------------------------------------------------------
# 与 rust-toolchain.toml 的 `channel = "stable"` 对齐（不在这里钉更旧的版本：
# `rust-version = 1.80` 是 MSRV，钉 stable 才能验证「stable 能编过全仓」）。
FROM rust:1-bookworm AS builder

WORKDIR /src

# 依赖缓存与编译缓存都挂在 cache mount 上：改源码时不会重编 37 个 crate，
# 改依赖时不会重下 registry。`--locked` 与门 ② 的
# `cargo build --workspace --all-targets --locked` 用同一个锁文件。
#
# ⚠️ `COPY . .` **必须在** `cargo fetch` **之前**（本文件第 1 版把 fetch 放在 COPY 之前，
# 那样构建会在 `could not find Cargo.toml` 处失败——因为此时工作目录里一个 manifest 都
# 没有）。依赖层缓存改由 cache mount 提供，不再需要「先 COPY manifest 再 fetch」那套技巧。
COPY . .

# 只编这一个二进制：`--bin multica-server` 是本仓的硬事实（见文件头第 1 条）。
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo fetch --locked && \
    cargo build --release --locked --bin multica-server

# ---- runtime ----------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# ca-certificates 是**必需**的（出站 HTTPS 要验根证书；rustls 不自带根证书）。
# curl 只为让 HEALTHCHECK 有东西可执行；装完即删缓存，装包用 --no-install-recommends。
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl; \
    rm -rf /var/lib/apt/lists/*; \
    useradd --system --uid 10001 --create-home --shell /usr/sbin/nologin multica

COPY --from=builder /src/target/release/multica-server /usr/local/bin/multica-server

USER 10001:10001
WORKDIR /home/multica

# 只把契约写下来，**不做**任何编排：helm / compose / K8s manifest 都在本波
# 「明确不做」清单里（docs/64 §9.14）。
EXPOSE 8080

# 探针**不能**用 `--version`：`apps/mc-server/src/main.rs` 没有这个 flag，进程会
# 直接起服务而不是打印版本。所以探针只判「进程还在、还应答」。路径以 mc-http 的
# 路由表为准（权威来源是 `scripts/route_parity.py`，门 ⑦）—— 这里是给部署者的提示，
# **不**是门禁判据。
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1

ENTRYPOINT ["/usr/local/bin/multica-server"]
