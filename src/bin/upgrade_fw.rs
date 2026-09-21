//! 板卡固件升级：把原来手工的 7 步打包成一条命令。
//!
//! 手工流程（本工具要替代的）：
//!   1. 把 `BOOT<xxx>.bin` 拷到 `~/tftp`
//!   2. `ln -s BOOT<xxx>.bin BOOT.bin`
//!   3. 用 `~/tftp/md5.sh` 算 md5
//!   4. 把 md5 手抄进 `SwitchFW<xxx>.yaml`
//!   5. 起 `~/tftp/tftp.py`（非特权端口 1069）
//!   6. 发 `FetchFW.yaml`（异步：立刻返回，后台才去拉固件；现场只能靠"网口有没有数据流"判断）
//!   7. 发 `SwitchFW.yaml`（同步：`-t 40` 等 `succeeded=1/0`）
//!
//! 本工具的等价命令：
//! ```text
//! ./upgrade_fw --addr 192.168.5.174 --fw ~/tftp/BOOT320.bin --storage emmc
//! ```
//!
//! 与手工流程的关键差异：
//!   * **就地伺服**：不拷文件、不建 `BOOT.bin` 软链、不改 `~/tftp`、不写任何 yaml；
//!   * md5 就地现算，直接进 `SwitchFW` 报文（仓库里现有的 `SwitchFW*.yaml` 是旧快照，
//!     其 md5 与 `~/tftp` 里当前固件并不一致，所以不能沿用）；
//!   * 传输结束的判据是**最后一块 DATA 被 ACK**（服务端亲历，不依赖板卡发完成通知）；
//!   * **绝不自动 reboot**：只封装 1-7 步，重启永远交给人。

use binrw::{BinRead, BinWrite};
use clap::{Parser, ValueEnum};
use md5::{Digest, Md5};
use rand::{RngExt, rng};
use std::{
    fs::File,
    io::{Cursor, Read},
    net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use syncdaq::{
    ctrl_msg::CtrlMsg,
    tftp_supervisor::{self, MultiOptions, MultiProgress},
};

/// 默认控制端口（`-a/--addr` 只写 IP 时用它）
const DEFAULT_CONTROL_PORT: u16 = 3000;
/// 轮询片：每片醒一次，用来检查超时与外部中断
const POLL: Duration = Duration::from_millis(200);

// ---- 分档退出码 ----
const EXIT_OK: i32 = 0;
const EXIT_USAGE: i32 = 1; // 参数/文件错
const EXIT_NO_BOARD: i32 = 2; // 板卡无应答（Query 探活失败）
const EXIT_TRANSFER: i32 = 3; // TFTP 传输失败/超时/重复会话
const EXIT_SWITCH: i32 = 4; // SwitchFW 返回 succeeded=0 或超时未回
const EXIT_TFTP_PORT: i32 = 5; // TFTP 端口不可用
const EXIT_INTERRUPTED: i32 = 130;

/// FetchFW 是否已经发出（发出之后任何中断都可能让板卡进 jtag 状态）
static FETCHFW_SENT: AtomicBool = AtomicBool::new(false);
/// 传输是否已经完整结束（只有它置位后，中断才是"安全"的）
static TRANSFER_DONE: AtomicBool = AtomicBool::new(false);

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum StorageArg {
    /// eMMC (protocol storage=0)
    Emmc,
    /// SD card (protocol storage=1)
    Sd,
}

impl StorageArg {
    /// 协议里的取值：0 = emmc，1 = sd
    fn proto(self) -> u32 {
        match self {
            StorageArg::Emmc => 0,
            StorageArg::Sd => 1,
        }
    }

    fn label(self) -> &'static str {
        match self {
            StorageArg::Emmc => "emmc",
            StorageArg::Sd => "sd",
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "upgrade_fw",
    version,
    about = "Package the manual board-firmware upgrade (steps 1-7) into one command",
    long_about = None
)]
struct Args {
    /// Board control address: "IP" or "IP:PORT" (default port 3000)
    #[arg(short = 'a', long = "addr", value_name = "IP[:PORT]")]
    addr: String,

    /// Firmware file to serve. The board always asks for "BOOT.bin"; the local
    /// file name is arbitrary.
    #[arg(short = 'f', long = "fw", value_name = "FILE")]
    fw: PathBuf,

    /// Which storage to stage/switch: 0 = emmc, 1 = sd
    #[arg(short = 's', long = "storage", value_enum, value_name = "emmc|sd")]
    storage: StorageArg,

    /// TFTP server IP that the board will fetch from (default: auto-detect the
    /// egress IP towards the board)
    #[arg(long = "srv-ip", value_name = "IP")]
    srv_ip: Option<IpAddr>,

    /// TFTP port to listen on; falls back to a free port if it is busy
    #[arg(long = "tftp-port", default_value_t = 1069, value_name = "PORT")]
    tftp_port: u16,

    /// Timeout waiting for the synchronous SwitchFW reply (the manual flow's 40 s)
    #[arg(long = "switch-timeout", default_value_t = 40, value_name = "SEC")]
    switch_timeout: u64,

    /// Timeout for the Query pre-flight and the immediate FetchFW ack
    #[arg(long = "query-timeout", default_value_t = 2, value_name = "SEC")]
    query_timeout: u64,

    /// How long to wait for the board's first TFTP request after FetchFW
    #[arg(long = "rrq-timeout", default_value_t = 120, value_name = "SEC")]
    rrq_timeout: u64,

    /// Fail if the TFTP session makes no progress for this long
    #[arg(long = "idle-timeout", default_value_t = 15, value_name = "SEC")]
    idle_timeout: u64,

    /// Print the two control messages and exit without sending anything
    #[arg(long = "dry-run", conflicts_with_all = ["fetch_only", "switch_only"])]
    dry_run: bool,

    /// Steps 1-6 only: fetch the firmware but do NOT switch it
    #[arg(long = "fetch-only", conflicts_with_all = ["dry_run", "switch_only"])]
    fetch_only: bool,

    /// Switch only: send SwitchFW for what the board already fetched (no transfer)
    #[arg(long = "switch-only", conflicts_with_all = ["dry_run", "fetch_only"])]
    switch_only: bool,

    /// Max concurrent TFTP sessions (broadcast/multi-board mode)
    #[arg(long = "max-clients", default_value_t = 16, value_name = "N")]
    max_clients: usize,

    /// Stop this long after the last request once every session finished
    #[arg(long = "quiet-window", default_value_t = 3, value_name = "SEC")]
    quiet_window: u64,

    /// Overall cap on the fetch phase (0 = no cap)
    #[arg(long = "overall-timeout", default_value_t = 600, value_name = "SEC")]
    overall_timeout: u64,

    /// Debug verbosity for the built-in TFTP server (0 = quiet)
    #[arg(short = 'd', long = "debug", default_value_t = 0, value_name = "N")]
    debug: u32,
}

#[derive(Copy, Clone)]
enum ReplyKind {
    Query,
    FetchFw,
}

impl ReplyKind {
    fn label(self) -> &'static str {
        match self {
            ReplyKind::Query => "QueryReply",
            ReplyKind::FetchFw => "FetchFWReply",
        }
    }

    /// 回复类型是否符合预期（不符合只是告警：板卡回了别的报文这件事本身有信息量）
    fn matches(self, reply: &CtrlMsg) -> bool {
        matches!(
            (self, reply),
            (ReplyKind::Query, CtrlMsg::QueryReply { .. })
                | (ReplyKind::FetchFw, CtrlMsg::FetchFWReply { .. })
        )
    }
}

