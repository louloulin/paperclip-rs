#!/usr/bin/env bash
#
# scripts/gates.sh — 本仓「每切片必须全绿」门禁的一键执行（plan1 §6.4）。
#
# 这是门禁命令的**唯一实现**：CI（`.github/workflows/ci.yml`）不重写命令，只调用本脚本（`scripts/gates.sh --only <gate>`），
# 因此本地与 CI 跑的是逐字同一批命令，不存在两处漂移。
#
# 十五道门（编号与 docs/plan1.md §5 W0 / §6.4、docs/24-W0-CI.md 的表格一一对应；⑪ / ⑫ / ⑬ / ⑭ / ⑮ 是后续切片追加的，追加号不重编旧号）：
#
#   ① fmt              cargo fmt --all --check
#   ② build            cargo build --workspace --all-targets --locked
#   ③ clippy           cargo clippy --workspace --all-targets -- -D warnings
#   ④ clippy-test-util cargo clippy -p mc-http --all-targets --features mc-http/test-util -- -D warnings
#   ⑤ test             cargo test --workspace                      （**不带** MULTICA_TEST_DATABASE_URL）
#   ⑥ db               mc-migrate run --dir migrations + cargo test -p mc-repos -p mc-http -p mc-scheduler -p mc-server
#                      --features mc-http/test-util -- --ignored
#                      （`mc-scheduler` = M5 内核的租约真库用例；`mc-server` = M5-9 两个生产端口的 真库用例 —— 它们在二进制 crate 里，只有这个包会跑）
#   ⑦ route-parity     python3 scripts/route_parity.py --quiet
#                      + python3 scripts/slash_alias_audit.py --quiet（尾斜杠形态：⑦ 折叠 `/x` 与 `/x/`，
#                        所以「只注册了带斜杠那一形态」它看不见；⑨ 的 fixture 里 0/58 用尾斜杠，同样看不见）
#   ⑧ schema-drift     python3 scripts/schema_drift.py --quiet      （**需要** MULTICA_TEST_DATABASE_URL）
#   ⑨ conformance      cargo run -q -p mc-conformance -- --no-db --check crates/mc-conformance/report.json
#   ⑩ file-size        python3 scripts/file_size_check.py --quiet    （R7 单文件 800 行硬上限）
#   ⑪ image            docker build + 容器内非 root + 二进制存在     （**不在**默认/`--with-db` 集合）
#   ⑫ scripts-tests    python3 scripts/**/test_*.py                （纯标准库 `unittest`，**不需要**任何第三方库；每个文件逐个执行，任一非 0 ⇒ 本门非 0）
#   ⑬ section-alloc    python3 scripts/section_alloc_check.py --quiet （`docs/37` 号段台账的双向校验 + 「撞号」判据；纯标准库、亚秒级，不需要数据库/编译/网络）
#   ⑭ judge-test-cov   python3 scripts/judge_test_coverage_check.py --quiet （`ci.yml` / `gates.sh`
#                      真正执行的每个 `scripts/**/<name>.py` 必须有 `test_<name>.py`（**包入口取包名**：`scripts/<pkg>/__main__.py`/`__init__.py` ⇒ `test_<pkg>.py`，不是 `test___main__.py`；**`python3 -m scripts.foo` 也算引用**；`--dry` 行进读数打 `[DRY]` 但不判红 —— LUM-2629/T1-6-R1，docs/37 §300）；纯标准库、亚秒级）
#   ⑮ realm-diff-tax   python3 -m scripts.t1_6_realm_diff_taxonomy （T1-6 `REALM_DIFF` 族归因分类器：静态面读
#                      `contracts/golden`，不编译/不连库/亚秒级。**判据 = rc==0 且输出非空且小节齐全**
#                      （不是「缺陷数」：它的 `main()` 恒返回 0，是归因报告而不是判红工具 —— LUM-2631/T1-6-R2，docs/37 §302））
#
# 默认跑 ①–⑤ + ⑦ + ⑨ + ⑩ + ⑫ + ⑬ + ⑭ + ⑮（不需要数据库）；`--with-db` 追加 ⑥ 与 ⑧（两者都需要真 PostgreSQL）。
# ⑪ 刻意不在任何默认集合里（见「已知坑」⑪）。
# 每道门打印一行 `GATE_<NAME>_EXIT=<code>`，末尾打印汇总表；任一非 0 → 本脚本 exit 1。
#
# 用法：
#   bash scripts/gates.sh                          # ①–⑤ + ⑦ + ⑨
#   bash scripts/gates.sh --with-db                # ①–⑨（库 URL 见下）
#   bash scripts/gates.sh --with-db --db-url 'postgres://user:pw@127.0.0.1:5432/multica_test'
#   MULTICA_TEST_DATABASE_URL='postgres://…' bash scripts/gates.sh --with-db
#   MULTICA_TEST_THREADS=4 bash scripts/gates.sh --with-db   # 覆盖 ⑥ 的 --test-threads（默认不传 = cargo 的 nproc）
#   bash scripts/gates.sh --only fmt,build         # 只跑选中的门（CI 用这个）
#   bash scripts/gates.sh --list                   # 列出闸门名
#   bash scripts/gates.sh --list-discovered        # 打印门 ⑫ **实际发现到**的测试文件清单
#
# 退出码：0 = 所有被选中的门全绿；1 = 至少一道门非 0；2 = 用法/前置条件错误 （例如选了 ⑥/⑧ 却没给库 URL）。注意 2 不是「门失败」，而是「根本没法开跑」。
#
# 已知坑（本仓实测，详见 docs/24-W0-CI.md §例外 与 docs/30-W0-DRIFT-GATE.md）：
#   * ⑤ 绝不能带 `MULTICA_TEST_DATABASE_URL`：`crates/mc-http` 的 `smoke` 集成测试
#     （target 指向根 `tests/smoke.rs`）拿到库就会对**同一个库重跑迁移** → `relation "user" already exists`。本脚本在 ⑤ 上用 `env -u` 显式剥掉该变量，
#     所以即使调用者已经 export 过它，⑤ 也是安全的。
#   * ⑥ 的连接预算可以用 `MULTICA_TEST_THREADS` 显式压低（见 run_db_gate 与文件顶部说明）。
#     🔴 **但实测证明它压不动**（`docs/37` §198.3，nproc=32 / PG max_connections=100）：
#     ⑥ 单独跑时 `pg_stat_activity` 峰值 **8** 条连接，而 `--test-threads=4` 跑出来**还是 8** ——
#     ⑥ 的连接峰值**不由 `--test-threads` 决定**，所以这个旋钮默认别设（设了只白付约 5% 墙钟：
#     62s → 65s，换来 0 条连接的余量）。
#     🔴 **判别式**：db 门出现 `pool timed out while waiting for an open connection` 时，
#     **先查这台 PG 上已有多少别人的连接，再谈代码**。这个失败有两个极易误判的特征：
#     其一，日志里 `FATAL: sorry, too many clients` 一条都没有 —— sqlx 的 acquire 超时
#     **先于** PG 的拒绝触发，所以「PG 从没抱怨过」并不代表连接够用；
#     其二，panic 落在**基础设施**上而不是业务断言上，于是看起来像「某个用例的缺陷」。
#     实测把连接占到 ~97/100 时 ⑥ 的 migrate 就会以 `pool timed out` 失败而 PG 一条 FATAL 都不打印。
#     只有 panic 指向业务断言才是真缺陷。
#   * ③ 与 ④ 不可合并：只有 ④ 会检查 `crates/mc-http/tests/*` 的 DB e2e 代码。
#   * ⑥ 必须先建表：`mc-repos` / `mc-http` 的 DB 测试直接 INSERT，**自己不做迁移**。
#   * ⑧ 与 ⑥ 的语义分工：⑥ 回答「迁移能跑 + e2e 能过」，⑧ 回答「跑出来的 schema 还是不是上游那份」。
#     ⑧ 用 `--quiet`（判据是退出码），红了才补打完整报告 —— 绿的时候它有 767 行差异明细，
#     塞进 CI 日志只会把真正的信号淹掉。它对着库 URL 建/删自己的 scratch 库 `schema_probe_w0b_drift`，**不读**目标库里的表；但目标库必须存在、该角色要有 CREATEDB 权限，
#     否则脚本 exit 2 → 本脚本记 FAIL（绝不静默跳过）。
#     ✅ 那个 scratch 库名**默认带本进程 PID**（LUM-1463 修）⇒ 同一台 PG 上并发跑两个 ⑧ 各建各的库、不再互踩。
#     ⚠️ 只有显式传同一个 `--db-name` 时才仍会互踩；写死名时代并发两片是 2/2 红（实测见 docs/37 §17）。
#   * ⑩ 只看**跟踪的代码文件**（`git ls-files`，不含 `docs/**`），并把存量违规钉在
#     `scripts/file_size_baseline.tsv` 里：清单外的文件不得超过 800 行，清单内的只允许变短，
#     已达标或已消失的条目必须从清单里删掉。刷新清单用 `--write-baseline`（基线只减不增）。
#     拆分大文件时 **改动会同时打到 mc-http 的热点文件**：拆完先跑 `--only file-size` 确认。
#   * ⑦ 第二条命令（`slash_alias_audit.py`）的欠账钉在 `docs/fixtures/slash-alias-allowlist.tsv`：
#     名单里的键只报不红（都是已知欠账，理由写在行尾），名单外的缺陷（MISSING_ALIAS / MISSING_EXACT）直接判红；**条目对应的键修好后必须删行**，残留的行会被当缺陷（exit 1），
#     否则它会掩盖同一键的下一次回归。查清单外的缺口用 `--no-allowlist`。
#   * ⑪（`image`，M10-7 / LUM-2109）**刻意不进默认集合，也不进 `--with-db` 集合**：
#     它需要本机有 docker（实测本机 `which docker` / `podman` / `buildah` 三者皆无），
#     而把它塞进默认集合会让 `bash scripts/gates.sh` 从 8/8 变 9/9、让几十份文档与 本波 13 个子 issue 的 DoD 全部失效。因此它**只能显式点名跑**
#     （`--only image`），判它的 job 是 CI 的 `image` job（CI 的 runner 有 docker）。
#     ⚠️ **缺 docker 时本门 exit 2（用法/前置条件错），绝不静默跳过、也绝不判绿** —— 与 ⑥/⑧ 的库 URL 同款处置（见文件顶部退出码说明）。想让本门在没有 docker 的机器上
#     「不算失败」，正确做法是**不选它**（`--only` 点名），不是让它自己假装通过。
#   * ⑪ 判的是**三件事**，不是「镜像能不能 build」这一件：① `docker build` 成功；
#     ② 容器内 `id -u` **不是 0**（非 root，`deploy/Dockerfile` 的 `USER 10001:10001`
#     必须真的生效）；③ `/usr/local/bin/multica-server` 存在且可执行（防 ENTRYPOINT 指向
#     不存在的文件 —— 那个二进制名叫 `multica-server`，**不叫 `mc-server`**，见
#     `apps/mc-server/Cargo.toml` 的 `[[bin]] name`）。②③ 各花一次容器启动， 比重新 build 便宜得多，所以放在 build 之后单独判。
#   * ⑫（`scripts-tests`，T1-6-H1 / LUM-2600；覆盖面 LUM-2602 / T1-6-I）执行 `scripts/**/test_*.py`：
#     这些是**纯标准库 `unittest`** 文件，零第三方依赖（本机实测 `python3 -m pytest` →
#     `No module named pytest`；因此门里逐字用 `python3 <file>`，**不引入 pytest**）， 不编译、不连库、不占磁盘，亚秒级，所以它在默认集合里。
#     🔴 **它为什么值得存在**：PR #182 修的正是「docstring 声明的判据，代码从来没执行」， 而本仓在**一个完整 cycle** 里带着两个全绿的 Python 测试文件（20 个用例），
#     而**没有任何门会执行它们** —— 「测试全绿」与「测试被跑过」是两件事。
#     ⇒ 发现规则让「新增文件漏接进门禁」**结构上不可能**：写死文件名的那天 就是这个门开始骗人的那天（新增第三个文件时没人会回来改门禁）。
#     🔴 **覆盖面必须是递归的（LUM-2602 修）**：原规则 `scripts/test_*.py` 只匹配
#     `scripts/` **顶层** ⇒ 包内测试全部漏接。实测 `scripts/t1_6_realm_diff_taxonomy/`
#     （9 个模块 / 1053 行，决定 T1-6 全部缺口的归因）与 `scripts/t1_6_precondition_taxonomy.py`
#     （339 行）在 PR #183 之后仍是「0 测试 + 0 门执行」（`docs/37 §274`）。
#     ⚠️ **文件名形状是有语义的**：包内测试必须叫 `test_*.py` —— `tests.py` **不匹配** `test_*.py`（差一个下划线），写成那样就等于没写。
#     ⚠️ **glob 为空 ⇒ 判红（exit 1），不是「无事发生 ⇒ 绿」**：与 ⑥/⑧ 缺库 URL、
#     ⑪ 缺容器 CLI 同一族处置 —— 让「没东西可跑」在退出码上可区分。
#     🔴 **exit 0 不等于「验证过」**：`python3 <file>` 对一个**没有用例**的文件
#     exit 0 且什么都不打。所以绿的定义是「rc == 0 **且** 出现了 unittest 的 `OK` 行」—— 与本仓反复踩到的「跑过了就算验证过」同一族坑（`docs/37 §272/§274`）。
#     LUM-2604 再补一条**与解释器版本无关**的同族判据：日志里出现 `Ran 0 tests` 就判红 ——
#     实测（Python 3.12）`unittest.main()` 对 0 用例的文件打的是 `NO TESTS RAN` + **rc=5**，
#     也就是说「有没有 OK 行」那一条靠的是解释器的退出码约定；一旦某个版本对 0 用例 exit 0 且打 `OK`，它就当场失效。数用例数不依赖版本。
#     🔴 **判红判的是集合的「身份」，不是「规模」（LUM-2604 修）**：上面两条只看**当次 发现到的集合**，没有任何东西把它钉在基线上 ⇒ 少发现一个文件、改名、把发现规则收窄，
#     全都表现为「集合仍然非空」⇒ **绿**。实测（`docs/37 §276`）把包内那个 56 用例的文件
#     **整个删掉**后，门仍是 `GATE_SCRIPTS_TESTS_EXIT=0` / `3 file(s)` / `PASS` ——
#     即**一个门可以在少跑 56 个用例的情况下报绿**。现在发现集合要与 `scripts/tests.manifest` **逐行一致**：多一个、少一个、改名、清单里重复一行，都判红；
#     集合大小只进 note，**不作判据**（`用例数 >= N` 正是探针 ① 塔到 45 却仍然绿的那类指标）。
#     ⚠️ **守门的检查必须住在集合之外**：任何「检查本门」的用例只能放在顶层 `scripts/test_gate_scripts_tests.py`（非递归 glob 下仍可见，且它在基线清单里），
#     或直接写进**本文件**（`gates.sh` 永不被发现）。把守门用例放进**包内**测试文件 = 发现规则一收窄它自己就不跑 = 无人报警（PR #184 的 `TestGateDiscoveryAgrees`
#     4 条全在包内 ⇒ 探针 ①②③ 全绿，这正是本片要消灭的形态）。
#     ⚠️ 检查门**不得断言本文件的源码文本**：实测 `assertIn("__pycache__", body)` 被本文件里 一行**注释**满足了（LUM-2602）⇒ 那条断言什么也没钉住。所以本门提供
#     `--list-discovered`（唯一实现 = `scripts_tests_discovered_files`），把门**实际算出的 清单**打出来 ⇒ 守门用例断言的是门的**行为**，不是它的散文。
#   * ⑨ 必须显式 `--no-db` 且剥掉库变量：`report.json` 是 **stateless 层**快照，而 mc-conformance 的
#     `--db-url` 带了 `env = "MULTICA_TEST_DATABASE_URL"` —— 谁 export 过这个变量（跑 ⑥/⑧ 的人都会），
#     它就会追加 database 层、把「合并取强者」的报告拿去比 stateless 快照 → 门因为**环境**而红。
#     所以 ⑨ 同时用 `env -u` 与 `--no-db` 两道保险（与 ⑤ 同理）。
#   * ⑬（`section-alloc`，LUM-2607 / T1-6-L）执行 `python3 scripts/section_alloc_check.py --quiet`：
#     校验 `docs/37` 的 `## §NNN` 段号与 `docs/section-alloc.tsv` **双向**一致（R1 文件→台账 /
#     R2 台账→文件）、台账段号唯一（R3 = **撞号**判据）、每个段号在文件里的出现次数等于台账 登记的次数（R4）。纯标准库、亚秒级、不连库不编译，所以它在默认集合里、也在 CI 的 `fast` job 里。
#     🔴 **它为什么值得存在**：`docs/37` 的号段是**手工、先到先得、无校验**的分配 —— 两片都能 合法地拿到同一个空号，撞号只在**合并时**以 `docs/37` 的 CONFLICT 暴露，连续三轮产生真实
#     合并成本（`LUM-2596` 的 PR #168 至今被号段冲突卡住、不可合并）。本门把撞号提前到**提交前**。
#     🔴 **它的判据里含它自己的前置件**：校验脚本被删/被改名 ⇒ 本门判红（`LUM-2604 / §276` 的 教训：判红条件只看被检验的集合时，「删掉检查器」表现为「没有检查报错」⇒ 静默判绿）。台账同理。
#     ⚠️ **新增 `scripts/**/test_*.py` 仍然要求同步改 `scripts/tests.manifest`**（门 ⑫ 的基线）；
#     本门的校验器特意叫 `section_alloc_check.py`（不匹配 `test_*.py`）⇒ **不需要**动那个基线。
#     ⚠️ **台账第 4 列**（`docs/37 出现次数`）是判据 R4 的输入：起手 base 里已有一个**真实撞号**
#     （`## §226` 出现两次：`docs/37:20250` 与 `docs/37:20884`），本片范围明确**不改号**，只能把它
#     **如实登记**为 2；任何**新增**撞号都会让 R3/R4 判红。详见 `docs/37 §279`。

