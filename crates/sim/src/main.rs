//! `auto-trader-sim` バイナリのエントリポイント(計画 Task 9)。
//!
//! ログは `tracing` で標準エラーへ、結果(表または JSON)は `cli` モジュールが標準出力へ出す
//! (spec 13 章)。結果をパイプ消費する運用を想定するコマンドのため、ログと結果の出力先を
//! 明示的に分離する(`.with_writer(std::io::stderr)`)。

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let code = auto_trader_sim::cli::run_args(std::env::args_os()).await;
    std::process::exit(code);
}