fn main() {
    // clap 对用法错误默认退 2，那会和"板卡无应答=2"撞车，所以这里自己接管：
    // --help/--version 正常打印退 0，其余用法错误统一映射成 EXIT_USAGE(1)。
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            let help_like = matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp
                    | clap::error::ErrorKind::DisplayVersion
                    | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            );
            std::process::exit(if help_like { EXIT_OK } else { EXIT_USAGE });
        }
    };
    install_guards();
    std::process::exit(run(&args));
}

/// 中断/崩溃的兜底：FetchFW 已发出但传输没完成时，必须把"板卡可能要人工 reboot"喊出来。
fn install_guards() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        prev(info);
        if FETCHFW_SENT.load(Ordering::Relaxed) && !TRANSFER_DONE.load(Ordering::Relaxed) {
            banner_fetch_failed("unexpected internal error (panic)", None);
        }
    }));
    let _ = ctrlc::set_handler(|| {
        if FETCHFW_SENT.load(Ordering::Relaxed) && !TRANSFER_DONE.load(Ordering::Relaxed) {
            banner_fetch_failed("interrupted by Ctrl-C", None);
        } else {
            eprintln!("\ninterrupted");
        }
        std::process::exit(EXIT_INTERRUPTED);
    });
}

/// FetchFW 发出之后失败时的横幅。
///
/// 现场经验：FetchFW 一旦发出却没跑完，板卡的抓取进程就会卡死——而**板卡照样响应
/// Query 等其它指令**，所以"还能 ping 通、还能回指令"完全不能当成它没卡死的证据。
/// 唯一的恢复手段是在 fpga 工程里做 JTAG 重编程（`jtag_run.sh`：PL+FSBL+app），
/// 普通 `Reboot` 指令在那种状态下没用。工具只负责把话说清楚并把决定权交回操作员。
fn banner_fetch_failed(reason: &str, started: Option<bool>) {
    let bar = "=".repeat(78);
    let started = match started {
        Some(true) => "yes - a TFTP session had already begun",
        Some(false) => "no - no TFTP request was ever served",
        None => "unknown",
    };
    eprintln!();
    eprintln!("{bar}");
    eprintln!(" OPERATOR ACTION REQUIRED - the firmware fetch did NOT complete.");
    eprintln!("   what happened    : {reason}");
    eprintln!("   transfer started : {started}");
    eprintln!();
    eprintln!(" FetchFW was already sent, so the board may now be in the \"stuck FetchFW\"");
    eprintln!(" state: it still answers Query and other commands - that is NOT a sign of");
    eprintln!(" health - but its fetch process is wedged and further upgrades will not work.");
    eprintln!(" Recovery is a JTAG reprogram, NOT a reboot (the Reboot command does not help):");
    eprintln!();
    eprintln!("   cd ~/fpga/sync_daq_100MSps_iq/scripts && ./jtag_run.sh");
    eprintln!();
    eprintln!(" Check the board console (/dev/ttyUSB1, 115200 8N1) for the reason, then decide:");
    eprintln!("   * console shows the fetch task never started (e.g. a malloc failure in the");
    eprintln!("     firmware) -> the board itself is fine, the FIRMWARE needs fixing;");
    eprintln!("   * console shows a fetch in progress/interrupted -> treat it as wedged and");
    eprintln!("     recover it with jtag_run.sh before running ./upgrade_fw again.");
    eprintln!("{bar}");
}