set -u
set -o pipefail

# 本机 /usr/bin/cargo 是 1.75，解析不了本仓的 manifest（edition2024 依赖等）；
# 真实工具链装在 ~/.cargo/bin。显式前置，别依赖调用者的 PATH。
export PATH="$HOME/.cargo/bin:$PATH"
# CI 之外的地方（例如 `sh scripts/gates.sh`）也保持一致的行为。
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.." || exit 2

# 门的规范顺序与显示编号（编号 == plan1 §6.4 的清单序号；⑦ 之后的编号由追加切片顺延，不重编）。
# 排列把两道**需要库**的门（⑥ ⑧）放在一起，离线门 ⑦ ⑨ 收尾；因此汇总表里 ⑧ 会印在 ⑦ 之前。
# `image` 排在最后但**不在**任何默认集合里（默认集合在下方的 SELECTED 分支里逐字写出，
# 不由 ALL_GATES 推导）—— 这样 `--list` / `--only image` 能点到它，而默认跑法碰不到它。
ALL_GATES="fmt build clippy clippy-test-util test db schema-drift route-parity conformance file-size image scripts-tests section-alloc judge-test-coverage realm-diff-taxonomy"

gate_label() {
    case "$1" in
        fmt) echo "①" ;;
        build) echo "②" ;;
        clippy) echo "③" ;;
        clippy-test-util) echo "④" ;;
        test) echo "⑤" ;;
        db) echo "⑥" ;;
        schema-drift) echo "⑧" ;;
        route-parity) echo "⑦" ;;
        conformance) echo "⑨" ;;
        file-size) echo "⑩" ;;
        image)      echo "⑪" ;;
        scripts-tests) echo "⑫" ;;
        section-alloc) echo "⑬" ;;
        judge-test-coverage) echo "⑭" ;;
        realm-diff-taxonomy) echo "⑮" ;;
        *) echo "?" ;;
    esac
}

