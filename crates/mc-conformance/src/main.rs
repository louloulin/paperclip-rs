//! `mc-conformance` CLI：回放 golden fixture，按需写出可 `--check` 的报告。

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use mc_conformance::{
    harness, load_dir, merge, run_tier, to_row, Bindings, Fixture, Outcome, Report, Tier,
};

/// 回放 `contracts/golden/**` 里的上游 golden fixture。
///
/// 退出码：0 = 回放完成且（若给了 `--require-pass`）达标；1 = `--check` 报告漂移；
/// 2 = 用法/加载错误；3 = `--require-pass` 未达标。
#[derive(Debug, Parser)]
#[command(name = "mc-conformance", version, about)]
struct Args {
    /// golden fixture 目录。
    #[arg(long, default_value = "contracts/golden", env = "MC_GOLDEN_DIR")]
    golden: PathBuf,

    /// 只回放 id 含该子串的 fixture（排查用）。
    #[arg(long)]
    filter: Option<String>,

    /// 只列 fixture 与来源，不发起回放。
    #[arg(long)]
    list: bool,

    /// 输出 JSON 报告（与 `--write` 的内容一致）。
    #[arg(long)]
    json: bool,

    /// 把报告写到该路径（stateless 层；确定性的，可提交、可 `--check`）。
    #[arg(long)]
    write: Option<PathBuf>,

    /// 与已提交的报告做字节比对，漂移则 exit 1。
    #[arg(long)]
    check: Option<PathBuf>,

    /// 测试库 URL；给了就额外跑 database 层（`MULTICA_TEST_DATABASE_URL` 亦可）。
    #[arg(long, env = "MULTICA_TEST_DATABASE_URL")]
    db_url: Option<String>,

    /// 不跑 database 层（即使环境里有 URL）。
    #[arg(long)]
    no_db: bool,

    /// 要求至少 pass 这么多条，否则 exit 3。
    #[arg(long, default_value_t = 0)]
    require_pass: usize,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    match run(args).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("mc-conformance: {e:#}");
            ExitCode::from(2)
        }
    }
}

#[allow(clippy::too_many_lines)] // 线性 CLI 流程：加载 → --list → 两层回放 → 报告；拆函数只是把状态搬来搬去。
async fn run(args: Args) -> Result<ExitCode> {
    let mut fixtures: Vec<Fixture> = load_dir(&args.golden)
        .with_context(|| format!("load golden dir {}", args.golden.display()))?;
    if let Some(f) = &args.filter {
        fixtures.retain(|fx| fx.id.contains(f.as_str()));
    }
    if fixtures.is_empty() {
        anyhow::bail!("no fixtures under {}", args.golden.display());
    }

    if args.list {
        for fx in &fixtures {
            println!(
                "{:<6} {:>3} {:<44} {} ({})",
                fx.method, fx.expect.status, fx.path, fx.id, fx.source.test
            );
        }
        println!("{} fixtures", fixtures.len());
        return Ok(ExitCode::SUCCESS);
    }

    // ---- stateless 层（CI 跑的、可 check 的那份）----------------------------
    let bindings = Bindings::stateless();
    let stateless_router = harness::stateless_router()?;
    let stateless = run_tier(&stateless_router, &fixtures, &bindings, Tier::Stateless).await;

    // ---- database 层（可选）-------------------------------------------------
    let mut database = None;
    let db_url = if args.no_db {
        None
    } else {
        args.db_url.clone()
    };
    if let Some(url) = db_url {
        match harness::database_router(&url).await {
            Ok((router, db_bindings)) => {
                let observed = run_tier(&router, &fixtures, &db_bindings, Tier::Database).await;
                eprintln!(
                    "database 层：{} 条（种子身份 user={} workspace={}）",
                    observed.len(),
                    db_bindings.user_id,
                    db_bindings.workspace_id
                );
                database = Some(observed);
            }
            Err(e) => {
                eprintln!("database 层跳过（{e:#}）；这是本地证据层，CI 不依赖它");
            }
        }
    }

    let rows = fixtures
        .iter()
        .enumerate()
        .map(|(i, fx)| {
            let db = database.as_ref().map(|d| d[i].clone());
            to_row(fx, &merge(Some(stateless[i].clone()), db))
        })
        .collect::<Vec<_>>();
    let report = Report::from_rows(&args.golden, &bindings, rows);

    if args.json {
        println!("{}", report.to_json()?);
    } else {
        print!("{}", report.render_text());
    }

    if let Some(path) = &args.write {
        let text = report.to_json()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &text).with_context(|| format!("write {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    }

    if let Some(path) = &args.check {
        let expected = std::fs::read_to_string(path)
            .with_context(|| format!("read committed report {}", path.display()))?;
        let actual = report.to_json()?;
        if expected != actual {
            eprintln!(
                "conformance report drifted from {}: 重新运行 mc-conformance --write {} 并解释差异",
                path.display(),
                path.display()
            );
            print_diff(&expected, &actual);
            return Ok(ExitCode::from(1));
        }
        eprintln!("report matches {}", path.display());
    }

    if report.totals.pass < args.require_pass {
        eprintln!(
            "require-pass: {} < {} （{}）",
            report.totals.pass,
            args.require_pass,
            report.summary_line()
        );
        return Ok(ExitCode::from(3));
    }

    // 有一个 fixture 都没被判定（既没 pass 也没说清为什么）才是真问题。
    if report.totals.fixtures != fixtures.len() {
        anyhow::bail!(
            "accounting broken: {} rows for {} fixtures",
            report.totals.fixtures,
            fixtures.len()
        );
    }
    let unexplained = report
        .fixtures
        .iter()
        .filter(|r| r.outcome == Outcome::Unevaluable && r.detail.trim().is_empty())
        .count();
    if unexplained > 0 {
        anyhow::bail!("{unexplained} fixtures are unevaluable with no reason recorded");
    }
    Ok(ExitCode::SUCCESS)
}

fn print_diff(expected: &str, actual: &str) {
    for (i, (a, b)) in expected.lines().zip(actual.lines()).enumerate() {
        if a != b {
            eprintln!(
                "  first difference at line {}:\n    committed: {a}\n    fresh:     {b}",
                i + 1
            );
            return;
        }
    }
    eprintln!(
        "  line counts differ: committed {} vs fresh {}",
        expected.lines().count(),
        actual.lines().count()
    );
}