fn step(i: u32, n: u32, msg: impl AsRef<str>) {
    println!("[{i}/{n}] {}", msg.as_ref());
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn note(msg: impl AsRef<str>) {
    println!("      {}", msg.as_ref());
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn human(n: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let v = n as f64;
    if v >= GIB {
        format!("{:.2} GiB", v / GIB)
    } else if v >= MIB {
        format!("{:.1} MiB", v / MIB)
    } else if v >= KIB {
        format!("{:.1} KiB", v / KIB)
    } else {
        format!("{n} B")
    }
}

fn hex16(md5: &[u8; 16]) -> String {
    let mut s = String::with_capacity(32);
    for b in md5 {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 按现有 `SwitchFW*.yaml` 的写法排版 md5 数组
fn yaml_md5(md5: &[u8; 16]) -> String {
    let items: Vec<String> = md5.iter().map(|b| format!("0x{b:02x}")).collect();
    format!("[ {}  ]", items.join(", "))
}

fn yaml_fetchfw(msg_id: u32, srv_ip: [u8; 4], srv_port: u16, storage: u32) -> String {
    format!(
        "- !FetchFW\n  msg_id: {msg_id}\n  srv_ip: [{}, {}, {}, {}]\n  srv_port: {srv_port}\n  storage: {storage}",
        srv_ip[0], srv_ip[1], srv_ip[2], srv_ip[3]
    )
}

fn yaml_switchfw(msg_id: u32, md5: &[u8; 16], storage: u32) -> String {
    format!(
        "- !SwitchFW\n  msg_id: {msg_id}\n  md5sum:\n      {}\n  storage: {storage}",
        yaml_md5(md5)
    )
}

fn parse_target(s: &str) -> Result<SocketAddr, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("--addr is empty".into());
    }
    // "ip:port" 或 "[v6]:port"
    if t.starts_with('[') || t.matches(':').count() == 1 {
        return t
            .to_socket_addrs()
            .map_err(|e| format!("bad --addr {t:?}: {e}"))?
            .next()
            .ok_or_else(|| format!("bad --addr {t:?}: no address resolved"));
    }
    // 裸 IP（含裸 v6）：补默认控制端口
    let ip: IpAddr = t
        .parse()
        .map_err(|e| format!("bad --addr {t:?} (expected IP or IP:PORT): {e}"))?;
    Ok(SocketAddr::new(ip, DEFAULT_CONTROL_PORT))
}

/// 探测"本机去板卡方向的出口 IP"——UDP connect 只做路由查表，不发任何报文。
fn detect_egress_ip(board: IpAddr) -> Result<IpAddr, String> {
    let bind = if board.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let s = UdpSocket::bind(bind).map_err(|e| format!("bind {bind} failed: {e}"))?;
    // 目标可能是广播地址（192.168.5.255）：不设 SO_BROADCAST 的话，连 connect()
    // 都会直接返回 EACCES (Permission denied)。send_cmd 里同样有这一句。
    s.set_broadcast(true)
        .map_err(|e| format!("set_broadcast failed: {e}"))?;
    s.connect(SocketAddr::new(board, 9))
        .map_err(|e| format!("cannot route to {board}: {e}"))?;
    let local = s.local_addr().map_err(|e| e.to_string())?.ip();
    if local.is_unspecified() {
        return Err(format!(
            "could not determine the local source IP towards {board}; pass --srv-ip"
        ));
    }
    Ok(local)
}

/// 目标是不是一个广播地址（子网广播 `x.x.x.255` 或 255.255.255.255）。
///
/// 广播在单板卡场景很好用（DHCP 地址变了也不用改命令），但**所有**板卡都会收到指令，
/// 而本工具只服务一个 TFTP 会话，所以多板卡时必须提前警告。
fn is_broadcast_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_broadcast() || v4.octets()[3] == 255,
        IpAddr::V6(_) => false,
    }
}

fn md5_file(path: &Path) -> Result<[u8; 16], String> {
    let mut f = File::open(path).map_err(|e| format!("open {} failed: {e}", path.display()))?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {} failed: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let out = hasher.finalize();
    let mut a = [0u8; 16];
    a.copy_from_slice(&out);
    Ok(a)
}

fn serialize(cmd: &CtrlMsg) -> Result<Vec<u8>, String> {
    let mut cur = Cursor::new(Vec::new());
    cmd.write(&mut cur)
        .map_err(|e| format!("serialize control message failed: {e}"))?;
    Ok(cur.into_inner())
}