gate_env_name() {
    case "$1" in
        fmt) echo "FMT" ;;
        build) echo "BUILD" ;;
        clippy) echo "CLIPPY" ;;
        clippy-test-util) echo "CLIPPY_TEST_UTIL" ;;
        test) echo "TEST" ;;
        db) echo "DB" ;;
        schema-drift) echo "SCHEMA_DRIFT" ;;
        route-parity) echo "ROUTE_PARITY" ;;
        conformance) echo "CONFORMANCE" ;;
        file-size) echo "FILE_SIZE" ;;
        image)      echo "IMAGE" ;;
        scripts-tests) echo "SCRIPTS_TESTS" ;;
        section-alloc) echo "SECTION_ALLOC" ;;
        judge-test-coverage) echo "JUDGE_TEST_COVERAGE" ;;
        realm-diff-taxonomy) echo "REALM_DIFF_TAXONOMY" ;;
        *) echo "UNKNOWN" ;;
    esac
}

# ---- 门 ⑫ 的发现规则与基线（LUM-2604）--------------------------------------
#
# 抽成独立函数的理由：`--list-discovered` 与门本体必须走**同一份**规则。若守门用例自己
# 复制一份发现规则，那么「门收窄了但复制品没收窄」就查不出来（两份规则各自绿）。

# 逐行打印参数。**不要**用 `printf '%s\n' "${arr[@]}"`：bash 对空数组会打出一个空行，
# 而下游 `comm` 会把那个空行当成一条真实条目（于是「集合为空」看上去像「有一条记录」）。
gates_print_lines() {
    local _x
    for _x in "$@"; do printf '%s\n' "$_x"; done
}

