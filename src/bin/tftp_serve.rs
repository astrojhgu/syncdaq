//! 独立 TFTP 服务端：给 `upgrade_fw` 的内置服务端做本地/现场测试用，
//! 也可以直接顶替原来的 `~/tftp/tftp.py`（fbtftp）：
//!
//! ```text
//! cargo run --release --bin tftp_serve -- --path ~/tftp/BOOT320.bin --port 1069
//! ```
//!
//! 与 `~/tftp/tftp.py` 的差别：
//!   * 不管客户端请求什么文件名，都回送 `--path` 指定的那个文件（板卡只请求 BOOT.bin）；
//!   * 传完一个会话就退出（一次一个会话），退出码和 `upgrade_fw` 对齐（0 成功 / 3 失败）；
//!   * 没有进展超过 `--idle-timeout` 就退出（不无限挂着）。

use clap::Parser;
use std::{
    net::IpAddr,
    path::PathBuf,
    time::{Duration, Instant},
};
use syncdaq::tftp_server::{self, Progress, ServeOptions};
use syncdaq::tftp_supervisor::{self, MultiOptions, MultiProgress};

#[derive(Parser, Debug)]
#[command(name = "tftp_serve", version, about = "Minimal read-only TFTP server")]
struct Args {
    /// File to serve (any requested name maps to it)
    #[arg(long = "path", value_name = "FILE")]
    path: PathBuf,

    /// Local address to bind
    #[arg(long = "bind-ip", default_value = "0.0.0.0", value_name = "IP")]
    bind_ip: IpAddr,

    /// Port to listen on (falls back to a free port if busy)
    #[arg(long = "port", default_value_t = 1069, value_name = "PORT")]
    port: u16,

    /// How long to wait for the first request
    #[arg(long = "rrq-timeout", default_value_t = 3600, value_name = "SEC")]
    rrq_timeout: u64,

    /// Fail if the session makes no progress for this long
    #[arg(long = "idle-timeout", default_value_t = 15, value_name = "SEC")]
    idle_timeout: u64,

    /// Serve several clients concurrently (multi-board / broadcast use)
    #[arg(long = "multi")]
    multi: bool,

    /// Concurrency cap for --multi
    #[arg(long = "max-clients", default_value_t = 16, value_name = "N")]
    max_clients: usize,

    /// With --multi: stop after this long with no new request once all sessions finished
    #[arg(long = "quiet-window", default_value_t = 3, value_name = "SEC")]
    quiet_window: u64,

    #[arg(short = 'd', long = "debug", default_value_t = 1, value_name = "N")]
    debug: u32,
}

fn main() {
    let args = Args::parse();
    if args.multi {
        run_multi(&args);
        return;
    }
    let opts = ServeOptions {
        path: args.path.clone(),
        bind_ip: args.bind_ip,
        port: args.port,
        rrq_timeout: Duration::from_secs(args.rrq_timeout),
        idle_timeout: Duration::from_secs(args.idle_timeout),
        preload: None,
        debug: args.debug,
    };

    let server = match tftp_server::bind(&opts) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(5);
        }
    };
    println!(
        "tftp_serve: {} -> {}:{} (fallback: {})",
        args.path.display(),
        args.bind_ip,
        server.port(),
        if server.port_fallback() { "yes" } else { "no" }
    );

    let start = Instant::now();
    let mut last_note = Instant::now() - Duration::from_secs(60);
    let res = server.serve(&opts, &mut |p: &Progress| {
        // 简单节流：每 0.5s 或每 1% 打一行
        if last_note.elapsed() < Duration::from_millis(500) {
            return;
        }
        last_note = Instant::now();
        let pct = if p.total == 0 {
            100
        } else {
            p.acked * 100 / p.total
        };
        println!(
            "  {:3}%  {}/{} bytes  block {}/{}  retx {}",
            pct, p.acked, p.total, p.block, p.last_block, p.retransmits
        );
    });

    match res {
        Ok(rep) => {
            let secs = rep.elapsed.as_secs_f64().max(1e-6);
            println!(
                "OK: {} bytes to {} in {:.1}s ({:.2} MiB/s), blksize {}, blocks {}, retransmits {}",
                rep.bytes,
                rep.peer,
                secs,
                rep.bytes as f64 / secs / 1048576.0,
                rep.blksize,
                rep.blocks,
                rep.retransmits
            );
            println!(
                "    requested name {:?}, options acked {:?}, total wall {:.1}s",
                rep.requested_name,
                rep.options_acked,
                start.elapsed().as_secs_f64()
            );
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(3);
        }
    }
}

/// 多客户端模式：并发服务到"所有会话完成 + 静默窗口"。
fn run_multi(args: &Args) {
    let mopts = MultiOptions {
        path: args.path.clone(),
        bind_ip: args.bind_ip,
        port: args.port,
        rrq_timeout: Duration::from_secs(args.rrq_timeout),
        idle_timeout: Duration::from_secs(args.idle_timeout),
        max_sessions: args.max_clients.max(1),
        quiet_window: Duration::from_secs(args.quiet_window),
        overall_timeout: None,
        debug: args.debug,
    };
    let sup = match tftp_supervisor::bind(&mopts) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(5);
        }
    };
    println!(
        "tftp_serve --multi: {} -> {}:{} (fallback: {}, max {} clients)",
        args.path.display(),
        args.bind_ip,
        sup.port(),
        if sup.port_fallback() { "yes" } else { "no" },
        args.max_clients
    );

    let start = Instant::now();
    let mut last = Instant::now() - Duration::from_secs(60);
    let res = sup.serve(&mopts, &mut |p: &MultiProgress| {
        if last.elapsed() < Duration::from_millis(500) {
            return;
        }
        last = Instant::now();
        let pct = if p.target_bytes == 0 {
            0
        } else {
            p.acked_bytes * 100 / p.target_bytes
        };
        println!(
            "  {} client(s) started, {} done (ok {}, failed {})  {}%  {}/{} bytes",
            p.started, p.done, p.ok, p.failed, pct, p.acked_bytes, p.target_bytes
        );
    });

    match res {
        Ok(rep) => {
            println!(
                "multi summary: {} session(s), {} completed, {} rejected, wall {:.1}s, md5 {}",
                rep.sessions.len(),
                rep.completed(),
                rep.rejected.len(),
                start.elapsed().as_secs_f64(),
                rep.served_md5
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            );
            for s in &rep.sessions {
                println!(
                    "  {} {}: {}/{} bytes, blksize {}, blocks {}, retx {}{}",
                    if s.completed { "OK  " } else { "FAIL" },
                    s.peer,
                    s.bytes,
                    rep.image_bytes,
                    s.blksize,
                    s.blocks,
                    s.retransmits,
                    s.note
                        .as_ref()
                        .map(|n| format!(" ({n})"))
                        .unwrap_or_default()
                );
            }
            if rep.sessions.iter().all(|s| s.completed) && !rep.sessions.is_empty() {
                std::process::exit(0);
            }
            std::process::exit(3);
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(3);
        }
    }
}