/// 发一条控制指令并等它的回复（同 `send_cmd` 的语义：收到即返回，超时返回 `None`）。
///
/// 这里不复用 `ctrl_msg::send_cmd`：那条路径内部有 `expect`/`assert`，遇到畸形回复会
/// panic，而这个工具一旦 FetchFW 发出就不能崩；另外它固定绑 `[::]:3001`，并发跑会撞端口。
fn ctrl_roundtrip(
    cmd: CtrlMsg,
    target: SocketAddr,
    timeout: Duration,
    want: ReplyKind,
    debug: u32,
) -> Result<Option<(SocketAddr, CtrlMsg)>, String> {
    let bytes = serialize(&cmd)?;
    let bind = if target.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let sock = UdpSocket::bind(bind).map_err(|e| format!("bind {bind} failed: {e}"))?;
    // 允许发往广播地址（192.168.5.255）；没有这一句 send_to 会报 EACCES
    sock.set_broadcast(true)
        .map_err(|e| format!("set_broadcast failed: {e}"))?;
    sock.send_to(&bytes, target)
        .map_err(|e| format!("send to {target} failed: {e}"))?;
    let sent_at = Instant::now();
    let deadline = sent_at + timeout;
    let mid = cmd.get_msg_id();
    if debug > 0 {
        eprintln!("[ctrl] sent {} bytes to {target}, msg_id={mid}", bytes.len());
    }
    let mut buf = vec![0u8; 9000];
    loop {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            if debug > 0 {
                eprintln!(
                    "[ctrl] no reply for msg_id={mid} within {:.1}s",
                    timeout.as_secs_f64()
                );
            }
            return Ok(None);
        }
        sock.set_read_timeout(Some(remain.min(POLL)))
            .map_err(|e| e.to_string())?;
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                if debug > 0 {
                    eprintln!("[ctrl] reply {n} bytes from {from}");
                }
                match CtrlMsg::read(&mut Cursor::new(&buf[..n])) {
                    Ok(reply) => {
                        if reply.get_msg_id() != mid {
                            if debug > 0 {
                                eprintln!(
                                    "[ctrl] ignoring reply with msg_id={} (ours is {mid})",
                                    reply.get_msg_id()
                                );
                            }
                            continue;
                        }
                        if debug > 0 {
                            eprintln!("[ctrl] reply after {:.3}s", sent_at.elapsed().as_secs_f64());
                        }
                        if !want.matches(&reply) {
                            eprintln!(
                                "[ctrl] warning: expected {} but got {}",
                                want.label(),
                                reply_name(&reply)
                            );
                        }
                        return Ok(Some((from, reply)));
                    }
                    Err(e) => {
                        eprintln!("[ctrl] warning: unparsable reply ({n} bytes) from {from}: {e}");
                        continue;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            // 被信号（Ctrl-C/SIGTERM）打断：不能当成"板卡无应答"。
            // 真正该退出的是信号处理器（它会打出横幅并 exit），这里继续等即可。
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                if debug > 0 {
                    eprintln!("[ctrl] recv interrupted by a signal; still waiting for msg_id={mid}");
                }
            }
            Err(e) => return Err(format!("recv failed: {e}")),
        }
    }
}

fn new_msg_id() -> u32 {
    rng().random::<u32>()
}

/// 只用于告警文案：给回复报文起个短名字
fn reply_name(reply: &CtrlMsg) -> &'static str {
    match reply {
        CtrlMsg::QueryReply { .. } => "QueryReply",
        CtrlMsg::FetchFWReply { .. } => "FetchFWReply",
        CtrlMsg::SwitchFWReply { .. } => "SwitchFWReply",
        CtrlMsg::InvalidMsg { .. } => "InvalidMsg",
        _ => "some other message",
    }
}
/// 单行聚合进度（N 块板卡并发抓取时的总览）
struct AggPrinter {
    tty: bool,
    last_print: Instant,
    last_pct: i64,
    printed: bool,
}

impl AggPrinter {
    fn new() -> Self {
        let tty = unsafe { libc::isatty(2) == 1 };
        Self {
            tty,
            last_print: Instant::now() - Duration::from_secs(1),
            last_pct: -1,
            printed: false,
        }
    }

    fn update(&mut self, p: &MultiProgress) {
        let pct = if p.target_bytes == 0 {
            0
        } else {
            (p.acked_bytes.saturating_mul(100) / p.target_bytes) as i64
        };
        let now = Instant::now();
        let due = if self.tty {
            pct != self.last_pct
                || now.duration_since(self.last_print) >= Duration::from_millis(200)
        } else {
            pct / 10 != self.last_pct / 10
        };
        if !due {
            return;
        }
        let secs = p.elapsed.as_secs_f64().max(1e-6);
        let rate = (p.acked_bytes as f64 / secs) as u64;
        let line = format!(
            "      fetching {} board(s) | {:3}% | {} / {} | {}/s | done {}/{} ok {} failed {}",
            p.started,
            pct,
            human(p.acked_bytes),
            human(p.target_bytes),
            human(rate),
            p.done,
            p.started,
            p.ok,
            p.failed
        );
        if self.tty {
            eprint!("\r\x1b[K{line}");
        } else {
            eprintln!("{line}");
        }
        self.last_print = now;
        self.last_pct = pct;
        self.printed = true;
    }

    fn finish(&mut self) {
        if self.tty && self.printed {
            eprintln!();
        }
    }
}