# 门 ⑫ 的发现规则：`scripts/**/test_*.py`（递归，排除 `__pycache__`）。
scripts_tests_discovered_files() {
    # 用 `find` 而不是 `shopt -s globstar` + `**`：globstar 是 **shell 选项**，
    # 语义依赖调用者的 shell 状态；`find` 在任何 bash 下逐字一致。
    #   `-prune -o`     排除 `__pycache__`（否则上一次的字节码目录会进清单）。
    #   `LC_ALL=C sort` 顺序不随文件系统而变（可复现）。
    find scripts -type d -name '__pycache__' -prune -o \
        -type f -name 'test_*.py' -print | LC_ALL=C sort
}

# 门 ⑫ 的**发现集合基线**：判红判的是集合的**身份**，不是规模。
SCRIPTS_TESTS_MANIFEST="$SCRIPT_DIR/tests.manifest"

# 读基线：忽略空行与 `#` 注释行、去掉行尾空白，并与发现规则同一排序。
# ⚠️ **刻意不去重**：重复的行必须由门报成缺陷，否则「把同一行再抄一遍」就能静默消掉一条 diff。
scripts_tests_manifest_entries() {
    [ -f "$SCRIPTS_TESTS_MANIFEST" ] || return 0
    awk '{ sub(/[[:space:]]+$/, "") }
         /^[[:space:]]*#/ { next }
         /^[[:space:]]*$/ { next }
         { print }' "$SCRIPTS_TESTS_MANIFEST" | LC_ALL=C sort
}

usage() {
    # 打印本文件顶部的注释块（第 3 行起，遇第一行非注释即停）。不要写死行号范围：
    # 每加一道门都要改行号的话，`--help` 迟早会截掉最后几行。
    awk 'NR > 2 { if ($0 !~ /^#/) exit; sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
}

WITH_DB=0
ONLY=""
LIST_DISCOVERED=0
DB_URL="${MULTICA_TEST_DATABASE_URL:-}"

# ⑥ 的 `--test-threads`：让它**可显式配置**，但**默认不传** —— 空 = 沿用 cargo 的 nproc 行为。
# 为什么默认不压低（`docs/37` §198.2/§198.3 实测，nproc=32 / PG max_connections=100 的机器）：
#   * `cargo test -p a -p b -p c -p d` 是**逐个 test binary 串行**跑的，不是 4 个 package 各跑 32 线程；
#   * 每个 db 用例 `Db::connect(&url, 4, 1)` 的池上限 4 只是**上界**，sqlx 懒建、连接用完即还；
#   * 实测 ⑥ 单独跑时 `pg_stat_activity` 峰值只有 **8** 条 —— 远低于 `nproc × 4 = 128` 那个理论上界；
#   * 🔴 承重结论：`--test-threads=4` 跑出来的峰值**仍然是 8**。也就是说 ⑥ 的连接峰值
#     **不由 `--test-threads` 决定**，这个旋钮在当前实现下**压不动连接预算**，
#     却要付约 5% 的墙钟（62s → 65s）⇒ 默认不设是唯一划算的选择。
# 真正的变量是**同一台 PG 上已经被别人占掉多少**（并发跑的其它切片 / 共享实例）：
# 实测占到 ~97/100 时 ⑥ 的 migrate 就会以 `pool timed out` 失败，且 PG 一条 FATAL 都不打印。
# 那种环境下要退让，就换**串行化 db 门**或**加大 max_connections**，而不是拧这个旋钮。
DB_THREAD_FLAG=()
if [ -n "${MULTICA_TEST_THREADS:-}" ]; then
    case "$MULTICA_TEST_THREADS" in
        ''|*[!0-9]*)
            echo "error: MULTICA_TEST_THREADS must be a positive integer (got: '$MULTICA_TEST_THREADS')" >&2
            exit 2 ;;
    esac
    [ "$MULTICA_TEST_THREADS" -ge 1 ] || {
        echo "error: MULTICA_TEST_THREADS must be >= 1 (got: $MULTICA_TEST_THREADS)" >&2
        exit 2
    }
    DB_THREAD_FLAG=(--test-threads="$MULTICA_TEST_THREADS")
fi

while [ $# -gt 0 ]; do
    case "$1" in
        --with-db) WITH_DB=1; shift ;;
        --db-url)
            [ $# -ge 2 ] || { echo "error: --db-url needs a value" >&2; exit 2; }
            DB_URL="$2"; shift 2 ;;
        --db-url=*) DB_URL="${1#*=}"; shift ;;
        --only)
            [ $# -ge 2 ] || { echo "error: --only needs a comma-separated gate list" >&2; exit 2; }
            ONLY="$2"; shift 2 ;;
        --only=*) ONLY="${1#*=}"; shift ;;
        --list)
            printf '%s\n' $ALL_GATES
            exit 0 ;;
        --list-discovered)
            # 门 ⑫ 的**实际**发现清单（唯一实现 = scripts_tests_discovered_files）。
            # 为什么是门的一部分而不是测试里的一个复制品：守门用例必须断言门的**行为**，
            # 而 `assertIn("find scripts", gates.sh 的源码)` 可以被一行**注释**满足
            # （LUM-2602 实测）。把清单打出来 ⇒ 断言的对象是门算出来的结果。
            LIST_DISCOVERED=1; shift ;;
        -h|--help)
            usage
            exit 0 ;;
        *)
            echo "error: unknown argument: $1" >&2
            echo "try: bash scripts/gates.sh --help" >&2
            exit 2 ;;
    esac
done

# `--list-discovered` 是**只读自省**面：打完清单就退，不跑任何门、不写任何东西。
# 它在门本体之前退出 ⇒ 守门用例可以在门里安全地调它（不会递归）。
if [ "$LIST_DISCOVERED" -eq 1 ]; then
    scripts_tests_discovered_files
    exit 0
fi

# ---- 选择要跑的门 ---------------------------------------------------------
if [ -n "$ONLY" ] && [ "$WITH_DB" -eq 1 ]; then
    echo "error: --only and --with-db are mutually exclusive (name 'db' in --only)" >&2
    exit 2
fi

SELECTED=""
if [ -n "$ONLY" ]; then
    for g in $(printf '%s' "$ONLY" | tr ',' ' '); do
        case " $ALL_GATES " in
            *" $g "*) SELECTED="$SELECTED $g" ;;
            *) echo "error: unknown gate: '$g' (known: $ALL_GATES)" >&2; exit 2 ;;
        esac
    done
else
    SELECTED=" fmt build clippy clippy-test-util test"
    [ "$WITH_DB" -eq 1 ] && SELECTED="$SELECTED db schema-drift"
    # ⑦ ⑨ ⑩ ⑫ ⑬ ⑭ ⑮ 都是离线确定性门（⑨ 用 --no-db 跑 stateless 层），因此留在默认集合里。
    SELECTED="$SELECTED route-parity conformance file-size scripts-tests section-alloc judge-test-coverage realm-diff-taxonomy"
fi

selected_gate() {
    case "$SELECTED " in
        *" $1 "*) return 0 ;;
        *) return 1 ;;
    esac
}

# ⑥ / ⑧ 的前置条件：必须有库 URL。缺了就直接报用法错误，而不是让测试静默跳过。
if selected_gate db || selected_gate schema-drift; then
    if [ -z "$DB_URL" ]; then
        echo "error: the 'db' / 'schema-drift' gates need a database URL" >&2
        echo "  pass --db-url 'postgres://user:pw@127.0.0.1:5432/<db>' or set MULTICA_TEST_DATABASE_URL" >&2
        echo "  (both gates create what they need: ⑥ migrates that DB, ⑧ creates and drops its own scratch DB)" >&2
        exit 2
    fi
