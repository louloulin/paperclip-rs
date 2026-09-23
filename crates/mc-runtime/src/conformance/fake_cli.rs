//! 套件用的**假 CLI**：现场生成一个 `#!/bin/sh` 脚本（回放一段事件流、或睡死、或非零
//! 退出），并在析构时删掉自己的临时目录。
//!
//! 从 `conformance/mod.rs` 拆出来是因为假 CLI 是自包含的一块（脚本骨架 + argv/stdin
//! 落盘 + `read_gate` 长连接闸门），而套件本体是另一块（断言）；两者一起长会顶到门 ⑩
//! 的 800 行上限。对外的路径不变（`mc_runtime::conformance::FakeCli` 由 mod 重导出）。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// 现场生成的假 CLI（`#!/bin/sh` 脚本）。析构时删掉整个临时目录。
pub struct FakeCli {
    dir: PathBuf,
    executable: PathBuf,
}

impl FakeCli {
    fn allocate(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "mc-runtime-conformance-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        Self {
            executable: dir.join(format!("{tag}.sh")),
            dir,
        }
    }

    /// 假 CLI 的绝对路径（交给 adapter 当 executable）。
    pub fn path(&self) -> PathBuf {
        self.executable.clone()
    }

    /// 工作目录（adapter 的落盘目录都在这里，析构时一起删）。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 脚本捕获到的 stdin。
    pub fn recorded_stdin(&self) -> String {
        std::fs::read_to_string(self.dir.join("stdin.txt")).unwrap_or_default()
    }

    /// 脚本捕获到的 argv（一行一个）。
    pub fn recorded_argv(&self) -> String {
        std::fs::read_to_string(self.dir.join("argv.txt")).unwrap_or_default()
    }

    /// 只打印一行版本号的假 CLI。
    pub fn version(stdout: &str) -> Self {
        let fake = Self::allocate("version");
        let payload = fake.payload("version.txt", stdout);
        fake.install(&format!("#!/bin/sh\ncat {}\n", quoted(&payload)));
        fake
    }

    /// 回放一段事件流、以 0 退出的假 CLI（同时捕获 stdin/argv）。
    pub fn replaying(transcript: &str) -> Self {
        let fake = Self::allocate("replay");
        let payload = fake.payload("transcript.txt", transcript);
        fake.install(&fake.script(&format!(
            "while IFS= read -r line; do\n  printf '%s\\n' \"$line\"\ndone < {}\nexit 0\n",
            quoted(&payload)
        )));
        fake
    }

    /// 回放一段事件流、写 stderr、以 `exit_code` 退出的假 CLI。
    pub fn failing(transcript: &str, exit_code: i32, stderr: &str) -> Self {
        let fake = Self::allocate("failing");
        let payload = fake.payload("transcript.txt", transcript);
        let error = fake.payload("stderr.txt", &format!("{stderr}\n"));
        fake.install(&fake.script(&format!(
            "while IFS= read -r line; do\n  printf '%s\\n' \"$line\"\ndone < {transcript}\ncat {error} >&2\nexit {exit_code}\n",
            transcript = quoted(&payload),
            error = quoted(&error),
        )));
        fake
    }

    /// 什么都不输出、睡死（用 `exec` 保证杀的就是 sleep 本体，不留孤儿）的假 CLI。
    pub fn sleeping(seconds: u32) -> Self {
        let fake = Self::allocate("sleeping");
        fake.install(&fake.script(&format!("exec sleep {seconds}\n")));
        fake
    }

    /// 先回放一段事件流、再睡死（用于"流已经起来了再取消"）。
    ///
    /// 回放必须是**一次写**（`cat`），不能逐行 `printf` 循环：取消用例在读到
    /// 首个正文事件后**立刻** kill 进程，逐行版会留下一个负载相关的窗口 ——
    /// 终态行还没落进管道就被杀，`opencode`/`codearts` 这类 fail-closed 解码器
    /// 会把"被我们自己掐断的流"判成 `Failed`（这是 `docs/33` §5 规定的**正确**
    /// 行为，不是 bug）⇒ 用例变成掷骰子。
    /// 一次写保证数据在首个事件可读时**已经**整段进了管道，`run` 的 `BufReader`
    /// 会把它入库，kill 之后的 drain 仍能读完（`docs/37` §18.6 有实测数据）。
    pub fn replaying_then_sleeping(transcript: &str, seconds: u32) -> Self {
        let fake = Self::allocate("replay-sleep");
        let payload = fake.payload("transcript.txt", transcript);
        fake.install(&fake.script(&format!("cat {}\nexec sleep {seconds}\n", quoted(&payload))));
        fake
    }

    /// **长连接 stdin** 假 CLI（JSON-RPC 类协议专用）的脚本骨架。
    ///
    /// 普通前台的 `cat > stdin.txt` 要读到 EOF 才往下走，而 JSON-RPC 的 stdin 是
    /// 长连接（写完 `initialize` 还在等应答）⇒ 双方互等，run 挂到超时。
    /// 这里改用前台 `read_gate <子串>`：逐行读 stdin、逐行落盘，读到含该子串的帧
    /// 才返回。闸门看的是**内容**而不是时序 ⇒ 确定性，不用 sleep 抢。
    ///
    /// 不改成后台 `cat`：dash 会把**异步列表**（`&`）的 stdin 接成 `/dev/null`，
    /// 后台进程读到的永远是空（实测 stdin.txt 恒空），`<&3` 也只是在某些 redirect
    /// 组合下才生效 —— 干脆不用后台。
    fn live_script(&self, gate: &str, replay: Option<&Path>, tail: &str) -> String {
        let mut body = self.live_preamble();
        let _ = writeln!(body, "read_gate {} || exit 0", quote_word(gate));
        if let Some(transcript) = replay {
            // 同样是一次写（理由见 `replaying_then_sleeping`）：`cat <文件>` 不读
            // stdin，因此不会吃掉 adapter 的帧。
            body.push_str("cat ");
            body.push_str(&quoted(transcript));
            body.push('\n');
        }
        body.push_str(tail);
        body
    }

    /// [`FakeCli::live_script`] 的公共前缀（argv/stdin 落盘 + `read_gate` 定义）。
    fn live_preamble(&self) -> String {
        let dir = quoted(&self.dir);
        format!(
            "#!/bin/sh\nd={dir}\nprintf '%s\\n' \"$@\" > \"$d/argv.txt\"\nexec 3<&0\n: > \"$d/stdin.txt\"\nread_gate() {{\n  while IFS= read -r line <&3; do\n    printf '%s\\n' \"$line\" >> \"$d/stdin.txt\"\n    case \"$line\" in *\"$1\"*) return 0;; esac\n  done\n  return 1\n}}\n"
        )
    }

    /// 逐帧回放脚本的正文（argv/stdin 落盘 + `read_gate` 定义）：每读到含
    /// `frames[i].0` 的客户端帧就 `cat` 出 `frames[i].1`，全部走完再执行 `tail`。
    fn scripted_body(&self, frames: &[(String, String)], tail: &str) -> String {
        let mut body = self.live_preamble();
        for (index, (gate, payload)) in frames.iter().enumerate() {
            let path = self.payload(&format!("frame-{index}.txt"), payload);
            let _ = writeln!(
                body,
                "read_gate {} || exit 0\ncat {}",
                quote_word(gate),
                quoted(&path)
            );
        }
        body.push_str(tail);
        body
    }

    /// **逐帧**回放的假 CLI（固定帧 id 的请求/应答协议专用）。
    ///
    /// 与 [`FakeCli::live_replaying`] 的唯一区别是**回放的时机**：后者把整段挂在
    /// 第一道门上，只适合"应答与前面的应答无关"的协议（AppServer 一次
    /// `thread/start` 就拿到了 `threadId`；一次性回放对它是安全的）。ACP 的应答带
    /// **相位**（`acp_core::client` 只认固定 id 的当前阶段），提前到达的应答会被当成
    /// 未知帧丢掉 ⇒ 必须一帧一帧推。
    pub fn live_scripted(frames: &[(String, String)], tail: &str) -> Self {
        let fake = Self::allocate("live-scripted");
        let body = fake.scripted_body(frames, tail);
        fake.install(&body);
        fake
    }

    /// 逐帧回放完 `frames` 后写 stderr 并**非零退出**（非零退出用例）。
    ///
    /// 调用方只喂"倒数第二组之前的帧"：进程级失败的归因（退出码 + stderr 尾巴）
    /// 不需要先跑完一轮 turn，而"解码器已判 Completed、退出码却是 3"是有歧义的场景。
    pub fn live_scripted_failing(
        frames: &[(String, String)],
        exit_code: i32,
        stderr: &str,
    ) -> Self {
        let fake = Self::allocate("live-scripted-failing");
        let error = fake.payload("stderr.txt", &format!("{stderr}\n"));
        let tail = format!("cat {} >&2\nexit {exit_code}\n", quoted(&error));
        let body = fake.scripted_body(frames, &tail);
        fake.install(&body);
        fake
    }

    /// 等 `after` 出现 → 回放事件流 → 等 `until` 也出现 → 以 0 退出。
    ///
    /// 第二道闸门必不可少：进程若在 adapter 写出 `turn/start`（带 prompt 的那帧）
    /// 之前就退出，写入会得到 EPIPE，`stdin.txt` 里就看不到 prompt，
    /// “prompt 必须走 stdin” 的断言会随机失败。
    pub fn live_replaying(transcript: &str, after: &str, until: &str) -> Self {
        let fake = Self::allocate("live-replay");
        let payload = fake.payload("transcript.txt", transcript);
        let tail = format!("read_gate '{until}'\nexit 0\n");
        fake.install(&fake.live_script(after, Some(&payload), &tail));
        fake
    }

    /// 等 `after` 出现 → 回放 → 写 stderr → 以 `exit_code` 退出。
    pub fn live_failing(transcript: &str, exit_code: i32, stderr: &str, after: &str) -> Self {
        let fake = Self::allocate("live-failing");
        let payload = fake.payload("transcript.txt", transcript);
        let error = fake.payload("stderr.txt", &format!("{stderr}\n"));
        let tail = format!("cat {} >&2\nexit {exit_code}\n", quoted(&error));
        fake.install(&fake.live_script(after, Some(&payload), &tail));
        fake
    }

    /// 等 `after` 出现 → 回放 → 睡死（用于“流起来了再取消”）。
    pub fn live_replaying_then_sleeping(transcript: &str, seconds: u32, after: &str) -> Self {
        let fake = Self::allocate("live-replay-sleep");
        let payload = fake.payload("transcript.txt", transcript);
        let tail = format!("exec sleep {seconds}\n");
        fake.install(&fake.live_script(after, Some(&payload), &tail));
        fake
    }

    fn payload(&self, name: &str, content: &str) -> PathBuf {
        let path = self.dir.join(name);
        std::fs::write(&path, content).expect("写 payload");
        path
    }

    fn install(&self, body: &str) {
        use std::io::Write as _;
        use std::process::{Command as StdCommand, Stdio as StdStdio};

        // 脚本**刻意不**由本进程直接写（不用 `std::fs::write`）：
        // 测试是多线程跑的，别的线程 `fork` 到 `execve` 之间会复制本进程的 FD。
        // 若本进程正持有这个脚本的写 FD，别的线程的子进程就会短暂带着它，
        // 此刻我们 exec 这个刚写好的文件会随机得到 ETXTBSY（Text file busy）——
        // 实测 25 次连跑挂 2 次。交给子进程写（内容走 stdin）、等它退出再 exec，
        // 本进程的 FD 表里从未出现过写 FD，竞态就从根上没了。
        let mut child = StdCommand::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "cat > {path} && chmod +x {path}",
                path = quoted(&self.executable)
            ))
            .stdin(StdStdio::piped())
            .stdout(StdStdio::null())
            .stderr(StdStdio::null())
            .spawn()
            .expect("起写脚本的子进程");
        child
            .stdin
            .take()
            .expect("stdin 管道")
            .write_all(body.as_bytes())
            .expect("写脚本内容");
        let status = child.wait().expect("等写脚本的子进程");
        assert!(status.success(), "写脚本失败（chmod 一起）：{status}");
    }

    /// 公共前缀：把 argv 与 stdin 落到临时目录，便于"prompt 必须走 stdin"的断言。
    fn script(&self, tail: &str) -> String {
        format!(
            "#!/bin/sh\nd={dir}\nprintf '%s\\n' \"$@\" > \"$d/argv.txt\"\ncat > \"$d/stdin.txt\"\n{tail}",
            dir = quoted(&self.dir),
        )
    }
}

impl Drop for FakeCli {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 把路径写成 shell 单引号字面量（临时目录路径里出现单引号才会出问题，直接拦掉）。
fn quoted(path: &Path) -> String {
    let text = path.display().to_string();
    assert!(!text.contains('\''), "临时目录路径不能含单引号：{text}");
    format!("'{text}'")
}

/// 把任意一个词写成 shell 单引号字面量（闸门子串里含 `"` 也安全：单引号里不转义）。
fn quote_word(word: &str) -> String {
    assert!(!word.contains('\''), "shell 字面量不能含单引号：{word}");
    format!("'{word}'")
}