/// 用广播 Query 枚举子网内所有在线板卡（广播升级时必须先知道"谁在线"，才能逐块切换、
/// 也才能发现"在线却始终不开始抓"的异常板卡）。
fn enumerate_boards(target: SocketAddr, per_round: Duration, debug: u32) -> Vec<SocketAddr> {
    let mut found: Vec<SocketAddr> = Vec::new();
    let bind = if target.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    // 多轮是为了抗广播回复碰撞/错峰：任何一轮新增都算发现
    for round in 0..3 {
        let qid = new_msg_id();
        let bytes = match serialize(&CtrlMsg::Query { msg_id: qid }) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[ctrl] warning: {e}");
                continue;
            }
        };
        let sock = match UdpSocket::bind(bind) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[ctrl] warning: bind {bind} failed: {e}");
                continue;
            }
        };
        if sock.set_broadcast(true).is_err() {
            eprintln!("[ctrl] warning: set_broadcast failed");
        }
        if let Err(e) = sock.send_to(&bytes, target) {
            eprintln!("[ctrl] warning: broadcast Query failed: {e}");
            continue;
        }
        let deadline = Instant::now() + per_round;
        let mut got_this_round = 0usize;
        let mut last_seen: Option<Instant> = None;
        while Instant::now() < deadline {
            let remain = deadline.saturating_duration_since(Instant::now());
            // 本轮已经收到过回复且已静默 400ms 就提前收轮，省时间
            if let Some(t) = last_seen {
                if t.elapsed() >= Duration::from_millis(400) {
                    break;
                }
            }
            if sock
                .set_read_timeout(Some(remain.min(POLL).max(Duration::from_millis(1))))
                .is_err()
            {
                break;
            }
            match sock.recv_from(&mut [0u8; 9000]) {
                Ok((_n, from)) => {
                    let _ = from;
                    // 收到任何来源的回复都算"这块板卡在线"（msg_id 已由我们随机化，
                    // 不同板卡会各自回同一条 Query）
                    if !found.iter().any(|b| b.ip() == from.ip()) {
                        found.push(from);
                        got_this_round += 1;
                        if debug > 0 {
                            eprintln!("[ctrl] round {round}: board {from} answered");
                        }
                    }
                    last_seen = Some(Instant::now());
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut
                        || e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        if debug > 0 {
            eprintln!(
                "[ctrl] enumeration round {round}: +{got_this_round} (total {})",
                found.len()
            );
        }
    }
    found.sort_by_key(|b| b.ip().to_string());
    found
}

/// 逐块下发 SwitchFW（各自 msg_id），然后统一收回复直到全部到齐或超时。
///
/// 返回顺序与 `boards` 一致。
fn switch_all(
    boards: &[SocketAddr],
    md5: &[u8; 16],
    storage: u32,
    timeout: Duration,
    debug: u32,
) -> Result<Vec<(SocketAddr, Result<u32, String>)>, String> {
    use std::collections::HashMap;
    if boards.is_empty() {
        return Ok(Vec::new());
    }
    let bind = if boards[0].is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let sock = UdpSocket::bind(bind).map_err(|e| format!("bind {bind} failed: {e}"))?;
    sock.set_broadcast(true)
        .map_err(|e| format!("set_broadcast failed: {e}"))?;

    let mut pending: HashMap<u32, SocketAddr> = HashMap::new();
    let mut order: Vec<(u32, SocketAddr)> = Vec::new();
    for b in boards {
        let id = new_msg_id();
        let cmd = CtrlMsg::SwitchFW {
            msg_id: id,
            md5sum: *md5,
            storage,
        };
        let bytes = serialize(&cmd)?;
        sock.send_to(&bytes, b)
            .map_err(|e| format!("send SwitchFW to {b} failed: {e}"))?;
        pending.insert(id, *b);
        order.push((id, *b));
        println!("      -> SwitchFW sent to {b} (msg_id={id})");
    }

    let mut results: Vec<(SocketAddr, Result<u32, String>)> = Vec::new();
    let deadline = Instant::now() + timeout;
    let mut buf = vec![0u8; 9000];
    while !pending.is_empty() && Instant::now() < deadline {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            break;
        }
        sock.set_read_timeout(Some(remain.min(POLL).max(Duration::from_millis(1))))
            .map_err(|e| e.to_string())?;
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                match CtrlMsg::read(&mut Cursor::new(&buf[..n])) {
                    Ok(reply) => {
                        let mid = reply.get_msg_id();
                        if let Some(board) = pending.remove(&mid) {
                            match reply {
                                CtrlMsg::SwitchFWReply { succeeded, .. } => {
                                    results.push((board, Ok(succeeded)));
                                }
                                other => results.push((
                                    board,
                                    Err(format!("unexpected reply: {}", reply_name(&other))),
                                )),
                            }
                        } else if debug > 0 {
                            eprintln!("[ctrl] ignoring SwitchFW reply for unknown msg_id={mid} from {from}");
                        }
                    }
                    Err(e) => {
                        eprintln!("[ctrl] warning: unparsable reply ({n} bytes) from {from}: {e}");
                    }
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("recv failed: {e}")),
        }
    }
    for (id, b) in order {
        if pending.contains_key(&id) {
            results.push((
                b,
                Err(format!(
                    "no reply within {:.0}s (board may be busy or wedged)",
                    timeout.as_secs_f64()
                )),
            ));
        }
    }
    // 按输入顺序整理
    results.sort_by_key(|(b, _)| boards.iter().position(|x| x == b).unwrap_or(usize::MAX));
    Ok(results)
}