fi

# ---- 执行 -----------------------------------------------------------------
# 结果表：每项 "gate|exit|secs|note"
RESULTS=""
OVERALL=0
GATE_RAN=0
GATE_PASSED=0
START_ALL="$(date +%s)"

record() { # name exit secs note
    RESULTS="${RESULTS}${1}|${2}|${3}|${4}
"
    GATE_RAN=$((GATE_RAN + 1))
    if [ "$2" -eq 0 ]; then
        GATE_PASSED=$((GATE_PASSED + 1))
    else
        OVERALL=1
    fi
}

# run_gate NAME cmd...  —— 收集式（不 fail-fast），失败只记 FAILED，继续跑后续的门。
run_gate() {
    local name="$1"; shift
    local start end rc
    printf '\n=== [%s] gate %s ===\n' "$(gate_label "$name")" "$name"
    printf '$ %s\n' "$*"
    start="$(date +%s)"
    "$@"
    rc=$?
    end="$(date +%s)"
    printf 'GATE_%s_EXIT=%s\n' "$(gate_env_name "$name")" "$rc"
    record "$name" "$rc" "$((end - start))" ""
    return 0
}

# ⑥ 有前置迁移，单独实现（迁移失败则不再跑 e2e，避免拿「表都建不出来」的库刷屏）。
run_db_gate() {
    local start end rc_m rc_e combined note t0 t1 t2
    printf '\n=== [%s] gate db ===\n' "$(gate_label db)"
    printf '$ MULTICA_DATABASE_URL=<db-url> mc-migrate run --dir migrations\n'
    printf '$ MULTICA_TEST_DATABASE_URL=<db-url> cargo test -p mc-repos -p mc-http -p mc-scheduler -p mc-server --features mc-http/test-util -- --ignored'
    if [ "${#DB_THREAD_FLAG[@]}" -gt 0 ]; then
        printf ' %s\n' "${DB_THREAD_FLAG[*]}"
    else
        # 显式印出「没设」这个事实：预算由 cargo 按 nproc 隐式决定，日志里必须看得见这一点。
        printf '   # --test-threads 未设（实测该旋钮压不动 ⑥ 的连接峰值，见文件顶部「已知坑」）\n'
    fi

    t0="$(date +%s)"
    MULTICA_DATABASE_URL="$DB_URL" cargo run -p mc-migrate -- run --dir migrations
    rc_m=$?
    t1="$(date +%s)"
    printf 'GATE_DB_MIGRATE_EXIT=%s\n' "$rc_m"

    if [ "$rc_m" -eq 0 ]; then
        MULTICA_TEST_DATABASE_URL="$DB_URL" cargo test -p mc-repos -p mc-http -p mc-scheduler -p mc-server \
            --features mc-http/test-util -- --ignored "${DB_THREAD_FLAG[@]+"${DB_THREAD_FLAG[@]}"}"
        rc_e=$?
        t2="$(date +%s)"
        printf 'GATE_DB_E2E_EXIT=%s\n' "$rc_e"
    else
        rc_e="skip"
        t2="$t1"
        printf 'GATE_DB_E2E_EXIT=skip  # not run: migrate failed (exit %s)\n' "$rc_m"
    fi

    if [ "$rc_m" -eq 0 ] && [ "$rc_e" -eq 0 ]; then
        combined=0
    else
        combined=1
    fi
    printf 'GATE_DB_EXIT=%s\n' "$combined"
    note="migrate=$rc_m,e2e=$rc_e"
    record db "$combined" "$((t2 - t0))" "$note"
    return 0
}

# ⑧ 自带 scratch 库（默认 `schema_probe_w0b_drift`），不下 ⑥ 迁移出来的那个库，所以两个门用同一个库 URL 是安全的。平时
# `--quiet`（判据是退出码）；红了才再跑一遍把完整报告打出来 —— 绿的时候那份报告有 700+ 行差异明细。
run_schema_drift_gate() {
    local start end rc
    printf '\n=== [%s] gate schema-drift ===\n' "$(gate_label schema-drift)"
    printf '$ MULTICA_TEST_DATABASE_URL=<db-url> python3 scripts/schema_drift.py --quiet\n'

    start="$(date +%s)"
    MULTICA_TEST_DATABASE_URL="$DB_URL" python3 scripts/schema_drift.py --quiet
    rc=$?

    if [ "$rc" -ne 0 ]; then
        printf '-- schema-drift is red (exit %s); full report follows --\n' "$rc"
        MULTICA_TEST_DATABASE_URL="$DB_URL" python3 scripts/schema_drift.py || true
        printf -- '-- end of schema-drift report --\n'
    fi

    end="$(date +%s)"
    printf 'GATE_SCHEMA_DRIFT_EXIT=%s\n' "$rc"
    record schema-drift "$rc" "$((end - start))" ""
    return 0
}

# ⑪ image（M10-7 / LUM-2109）—— 判 deploy/Dockerfile 这**唯一**的发布制品。
#
# 为什么单独写一个函数而不是 run_gate 一行：这条门要判三件事、且中间要复用同一个
# 镜像 tag（build 一次、起两次容器），run_gate 的「name + 一条命令」形状装不下。
#
# 三条判据（见文件顶部「已知坑」⑪）：build 成功 / 容器内非 root / 二进制存在且可执行。② ③ 刻意不合并成
# 一次容器启动：合并就少了一个可读的失败点，而分开时两次 `docker run --rm --entrypoint` 各自只花几百毫秒，
# 相对 build 的分钟级开销可以忽略。
run_scripts_tests_gate() {
    # ⑫ scripts-tests（LUM-2600 / T1-6-H1；覆盖面 LUM-2602 / T1-6-I）—— 执行 `scripts/**/test_*.py`。
    #
    # 为什么单独写函数而不是 run_gate 一行：判据是「**每一个** 测试文件都真跑了且都绿」，
    # 文件数在运行时才确定（发现规则），而 run_gate 的形状是「name + 一条固定命令」。
    #
    # 为什么不逐个把文件名写死：写死的那天就是这个门开始骗人的那天 —— 新增第三个测试文件时没人会记得回来改这里，
    # 于是又变成「全绿但没人看得见」。⇒ 发现规则让「新增测试文件漏接进门禁」**结构上不可能**（见文件顶部「已知坑」⑫）。
    #
    # 🔴 但「发现规则」本身也是**被检验的对象**（LUM-2604）：所以发现集合还要与 `scripts/tests.manifest` 逐行比对，判的是集合的**身份**；
    # 而且检查这件事的用例住在顶层 `scripts/test_gate_scripts_tests.py`（发现规则收窄时它仍会被执行）。
    local start end rc combined f files=() note log no_tests
    local -a baseline=() only_tree=() only_manifest=() dup_entries=()
    local identity_red=0 _p
    printf '\n=== [⑫] gate scripts-tests ===\n'

    # 🔴 **递归**发现（LUM-2602）。规则现在只有一处实现（`scripts_tests_discovered_files`），
    # 因为 `--list-discovered` 必须打出**门自己**算出来的清单 —— 守门用例断言的是这个清单，
    # 不是本文件的源码文本（源码子串可以被**注释**满足，实测见文件顶部「已知坑」⑫）。
    # 原来的 `files=(scripts/test_*.py)` 只匹配 `scripts/` **顶层** ⇒ 包内测试
    # （如 `scripts/t1_6_realm_diff_taxonomy/test_*.py`）全部漏接，而那 1053 行正是
    # 「决定每条 T1-6 缺口归谁」的那套分类器（`docs/37 §274`）。
    mapfile -t files < <(scripts_tests_discovered_files)

    # 🔴 判据一：发现集合的**身份**必须与基线逐行一致（LUM-2604）。下面那条「集合非空」与「0 用例」都只看**当次**
    # 发现到的集合，因此**不够**：丢文件 / 改名 / 收窄发现规则都表现为「集合仍然非空」⇒ 绿。实测（`docs/37 §276`）
    # 删掉 56 个用例那个文件后，旧判据仍是 `exit=0 / 3 file(s) / PASS`。
    if [ ! -f "$SCRIPTS_TESTS_MANIFEST" ]; then
        identity_red=1
        printf 'error: the scripts-tests gate needs its discovery baseline, but it is missing\n' >&2
        printf '  expected: scripts/tests.manifest\n' >&2
        printf '  🔴 this gate pins the IDENTITY of the discovered set, NOT its SIZE.\n' >&2
        printf '     Without the baseline the gate cannot tell "4 files, all of them" from\n' >&2
        printf '     "3 files, one of them silently gone" (LUM-2604 / docs/37 §276).\n' >&2
    else
        mapfile -t baseline < <(scripts_tests_manifest_entries)
        mapfile -t only_tree < <(
            comm -23 <(gates_print_lines "${files[@]}") <(gates_print_lines "${baseline[@]}")
        )
        mapfile -t only_manifest < <(
            comm -13 <(gates_print_lines "${files[@]}") <(gates_print_lines "${baseline[@]}")
        )
        mapfile -t dup_entries < <(gates_print_lines "${baseline[@]}" | uniq -d)
        if [ "${#only_tree[@]}" -ne 0 ] || [ "${#only_manifest[@]}" -ne 0 ] || [ "${#dup_entries[@]}" -ne 0 ]; then
            identity_red=1
            printf 'error: the scripts-tests gate discovered a set whose IDENTITY differs from its baseline\n' >&2
            printf '  🔴 this gate pins the IDENTITY of the discovered set, NOT its SIZE:\n' >&2
            printf '     a deleted file, a renamed file, or a narrowed discovery rule all leave the\n' >&2
            printf '     set "non-empty" and used to read as green (LUM-2602 / docs/37 §276).\n' >&2
            for _p in "${only_tree[@]}"; do
                printf '   + %s   [discovered, NOT in scripts/tests.manifest]\n' "$_p" >&2
            done
            for _p in "${only_manifest[@]}"; do
                printf '   - %s   [in scripts/tests.manifest, NOT discovered]\n' "$_p" >&2
            done
            for _p in "${dup_entries[@]}"; do
                printf '   = %s   [listed more than once in scripts/tests.manifest]\n' "$_p" >&2
            done
            printf '  fix: intended new test file => add its line to scripts/tests.manifest;\n' >&2
            printf '       intended deletion/rename => delete/replace that line. Both in ONE commit.\n' >&2
        fi
    fi

    # 一个测试文件都没有 ⇒ **判红**，不是「无事发生 ⇒ 绿」。
    # 理由与 ⑥/⑧ 缺库 URL、⑪ 缺容器 CLI 同一族：让「没东西可跑」在退出码上可区分。
    # （这一条**保留** —— 它对；但上面「集合身份」那条才是本片新增的判据，两者都不足够。）
    if [ "${#files[@]}" -eq 0 ]; then
        printf 'error: the scripts-tests gate found no scripts/**/test_*.py to run\n' >&2
        printf '  (an empty glob must never read as green: that is how 20 green cases\n' >&2
        printf '   sat in this repo for a whole cycle with nothing executing them)\n' >&2
        printf 'GATE_SCRIPTS_TESTS_EXIT=1\n'
        record scripts-tests 1 0 "no scripts/**/test_*.py found"
        return 0
    fi

    start="$(date +%s)"
    # 身份不匹配时**仍然把发现到的文件都跑一遍**：本门的判据是**行为读数**，早退就看不到
    # 「哪些文件真的被跑了」—— 而「发现规则收窄时守门用例仍被**执行**」正是本片的一条硬验收
    # （LUM-2604 §四.2）。所以身份判红只置 combined，**不** return。
    combined=$identity_red
    no_tests=0
    log="$(mktemp)"
    # 逐个文件都跑（不 fail-fast）：一个文件炸了不许掩盖另一个文件的读数。
    for f in "${files[@]}"; do
        printf '$ python3 %s\n' "$f"
        # 输出重定向到文件再打：`python3 ... | tee` 遇上 SIGPIPE 会把本轮日志截断。
        python3 "$f" >"$log" 2>&1
        rc=$?
        cat "$log"
        # 🔴 **exit 0 不等于「验证过」**（LUM-2602）。`python3 <file>` 对一个没有用例的
        # 文件 **exit 0** 且什么都不打（若它调了 `unittest.main()` 则打 `Ran 0 tests`）。
        # ⇒ 绿的定义收紧为「rc == 0 **且** unittest 打了 `OK` 行」。
        if [ "$rc" -eq 0 ] && ! grep -Eq '^OK' "$log"; then
            printf '  !! %s exit=0 但没有 unittest 的 OK 行 ⇒ 判红（0 个用例 = 没验证过）\n' "$f"
            combined=1
            no_tests=$((no_tests + 1))
        elif grep -Eq '^Ran 0 tests' "$log"; then
            # 🔴 与**解释器版本无关**的 0 用例判据（LUM-2604）。上面那条「有没有 OK 行」
            # 在本机（Python 3.12）对 `unittest.main()` 的 0 用例文件是靠 **rc=5** 才红的
            # （那次的输出是 `NO TESTS RAN`，**不是** `OK`）；而 unittest 的退出码/输出
            # **随版本变** —— 一旦某个版本对 0 用例 exit 0 且打 `OK`，上面那条当场失效。
            # 数用例数就与版本无关，而且判词直接指向「0 个用例」这件事本身。
            printf '  !! %s 报 `Ran 0 tests` ⇒ 判红（0 个用例 = 没验证过）\n' "$f"
            combined=1
            no_tests=$((no_tests + 1))
        fi
        printf '  -> %s exit=%s\n' "$f" "$rc"
        [ "$rc" -ne 0 ] && combined=1
    done
    rm -f "$log"
    end="$(date +%s)"

    note="${#files[@]} file(s)"
    [ "$identity_red" -ne 0 ] && note="${note}, discovery-identity mismatch"
    [ "$combined" -ne 0 ] && note="${note}, at least one red"
    [ "$no_tests" -ne 0 ] && note="${note}, ${no_tests} with zero test cases"
    printf 'GATE_SCRIPTS_TESTS_EXIT=%s\n' "$combined"
    record scripts-tests "$combined" "$((end - start))" "$note"
    return 0
}