fn run(args: &Args) -> i32 {
    const N: u32 = 5;

    // ---------------- [1/5] 参数与文件 ----------------
    step(1, N, "checks: parameters + firmware file");

    let target = match parse_target(&args.addr) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };
    if !target.is_ipv4() {
        eprintln!("error: the board control path must be IPv4 (FetchFW carries a 4-byte srv_ip)");
        return EXIT_USAGE;
    }
    let meta = match std::fs::metadata(&args.fw) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: cannot stat {}: {e}", args.fw.display());
            return EXIT_USAGE;
        }
    };
    if !meta.is_file() {
        eprintln!("error: {} is not a regular file", args.fw.display());
        return EXIT_USAGE;
    }
    if meta.len() == 0 {
        eprintln!("error: {} is empty", args.fw.display());
        return EXIT_USAGE;
    }
    let md5 = match md5_file(&args.fw) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };
    let storage = args.storage.proto();
    let multi = is_broadcast_ip(target.ip());
    note(format!(
        "firmware: {} ({} bytes = {})",
        args.fw.display(),
        meta.len(),
        human(meta.len())
    ));
    note(format!("md5     : {}", hex16(&md5)));
    note(format!(
        "board   : {target}    storage: {} ({storage})    mode: {}",
        args.storage.label(),
        if multi { "MULTI-BOARD (broadcast)" } else { "single board (unicast)" }
    ));
    if args.fw.file_name().and_then(|s| s.to_str()) != Some("BOOT.bin") {
        note("note    : board always requests \"BOOT.bin\"; this file is served under that name");
    }
    if multi {
        note("warning : broadcast target: EVERY board on this subnet will act on FetchFW and");
        note("          SwitchFW; concurrency is capped by --max-clients. Make sure that is");
        note("          what you want, otherwise use a single board's unicast IP.");
    }

    let srv_ip = match args.srv_ip {
        Some(ip) => ip,
        None => match detect_egress_ip(target.ip()) {
            Ok(ip) => ip,
            Err(e) => {
                eprintln!("error: {e}");
                return EXIT_USAGE;
            }
        },
    };
    let srv_ip4 = match srv_ip {
        IpAddr::V4(v4) => v4.octets(),
        IpAddr::V6(_) => {
            eprintln!(
                "error: TFTP server IP must be IPv4 (FetchFW carries a 4-byte srv_ip); pass --srv-ip"
            );
            return EXIT_USAGE;
        }
    };
    note(format!("tftp srv: {srv_ip} (port {})", args.tftp_port));

    if args.dry_run {
        println!();
        println!("-- dry run: nothing was sent --");
        let did = new_msg_id();
        println!("{}", yaml_fetchfw(did, srv_ip4, args.tftp_port, storage));
        let sid = new_msg_id();
        println!("{}", yaml_switchfw(sid, &md5, storage));
        println!();
        if multi {
            println!("would enumerate online boards with a broadcast Query, then serve up to {} of", args.max_clients);
            println!("them concurrently and send SwitchFW to EVERY online board (storage={storage})");
        } else {
            println!("would send: FetchFW to {target}, then serve {} over TFTP, then SwitchFW (storage={storage})", args.fw.display());
        }
        println!("would NOT reboot (this tool never reboots)");
        return EXIT_OK;
    }

    // ---------------- [2/5] 探活 / 枚举 ----------------
    let mut boards: Vec<SocketAddr> = Vec::new();
    if multi {
        step(2, N, format!("preflight: enumerating online boards (broadcast Query {target})"));
        let per_round = Duration::from_secs(args.query_timeout.max(1));
        boards = enumerate_boards(target, per_round, args.debug);
        if boards.is_empty() {
            eprintln!(
                "error: no board answered the broadcast Query on {target} within {} round(s)",
                3
            );
            return EXIT_NO_BOARD;
        }
        let list: Vec<String> = boards.iter().map(|b| b.to_string()).collect();
        note(format!(
            "{} online board(s): {}",
            boards.len(),
            list.join(", ")
        ));
    } else {
        step(2, N, format!("preflight: Query {target}"));
        let qid = new_msg_id();
        match ctrl_roundtrip(
            CtrlMsg::Query { msg_id: qid },
            target,
            Duration::from_secs(args.query_timeout),
            ReplyKind::Query,
            args.debug,
        ) {
            Ok(Some((
                from,
                CtrlMsg::QueryReply {
                    fm_ver,
                    tick_cnt1,
                    tick_cnt2,
                    locked,
                    ..
                },
            ))) => {
                note(format!(
                    "board online: {from} fm_ver={fm_ver} (0x{fm_ver:08x}) tick={tick_cnt1}/{tick_cnt2} locked=0x{locked:08x}"
                ));
                boards.push(from);
            }
            Ok(Some((from, other))) => {
                note(format!("{from} replied, but not to Query: {other}"));
                boards.push(from);
            }
            Ok(None) => {
                eprintln!(
                    "error: no Query reply from {target} within {}s - board unreachable or wrong --addr",
                    args.query_timeout
                );
                return EXIT_NO_BOARD;
            }
            Err(e) => {
                eprintln!("error: {e}");
                return EXIT_NO_BOARD;
            }
        }
    }

    // ---------------- [3/5] 取固件 ----------------
    let mut fetch_failed: Vec<String> = Vec::new();
    let mut served_md5 = md5;
    let mut fetch_lines: Vec<(SocketAddr, String)> = Vec::new();
    let mut tftp_desc = String::from("skipped");
    if args.switch_only {
        step(3, N, "fetch: skipped (--switch-only)");
        note("using this file's current md5; run it against the SAME image the board(s) fetched");
    } else {
        step(
            3,
            N,
            format!(
                "fetch: serving {} over TFTP to {} board(s)",
                args.fw.display(),
                boards.len()
            ),
        );
        let mopts = MultiOptions {
            path: args.fw.clone(),
            bind_ip: srv_ip,
            port: args.tftp_port,
            rrq_timeout: Duration::from_secs(args.rrq_timeout),
            idle_timeout: Duration::from_secs(args.idle_timeout),
            max_sessions: args.max_clients.max(1),
            quiet_window: Duration::from_secs(args.quiet_window),
            overall_timeout: if args.overall_timeout == 0 {
                None
            } else {
                Some(Duration::from_secs(args.overall_timeout))
            },
            debug: args.debug,
        };
        let sup = match tftp_supervisor::bind(&mopts) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {e}");
                return EXIT_TFTP_PORT;
            }
        };
        let listen_port = sup.port();
        let fell_back = sup.port_fallback();
        note(format!(
            "tftp listening on {srv_ip}:{listen_port} (port fallback: {})",
            if fell_back { "yes" } else { "no" }
        ));

        let fid = new_msg_id();
        let fetch = CtrlMsg::FetchFW {
            msg_id: fid,
            srv_ip: srv_ip4,
            srv_port: listen_port as u32,
            storage,
        };
        println!("      --- FetchFW ---");
        for l in yaml_fetchfw(fid, srv_ip4, listen_port, storage).lines() {
            println!("      {l}");
        }

        FETCHFW_SENT.store(true, Ordering::Relaxed);
        match ctrl_roundtrip(
            fetch,
            target,
            Duration::from_secs(args.query_timeout),
            ReplyKind::FetchFw,
            args.debug,
        ) {
            Ok(Some((
                from,
                CtrlMsg::FetchFWReply {
                    nbytes_fetched,
                    md5checked,
                    ..
                },
            ))) => {
                note(format!(
                    "board ack (async) from {from}: nbytes_fetched={nbytes_fetched} md5checked={md5checked}"
                ));
                note("this ack is NOT the end of the transfer - the boards are fetching now");
            }
            Ok(Some((from, other))) => {
                note(format!("unexpected reply to FetchFW from {from}: {other}"))
            }
            Ok(None) => note("no immediate ack (async command); waiting for TFTP requests"),
            Err(e) => note(format!("warning: {e}")),
        }

        let mut printer = AggPrinter::new();
        let rep = match sup.serve(&mopts, &mut |p| printer.update(p)) {
            Ok(r) => r,
            Err(e) => {
                printer.finish();
                eprintln!("error: TFTP serving failed: {e}");
                match e {
                    tftp_supervisor::ServeError::NoRequest { .. } => banner_fetch_failed(
                        &format!(
                            "no TFTP request arrived on port {listen_port} within {}s after FetchFW",
                            args.rrq_timeout
                        ),
                        Some(false),
                    ),
                    _ => banner_fetch_failed(&format!("TFTP serving failed: {e}"), None),
                }
                return EXIT_TRANSFER;
            }
        };
        printer.finish();
        TRANSFER_DONE.store(
            rep.sessions.iter().all(|s| s.completed) && !rep.sessions.is_empty(),
            Ordering::Relaxed,
        );

        let secs = rep.elapsed.as_secs_f64().max(1e-6);
        note(format!(
            "{} session(s): {} completed, image {} served, elapsed {:.1}s",
            rep.sessions.len(),
            rep.completed(),
            human(rep.image_bytes),
            secs
        ));
        tftp_desc = format!("{srv_ip}:{listen_port}");

        if rep.served_md5 != md5 {
            println!();
            eprintln!("error: the firmware file CHANGED while it was being served - refusing to switch");
            eprintln!("       md5 computed before serving : {}", hex16(&md5));
            eprintln!("       md5 of the bytes actually served: {}", hex16(&rep.served_md5));
            eprintln!("       The board(s) staged the SERVED content, so sending the earlier md5 in");
            eprintln!("       SwitchFW would fail the board's own check (succeeded=0).");
            eprintln!("       Re-run ./upgrade_fw to fetch the file as it is now.");
            return EXIT_USAGE;
        }
        served_md5 = rep.served_md5;

        for s in &rep.sessions {
            if s.completed {
                fetch_lines.push((
                    s.peer,
                    format!(
                        "OK {} in {:.1}s (blksize {}, {} blocks, {} retx)",
                        human(s.bytes),
                        s.elapsed.as_secs_f64(),
                        s.blksize,
                        s.blocks,
                        s.retransmits
                    ),
                ));
            } else {
                let why = s.note.clone().unwrap_or_else(|| "did not complete".into());
                fetch_lines.push((s.peer, format!("INCOMPLETE: {why}")));
                fetch_failed.push(format!("{}: {why}", s.peer));
            }
        }
        if multi {
            // 枚举到、却始终没发起 RRQ 的板卡：正是"卡死态"的典型特征
            for b in &boards {
                if !rep.sessions.iter().any(|s| s.peer.ip() == b.ip()) {
                    fetch_lines.push((
                        *b,
                        "NOT STARTED (answered preflight, never sent an RRQ)".into(),
                    ));
                    fetch_failed
                        .push(format!("{b}: answered preflight but never started fetching"));
                }
            }
        }
        if !rep.rejected.is_empty() {
            let list: Vec<String> = rep.rejected.iter().map(|b| b.to_string()).collect();
            note(format!(
                "warning : refused {} client(s) over the concurrency cap ({}): {}",
                rep.rejected.len(),
                args.max_clients,
                list.join(", ")
            ));
            for b in &rep.rejected {
                fetch_failed.push(format!(
                    "{b}: refused (concurrency cap {}) - its fetch was interrupted",
                    args.max_clients
                ));
            }
        }

        if args.fetch_only {
            println!();
            println!("-- fetch-only: firmware staged on {} (storage={storage}), NOT switched --", args.storage.label());
            print_board_table(&boards, &fetch_lines, &[]);
            if !fetch_failed.is_empty() {
                banner_fetch_failed(
                    &format!("{} board(s) did not complete the fetch", fetch_failed.len()),
                    Some(true),
                );
                return EXIT_TRANSFER;
            }
            println!(
                "   to switch later: ./upgrade_fw --addr {} --fw {} --storage {} --switch-only",
                args.addr,
                args.fw.display(),
                args.storage.label()
            );
            return EXIT_OK;
        }
    }

    // ---------------- [4/5] 切换 ----------------
    let targets: Vec<SocketAddr> = boards.clone();
    step(
        4,
        N,
        format!(
            "switch: SwitchFW to {} board(s), waiting up to {}s in total",
            targets.len(),
            args.switch_timeout
        ),
    );
    if targets.is_empty() {
        eprintln!("error: no board to switch");
        return EXIT_NO_BOARD;
    }
    println!("      --- SwitchFW (md5={}) ---", hex16(&served_md5));
    for l in yaml_switchfw(0, &served_md5, storage).lines().skip(1) {
        println!("      {l}");
    }
    let sw = match switch_all(
        &targets,
        &served_md5,
        storage,
        Duration::from_secs(args.switch_timeout),
        args.debug,
    ) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            hint_switch_only(args, false);
            return EXIT_SWITCH;
        }
    };
    let mut switch_lines: Vec<(SocketAddr, String)> = Vec::new();
    for (b, r) in &sw {
        let line = match r {
            Ok(1) => "succeeded=1".to_string(),
            Ok(v) => format!("succeeded={v} (refused: md5 mismatch or nothing staged)"),
            Err(e) => format!("FAILED: {e}"),
        };
        switch_lines.push((*b, line));
    }

    // ---------------- [5/5] 汇总 ----------------
    step(5, N, "summary");
    println!("      firmware     {}", args.fw.display());
    println!("      size         {} bytes ({})", meta.len(), human(meta.len()));
    println!("      md5          {}", hex16(&served_md5));
    println!(
        "      target       {target}  ({})",
        if multi { "broadcast: multi-board" } else { "unicast: single board" }
    );
    println!("      storage      {} ({storage})", args.storage.label());
    println!("      tftp         {tftp_desc}");
    println!();
    print_board_table(&boards, &fetch_lines, &switch_lines);

    let fetch_anomalies = fetch_failed.len();
    let switch_failures = sw.iter().filter(|(_, r)| !matches!(r, Ok(1))).count();
    let code = if fetch_anomalies > 0 {
        eprintln!();
        eprintln!("error: {fetch_anomalies} board(s) had FETCH problems:");
        for f in &fetch_failed {
            eprintln!("       - {f}");
        }
        banner_fetch_failed(
            &format!("{fetch_anomalies} board(s) did not fetch the firmware"),
            Some(true),
        );
        EXIT_TRANSFER
    } else if switch_failures > 0 {
        eprintln!();
        eprintln!("error: {switch_failures} board(s) failed to switch (fetch was fine)");
        hint_switch_only(args, true);
        EXIT_SWITCH
    } else {
        println!();
        println!(
            "RESULT: OK - {} board(s) fetched and selected the firmware on {}.",
            boards.len(),
            args.storage.label()
        );
        println!("This tool never reboots. Reboot the boards when ready, e.g.");
        for b in &boards {
            println!(
                "  cargo run --release --bin send_cmd -- -a {b} -c cmd/Reboot.yaml -d 1 -t 3"
            );
        }
        EXIT_OK
    };
    code
}