# ⑬ section-alloc（LUM-2607 / T1-6-L）—— `docs/37` 号段台账的**双向校验 + 撞号判据**。
#
# 为什么单独写函数而不是 `run_gate section-alloc python3 …` 一行：本门的**前置件也是判据**。
# `run_gate` 在「校验脚本被删/被改名」时也会拿到非 0（python3 打不开文件 ⇒ 2），但那个读数看不出病因；
# 而本片要钉的形态（`LUM-2604 / §276`）正是「判红条件只看被检验的集合 ⇒ 丢掉检查器表现为绿」。所以这里把
# 「校验器存在」显式写成判据之一（与 ⑥/⑧ 缺库 URL、⑪ 缺容器 CLI、⑫ 空 glob 同族：**让「没东西可跑」在退出码上可区分**）。
# 台账缺失 / 空台账 / 双向不一致 / 撞号 / 出现次数不符 —— 全部由脚本自身的退出码给出。
SECTION_ALLOC_CHECKER="scripts/section_alloc_check.py"
run_section_alloc_gate() {
    local start end rc
    printf '\n=== [%s] gate section-alloc ===\n' "$(gate_label section-alloc)"
    printf '$ python3 %s --quiet\n' "$SECTION_ALLOC_CHECKER"
    start="$(date +%s)"

    # 🔴 判据一：校验器必须在。删掉 / 改名它 ⇒ 红，而不是「无事发生 ⇒ 绿」。
    if [ ! -f "$SECTION_ALLOC_CHECKER" ]; then
        end="$(date +%s)"
        printf 'error: the section-alloc gate needs its checker, but it is missing\n' >&2
        printf '  expected: %s\n' "$SECTION_ALLOC_CHECKER" >&2
        printf '  🔴 校验器不在就必须判红：否则「删掉检查」表现为「没有检查报错」⇒ 静默判绿（§276）\n' >&2
        printf 'GATE_SECTION_ALLOC_EXIT=1\n'
        record section-alloc 1 "$((end - start))" "checker missing"
        return 0
    fi

    python3 "$SECTION_ALLOC_CHECKER" --quiet
    rc=$?
    # 红了才再跑一遍把逐条缺陷（R1/R2/R3/R4 各是哪个段号）打出来 —— 与 ⑧ 同款：
    # `--quiet` 是判据面（只看退出码），明细是诊断面，绿的时候它们只是噪音。
    if [ "$rc" -ne 0 ]; then
        printf '$ python3 %s   # 红了重跑一遍，把逐条缺陷打出来（与 ⑧ 同款）\n' "$SECTION_ALLOC_CHECKER"
        python3 "$SECTION_ALLOC_CHECKER" || true
    fi
    end="$(date +%s)"
    printf 'GATE_SECTION_ALLOC_EXIT=%s\n' "$rc"
    record section-alloc "$rc" "$((end - start))" ""
    return 0
}

# ⑭ judge-test-coverage（LUM-2626 / T1-6-G1；判词形状 LUM-2629 / T1-6-R1，docs/37 §300）—— `ci.yml` / `gates.sh` 真正执行的每个 `scripts/**/<name>.py` 都必须有 `scripts/**/test_<name>.py`；§300 修了两条形状规则（包入口取**包名**而非 `__main__` = 假红；`-m` 模块形态**必须**进读数 = 旧版假绿）。
#
# 同样写成独立函数而不是 `run_gate … python3 … --quiet` 一行：门本体必须能**逐条点名**缺口（哪个引用面、
# 哪个脚本、缺哪个 `test_`），而 `--quiet` 面只打一行总结（判词面）。与 ⑧ / ⑬ 同款：红了才再跑一遍不带
# `--quiet`，把明细打出来。
JUDGE_TEST_COVERAGE_CHECKER="scripts/judge_test_coverage_check.py"
run_judge_test_coverage_gate() {
    local start end rc
    printf '\n=== [%s] gate judge-test-coverage ===\n' "$(gate_label judge-test-coverage)"
    printf '$ python3 %s --quiet\n' "$JUDGE_TEST_COVERAGE_CHECKER"
    start="$(date +%s)"

    # 🔴 判据一：判定器必须在。删掉 / 改名它 ⇒ 红，而不是「无事发生 ⇒ 绿」（§276）。
    if [ ! -f "$JUDGE_TEST_COVERAGE_CHECKER" ]; then
        end="$(date +%s)"
        printf 'error: the judge-test-coverage gate needs its checker, but it is missing\n' >&2
        printf '  expected: %s\n' "$JUDGE_TEST_COVERAGE_CHECKER" >&2
        printf 'GATE_JUDGE_TEST_COVERAGE_EXIT=1\n'
        record judge-test-coverage 1 "$((end - start))" "checker missing"
        return 0
    fi

    python3 "$JUDGE_TEST_COVERAGE_CHECKER" --quiet
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf '$ python3 %s   # 红了重跑一遍，把逐条缺口打出来\n' "$JUDGE_TEST_COVERAGE_CHECKER"
        python3 "$JUDGE_TEST_COVERAGE_CHECKER" || true
    fi
    end="$(date +%s)"
    printf 'GATE_JUDGE_TEST_COVERAGE_EXIT=%s\n' "$rc"
    record judge-test-coverage "$rc" "$((end - start))" ""
    return 0
}

# ⑮ realm-diff-taxonomy（LUM-2631 / T1-6-R2，`docs/37 §302`）—— T1-6 `REALM_DIFF` 族的归因
# 分类器。🔴 判据**不是**「缺陷数」：那个分类器的 `main()` 恒返回 0（它是归因报告，不是判红工具）⇒
# 光看 rc 只能逮住 traceback，逮不住「少打了一节结论」。所以判三件事：跑得起来、输出非空、**每个小节
# 标题都在**（结论面，不是装饰）。⚠️ 不用 `--dry` 绕过（那个 flag 门 ⑭ 记成 `[DRY]` 试跑、不要求有测试
# ⇒ 等于「接了个不接的门」）。⚠️ 下面是**字面量**命令而不是 `python3 -m "$VAR"`：门 ⑭ 只从字面量收
# 引用面（它的「已知边界 1」），走变量会整条漏掉 ⇒ 实测 `judges` 会停在 6。「包入口被删 ⇒ 判红」不需要
# 单独一条判据：`python3 -m` 找不到包/入口本身就是 rc=1。
run_realm_diff_taxonomy_gate() {
    local start end rc out missing=0
    printf '\n=== [%s] gate realm-diff-taxonomy ===\n' "$(gate_label realm-diff-taxonomy)"
    printf '$ python3 -m scripts.t1_6_realm_diff_taxonomy\n'
    start="$(date +%s)"; out="$(mktemp)"
    python3 -m scripts.t1_6_realm_diff_taxonomy >"$out" 2>&1; rc=$?
    for section in "并行结论：" "by-design：" "判别式双向验证" "静态面判不了、必须看 db 读数的子族" "负责面（文件集合）"; do
        if ! grep -qF "$section" "$out"; then
            printf 'error: the classifier output is missing the section: %s\n' "$section" >&2; missing=1
        fi
    done
    if [ ! -s "$out" ]; then printf 'error: the classifier printed nothing\n' >&2; missing=1; fi
    [ "$missing" -eq 0 ] || rc=1
    if [ "$rc" -ne 0 ]; then cat "$out"; fi
    rm -f "$out"; end="$(date +%s)"
    printf 'GATE_REALM_DIFF_TAXONOMY_EXIT=%s\n' "$rc"
    record realm-diff-taxonomy "$rc" "$((end - start))" ""
    return 0
}

run_image_gate() {
    local start end rc tag
    tag="multica-server:image"
    printf '\n=== [⑪] gate image ===\n'
    printf '$ docker build -f deploy/Dockerfile -t %s .\n' "$tag"

    # 缺 docker ⇒ 用法/前置条件错（exit 2），**不是**门失败、也**不是**跳过。
    # 与 ⑥/⑧ 缺库 URL 同款：让「没法跑」和「跑了但红」在退出码上可区分。
    if ! command -v "${IMAGE_DOCKER:-docker}" >/dev/null 2>&1; then
        printf 'error: the image gate needs a container CLI; none found (%s)\n' "${IMAGE_DOCKER:-docker}" >&2
        printf '  the image gate is deliberately NOT in the default set: it needs docker,\n' >&2
        printf '  which this host does not have (measured: docker/podman/buildah all absent).\n' >&2
        printf '  run it where a container CLI exists (the CI "image" job), or do not select it.\n' >&2
        printf '  set IMAGE_DOCKER=<cli> to point at a non-default one.\n' >&2
        return 2
    fi

    start="$(date +%s)"
    if ! "${IMAGE_DOCKER:-docker}" build -f deploy/Dockerfile -t "$tag" . ; then
        end="$(date +%s)"
        printf 'GATE_IMAGE_EXIT=1\n'
        record image 1 "$((end - start))" "build failed"
        return 0
    fi

    # ② 非 root：镜像里 `USER 10001:10001` 必须真的生效（写成 root 也能 build 成功，
    #    所以这一条只能靠**起容器问它**来判）。
    local uid
    uid="$("${IMAGE_DOCKER:-docker}" run --rm --entrypoint id "$tag" -u 2>/dev/null || true)"
    if [ -z "$uid" ] || [ "$uid" = "0" ]; then
        end="$(date +%s)"
        printf 'error: image runs as uid %s (expected non-zero) — deploy/Dockerfile USER is not effective\n' "${uid:-<none>}" >&2
        printf 'GATE_IMAGE_EXIT=1\n'
        record image 1 "$((end - start))" "runs as root"
        return 0
    fi

    # ③ 二进制存在且可执行：二进制名是 `multica-server`（apps/mc-server/Cargo.toml 的
    #    `[[bin]] name`），**不是** crate 名 `mc-server`。这一条防的正是「ENTRYPOINT 指到
    #    一个不存在的路径、而 build 仍然成功」这种最难在运行时才暴露的错误。
    if ! "${IMAGE_DOCKER:-docker}" run --rm --entrypoint test "$tag" -x /usr/local/bin/multica-server ; then
        end="$(date +%s)"
        printf 'error: /usr/local/bin/multica-server missing or not executable in the image\n' >&2
        printf 'GATE_IMAGE_EXIT=1\n'
        record image 1 "$((end - start))" "binary missing"
        return 0
    fi

    end="$(date +%s)"
    printf 'image OK: uid=%s, /usr/local/bin/multica-server is executable\n' "$uid"
    rc=0
    printf 'GATE_IMAGE_EXIT=0\n'
    record image "$rc" "$((end - start))" "uid=$uid"
    return 0
}

for gate in $SELECTED; do
    case "$gate" in
        fmt)            run_gate fmt            cargo fmt --all --check ;;
        build)          run_gate build          cargo build --workspace --all-targets --locked ;;
        clippy)         run_gate clippy         cargo clippy --workspace --all-targets -- -D warnings ;;
        clippy-test-util) run_gate clippy-test-util \
                            cargo clippy -p mc-http --all-targets --features mc-http/test-util -- -D warnings ;;
        # ⑤ 显式剥掉 DB 变量：见文件顶部「已知坑」。
        test)           run_gate test env -u MULTICA_TEST_DATABASE_URL -u MULTICA_DATABASE_URL \
                            cargo test --workspace ;;
        # ⑥ 的 --test-threads 来自 $MULTICA_TEST_THREADS（默认不传），见文件顶部「已知坑」。
        db)             run_db_gate ;;
        schema-drift)   run_schema_drift_gate ;;
        route-parity)   run_gate route-parity bash -c \
                            'python3 scripts/route_parity.py --quiet && python3 scripts/slash_alias_audit.py --quiet' ;;
        # ⑨ 显式 --no-db + 剥掉库变量：见文件顶部「已知坑」。判据是 --check 的退出码
        # （report.json 逐字节比对，漂移 → 1，见 crates/mc-conformance/src/main.rs）。
        conformance)    run_gate conformance env -u MULTICA_TEST_DATABASE_URL -u MULTICA_DATABASE_URL \
                            cargo run -q -p mc-conformance -- --no-db \
                            --check crates/mc-conformance/report.json ;;
        # ⑩ 纯离线、秒级；判据是 scripts/file_size_check.py 的退出码（越限 → 1）。
        file-size)      run_gate file-size python3 scripts/file_size_check.py --quiet ;;
        # ⑪ 不在默认/`--with-db` 集合里（见文件顶部「已知坑」⑪）；只能用 `--only image` 点名。
        # 前置条件缺失（无容器 CLI）必须以 **exit 2** 终止整个脚本，而不是记成「没跑过这道门」
        # 后照样 exit 0 —— 那正是「静默判绿」，是本仓明令禁止的（与 ⑥/⑧ 缺库 URL 同款）。
        image)          run_image_gate; _img_rc=$?; [ "$_img_rc" -eq 2 ] && exit 2 ;;
        scripts-tests)  run_scripts_tests_gate ;;
        section-alloc)  run_section_alloc_gate ;;
        judge-test-coverage) run_judge_test_coverage_gate ;;
        realm-diff-taxonomy) run_realm_diff_taxonomy_gate ;;
        *)              echo "error: unhandled gate '$gate'" >&2; exit 2 ;;
    esac
done

# ---- 汇总表 ---------------------------------------------------------------
END_ALL="$(date +%s)"

printf '\n============================= GATE SUMMARY =============================\n'
printf '  #  %-17s %5s %6s  %s\n' "gate" "exit" "time" "result"
printf '%s\n' "$RESULTS" | while IFS='|' read -r name rc secs note; do
    [ -n "$name" ] || continue
    if [ "$rc" = "0" ]; then
        result="PASS"
    else
        result="FAIL"
    fi
    if [ -n "$note" ]; then
        printf '  %s  %-17s %5s %5ss  %s  (%s)\n' \
            "$(gate_label "$name")" "$name" "$rc" "$secs" "$result" "$note"
    else
        printf '  %s  %-17s %5s %5ss  %s\n' \
            "$(gate_label "$name")" "$name" "$rc" "$secs" "$result"
    fi
done

# 被跳过的门（本表只列实际跑过的）
NOT_SELECTED=""
for gate in $ALL_GATES; do
    if ! selected_gate "$gate"; then
        NOT_SELECTED="$NOT_SELECTED $gate"
    fi
done
if [ -n "$NOT_SELECTED" ]; then
    printf '  (not selected:%s)\n' "$NOT_SELECTED"
fi

printf '%s\n' "-----------------------------------------------------------------------"
if [ "$OVERALL" -eq 0 ]; then
    printf '  overall: PASS — %s/%s gate(s) green in %ss\n' "$GATE_PASSED" "$GATE_RAN" "$((END_ALL - START_ALL))"
else
    printf '  overall: FAIL — %s/%s gate(s) green in %ss (rerun the red gate(s) with --only)\n' \
        "$GATE_PASSED" "$GATE_RAN" "$((END_ALL - START_ALL))"
fi
printf '%s\n' "======================================================================="

exit "$OVERALL"