/// 逐板卡结果表：把"抓取"和"切换"两维并排打出来
fn print_board_table(
    boards: &[SocketAddr],
    fetch_lines: &[(SocketAddr, String)],
    switch_lines: &[(SocketAddr, String)],
) {
    let mut rows: Vec<(String, String, String)> = Vec::new();
    for b in boards {
        let f = fetch_lines
            .iter()
            .find(|(x, _)| x.ip() == b.ip())
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| "skipped".into());
        let s = switch_lines
            .iter()
            .find(|(x, _)| x.ip() == b.ip())
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| "-".into());
        rows.push((b.to_string(), f, s));
    }
    let w0 = rows.iter().map(|r| r.0.len()).max().unwrap_or(5).max(5);
    let w1 = rows.iter().map(|r| r.1.len()).max().unwrap_or(5).max(5);
    println!("      {:<w0$}  {:<w1$}  {}", "board", "fetch", "switch");
    for (b, f, s) in rows {
        println!("      {b:<w0$}  {f:<w1$}  {s}");
    }
}
fn hint_switch_only(args: &Args, transferred_now: bool) {
    if transferred_now {
        eprintln!("hint: the firmware was transferred completely, so do NOT re-fetch. Retry just the switch with:");
    } else {
        eprintln!("hint: retry just the switch (no transfer) with:");
    }
    eprintln!(
        "  ./upgrade_fw --addr {} --fw {} --storage {} --switch-only",
        args.addr,
        args.fw.display(),
        args.storage.label()
    );
}
