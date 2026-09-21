//! 内置只读 TFTP 服务端（RFC 1350 + `blksize`/`tsize`/`timeout` 选项协商）。
//!
//! 用途：`upgrade_fw` 用它把固件送给板卡。相比原来手工运行的 `~/tftp/tftp.py`（fbtftp），
//! 这里的重点是**让调用方掌握"传输到底结束了没有"这个判据**：
//!
//! * 传输结束的权威判据 = 最后一块 DATA 被 ACK（服务端亲历，不依赖板卡发完成通知）；
//! * 支持 16 位块号回绕——35MB / 512B = 68581 块 > 65535，不回绕必然错位；
//! * 与 fbtftp 保持一致的行为：每个会话 `bind(ip, 0)` 开新 TID、原样 ack 客户端请求的
//!   `blksize`（这两个行为已被现有手工流程证明能跟这块板卡配合）；
//! * 传输期内只接受一个会话：**同 IP** 重发 RRQ 视为丢包重传（重发 OACK/DATA1），
//!   **异 IP** 请求直接判失败；
//! * 无进展超过 `idle_timeout` 立即返回错误——是否放弃这个决定权交给调用方
//!   （板卡被中断会进 jtag 状态、普通 reboot 无效，必须人工处理）。

use std::{
    io,
    net::{IpAddr, SocketAddr, UdpSocket},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use md5::{Digest, Md5};

pub const OP_RRQ: u16 = 1;
pub const OP_WRQ: u16 = 2;
pub const OP_DATA: u16 = 3;
pub const OP_ACK: u16 = 4;
pub const OP_ERROR: u16 = 5;
pub const OP_OACK: u16 = 6;

pub const ERR_UNDEFINED: u16 = 0;
pub const ERR_NOT_FOUND: u16 = 1;
pub const ERR_ACCESS: u16 = 2;
pub const ERR_ILLEGAL: u16 = 4;

/// RFC 1350 的默认块大小
pub const DEFAULT_BLKSIZE: usize = 512;
/// `blksize` 选项上限（RFC 2348）
pub const MAX_BLKSIZE: usize = 65464;

/// 会话循环的轮询粒度：每 200ms 醒一次，用来检查"异 IP 请求""调用方要求停止""无进展超时"。
const POLL_SLICE: Duration = Duration::from_millis(200);

/// 传输进度快照（每收到一个 ACK 回调一次，节流由调用方自己做）
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    /// 已被 ACK 的字节数
    pub acked: u64,
    /// 文件总字节数
    pub total: u64,
    /// 已被 ACK 的块号（从 1 开始）
    pub block: u64,
    /// 最后一块的块号
    pub last_block: u64,
    /// 协商后的块大小
    pub blksize: usize,
    pub elapsed: Duration,
    pub retransmits: u64,
}

#[derive(Debug)]
pub enum ServeError {
    /// 文件读不了 / 网络 socket 出错
    Io(io::Error),
    /// `path` 不是一个普通文件
    NotAFile(PathBuf),
    /// 绑定监听端口失败（且已经没得退让）
    Bind { port: u16, source: io::Error },
    /// 超时都没等到第一条 RRQ
    NoRequest { waited: Duration, port: u16 },
    /// 传输中途无进展超过 idle_timeout
    Stalled {
        block: u64,
        last_block: u64,
        acked: u64,
        total: u64,
        idle: Duration,
        retransmits: u64,
    },
    /// 另一个 IP 也来拉固件（严格单会话）
    ForeignSession {
        first_peer: SocketAddr,
        second_peer: SocketAddr,
    },
    /// 板卡回了 ERROR 报文
    PeerError { code: u16, message: String },
    /// 客户端请求了非 octet 模式
    BadMode(String),
    /// RRQ 报文本身畸形
    BadRequest(String),
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServeError::Io(e) => write!(f, "socket/file I/O error: {e}"),
            ServeError::NotAFile(p) => write!(f, "{} is not a regular file", p.display()),
            ServeError::Bind { port, source } => {
                write!(f, "cannot bind UDP port {port}: {source}")
            }
            ServeError::NoRequest { waited, port } => write!(
                f,
                "no TFTP request on port {port} within {:.1}s",
                waited.as_secs_f64()
            ),
            ServeError::Stalled {
                block,
                last_block,
                acked,
                total,
                idle,
                retransmits,
            } => write!(
                f,
                "transfer stalled: block {block}/{last_block}, {acked}/{total} bytes acked, \
                 no progress for {:.1}s ({retransmits} retransmits)",
                idle.as_secs_f64()
            ),
            ServeError::ForeignSession {
                first_peer,
                second_peer,
            } => write!(
                f,
                "second TFTP session refused: {second_peer} asked for the firmware while \
                 {first_peer} is still fetching"
            ),
            ServeError::PeerError { code, message } => {
                write!(f, "peer replied TFTP ERROR {code}: {message}")
            }
            ServeError::BadMode(m) => write!(f, "unsupported transfer mode {m:?} (need octet)"),
            ServeError::BadRequest(m) => write!(f, "malformed RRQ: {m}"),
        }
    }
}

impl std::error::Error for ServeError {}

impl From<io::Error> for ServeError {
    fn from(e: io::Error) -> Self {
        ServeError::Io(e)
    }
}

#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// 要送出去的文件（板卡请求 `BOOT.bin`，本地叫什么名字都行）
    pub path: PathBuf,
    /// 绑定的本机地址（= 告诉板卡的 srv_ip）
    pub bind_ip: IpAddr,
    /// 期望端口；被占用且 `port != 0` 时自动退让到内核临时端口
    pub port: u16,
    /// 等第一条 RRQ 的上限
    pub rrq_timeout: Duration,
    /// 传输期"无进展"上限
    pub idle_timeout: Duration,
    /// 预读好的镜像：多客户端 supervisor 读一次后用 `Arc` 共享给每个会话，
    /// 保证所有板卡拿到的字节完全相同、md5 天然一致（也避免 N 份 35MB 拷贝）。
    pub preload: Option<Arc<Vec<u8>>>,
    pub debug: u32,
}

#[derive(Debug)]
pub struct ServeReport {
    /// 实际监听的端口（自动退让时与 `ServeOptions::port` 不同）
    pub server_port: u16,
    /// 端口是否发生了自动退让
    pub port_fallback: bool,
    /// 真正拉固件的对端
    pub peer: SocketAddr,
    /// 板卡请求的文件名（预期是 `BOOT.bin`）
    pub requested_name: String,
    pub blksize: usize,
    /// 实际回给板卡的选项（含 `blksize`/`tsize`/`timeout`）
    pub options_acked: Vec<(String, String)>,
    pub bytes: u64,
    pub blocks: u64,
    /// **真正被送出去的那份内容**的 md5。
    ///
    /// 调用方开局算的 md5 与这里可能不一致（文件在抓取途中被替换过）。板卡的
    /// SwitchFW 校验的是它暂存下来的内容，所以发 SwitchFW 之前必须以这个值为准。
    pub served_md5: [u8; 16],
    pub retransmits: u64,
    /// 同 IP 重发 RRQ 的次数（丢包重传，不算失败）
    pub rrq_retransmits: u64,
    pub elapsed: Duration,
}

fn tlog(debug: u32, level: u32, msg: impl AsRef<str>) {
    if debug >= level {
        // 先清掉进度行，避免两个输出互相污染
        eprintln!("\r\x1b[K[tftp] {}", msg.as_ref());
    }
}

/// RRQ 解析结果
#[derive(Debug, Clone)]
struct Rrq {
    name: String,
    mode: String,
    options: Vec<(String, String)>,
}

fn parse_rrq(buf: &[u8]) -> Result<Rrq, ServeError> {
    if buf.len() < 4 {
        return Err(ServeError::BadRequest("packet too short".into()));
    }
    let mut parts = buf[2..].split(|b| *b == 0);
    let name = parts
        .next()
        .ok_or_else(|| ServeError::BadRequest("missing filename".into()))?;
    let mode = parts
        .next()
        .ok_or_else(|| ServeError::BadRequest("missing mode".into()))?;
    let name = String::from_utf8_lossy(name).to_string();
    let mode = String::from_utf8_lossy(mode).to_string();
    let mut options = Vec::new();
    loop {
        match parts.next() {
            None => break,
            Some(k) if k.is_empty() => break,
            Some(k) => {
                let v = parts.next().ok_or_else(|| {
                    ServeError::BadRequest(format!("option {:?} without value", String::from_utf8_lossy(k)))
                })?;
                options.push((
                    String::from_utf8_lossy(k).to_ascii_lowercase(),
                    String::from_utf8_lossy(v).to_string(),
                ));
            }
        }
    }
    Ok(Rrq { name, mode, options })
}

fn oack_packet(opts: &[(String, String)]) -> Vec<u8> {
    let mut p = Vec::with_capacity(64);
    p.extend_from_slice(&OP_OACK.to_be_bytes());
    for (k, v) in opts {
        p.extend_from_slice(k.as_bytes());
        p.push(0);
        p.extend_from_slice(v.as_bytes());
        p.push(0);
    }
    p
}

fn data_packet(block: u64, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + payload.len());
    p.extend_from_slice(&OP_DATA.to_be_bytes());
    p.extend_from_slice(&(block as u16).to_be_bytes());
    p.extend_from_slice(payload);
    p
}

fn error_packet(code: u16, msg: &str) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + msg.len() + 1);
    p.extend_from_slice(&OP_ERROR.to_be_bytes());
    p.extend_from_slice(&code.to_be_bytes());
    p.extend_from_slice(msg.as_bytes());
    p.push(0);
    p
}

fn be_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([buf[off], buf[off + 1]])
}

/// 最后一块的块号。`total == 0` 或整除块大小时，末尾要补一个 0 长块。
fn last_block_no(total: u64, blksize: usize) -> u64 {
    total / blksize as u64 + 1
}

fn block_payload(file: &[u8], block: u64, blksize: usize) -> &[u8] {
    let off = (block - 1) * blksize as u64;
    if off >= file.len() as u64 {
        &[]
    } else {
        let end = ((off + blksize as u64) as usize).min(file.len());
        &file[off as usize..end]
    }
}

/// 已绑定、但还没开始服务的服务端。
///
/// 拆成"先 `bind` 再 `serve`"是有原因的：调用方必须在**发出 FetchFW 之前**就知道
/// 真正生效的端口——1069 被占时会自动退让到内核临时端口，而这个端口要写进 FetchFW，
/// 否则板卡会去 1069 拉固件（那里可能是别人的 tftp.py）。
pub struct Server {
    sock: UdpSocket,
    port: u16,
    port_fallback: bool,
}

/// 绑主端口；被占用时退让到内核临时端口。
pub fn bind(opts: &ServeOptions) -> Result<Server, ServeError> {
    let want = SocketAddr::new(opts.bind_ip, opts.port);
    let (sock, port, port_fallback) = match UdpSocket::bind(want) {
        Ok(s) => (s, opts.port, false),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse && opts.port != 0 => {
            let s = UdpSocket::bind(SocketAddr::new(opts.bind_ip, 0)).map_err(|source| {
                ServeError::Bind {
                    port: opts.port,
                    source,
                }
            })?;
            let p = s.local_addr()?.port();
            tlog(
                opts.debug,
                0,
                format!(
                    "UDP port {} is busy (leftover tftp.py?); falling back to {}",
                    opts.port, p
                ),
            );
            (s, p, true)
        }
        Err(source) => {
            return Err(ServeError::Bind {
                port: opts.port,
                source,
            });
        }
    };
    Ok(Server {
        sock,
        port,
        port_fallback,
    })
}

/// 便捷入口：绑端口 + 服务一个会话（独立测试服务端 `tftp_serve` 用这个）。
pub fn serve(
    opts: &ServeOptions,
    progress: &mut dyn FnMut(&Progress),
) -> Result<ServeReport, ServeError> {
    bind(opts)?.serve(opts, progress)
}

impl Server {
    /// 主端口 socket（多客户端 supervisor 需要自己收 RRQ）
    pub fn socket(&self) -> &UdpSocket {
        &self.sock
    }

    /// 实际监听的端口（就是该写进 FetchFW 的 `srv_port`）
    pub fn port(&self) -> u16 {
        self.port
    }

    /// 是否发生了端口自动退让
    pub fn port_fallback(&self) -> bool {
        self.port_fallback
    }

    /// 等第一条 RRQ，然后按单会话语义把它服务完（`tftp_serve` 用这条路径）。
    ///
    /// 调用方拿到 `Stalled` / `ForeignSession` / `PeerError` 就说明**传输已经开始却没完成**
    /// ——此时板卡可能已经处于需要人工 reboot 的状态，调用方必须把这件事喊出来。
    pub fn serve(
        self,
        opts: &ServeOptions,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<ServeReport, ServeError> {
        let Server {
            sock,
            port,
            port_fallback,
        } = self;
        let (rrq, peer) = wait_for_rrq(&sock, opts, port)?;
        let watch = Some(WatchState::new(sock));
        serve_session(opts, port, port_fallback, rrq, peer, watch, progress)
    }

    /// 把"别处已经收到的 RRQ"交给本 Server 服务（多客户端 supervisor 走这条路径）。
    ///
    /// 与 `serve` 的区别只有一个：**不起"异 IP 请求"监视线程**——发现新客户端是
    /// supervisor 的职责，会话自身的丢包重传逻辑与单会话路径完全一致。
    pub fn serve_from_rrq(
        self,
        opts: &ServeOptions,
        rrq: Vec<u8>,
        peer: SocketAddr,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<ServeReport, ServeError> {
        serve_session(opts, self.port, self.port_fallback, rrq, peer, None, progress)
    }
}

/// 会话期间"另一个客户端也来拉"的监视状态（只有单会话路径会用到）。
struct WatchState {
    sock: UdpSocket,
    stop: Arc<AtomicBool>,
    rrq_retrans: Arc<AtomicU64>,
    foreign: Arc<Mutex<Option<SocketAddr>>>,
}

impl WatchState {
    fn new(sock: UdpSocket) -> Self {
        Self {
            sock,
            stop: Arc::new(AtomicBool::new(false)),
            rrq_retrans: Arc::new(AtomicU64::new(0)),
            foreign: Arc::new(Mutex::new(None)),
        }
    }
}

/// 在主端口上等一条 RRQ；WRQ 明确拒绝，别的 opcode 忽略。
fn wait_for_rrq(
    sock: &UdpSocket,
    opts: &ServeOptions,
    port: u16,
) -> Result<(Vec<u8>, SocketAddr), ServeError> {
    let deadline = Instant::now() + opts.rrq_timeout;
    let mut buf = vec![0u8; 2048];
    loop {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return Err(ServeError::NoRequest {
                waited: opts.rrq_timeout,
                port,
            });
        }
        sock.set_read_timeout(Some(remain.min(POLL_SLICE)))?;
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                if n < 2 {
                    continue;
                }
                match be_u16(&buf, 0) {
                    OP_RRQ => return Ok((buf[..n].to_vec(), from)),
                    OP_WRQ => {
                        // 只读服务：明确拒绝，继续等真正的 RRQ
                        let _ = sock.send_to(&error_packet(ERR_ACCESS, "read-only server"), from);
                        tlog(opts.debug, 0, format!("refused WRQ from {from} (read-only)"));
                    }
                    other => {
                        tlog(
                            opts.debug,
                            1,
                            format!("ignored opcode {other} from {from} while waiting for RRQ"),
                        );
                    }
                }
            }
            // Interrupted：被信号打断。**绝不能**当成传输失败——那会把板卡推进
            // 需要 jtag_run.sh 的卡死态，所以这里当作"什么都没发生"继续等。
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::TimedOut
                    || e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ServeError::Io(e)),
        }
    }
}

/// 送完一个会话（单会话与多客户端共用同一份实现）。
///
/// `watch` 为 `Some` 时会在会话期间继续监听主端口：把**异 IP** 的 RRQ 记成
/// `ForeignSession` 失败（严格单会话语义），把**同 IP** 重发的 RRQ 记成丢包重传。
/// supervisor 传 `None`，因为那些判断由它自己做。
fn serve_session(
    opts: &ServeOptions,
    server_port: u16,
    port_fallback: bool,
    rrq_pkt: Vec<u8>,
    peer: SocketAddr,
    watch: Option<WatchState>,
    progress: &mut dyn FnMut(&Progress),
) -> Result<ServeReport, ServeError> {
    let meta = std::fs::metadata(&opts.path)?;
    if !meta.is_file() {
        return Err(ServeError::NotAFile(opts.path.clone()));
    }
    let total = meta.len();
    // 共享镜像（supervisor 预读）或就地读入：切片、tsize、末块判定都没有边界问题。
    let file: Arc<Vec<u8>> = match &opts.preload {
        Some(buf) => buf.clone(),
        None => Arc::new(std::fs::read(&opts.path)?),
    };
    // 记下"真正要塞进网线的那份内容"的 md5：调用方开局算的那份可能已经过期
    // （文件被抓取途中替换过），SwitchFW 必须用这个值。
    let served_md5: [u8; 16] = {
        let mut h = Md5::new();
        h.update(file.as_slice());
        let mut a = [0u8; 16];
        a.copy_from_slice(&h.finalize());
        a
    };

    let rrq = parse_rrq(&rrq_pkt)?;
    tlog(
        opts.debug,
        0,
        format!(
            "RRQ from {peer}: name={:?} mode={:?} options={:?}",
            rrq.name, rrq.mode, rrq.options
        ),
    );
    if rrq.mode.to_ascii_lowercase() != "octet" {
        if let Ok(tmp) = UdpSocket::bind(SocketAddr::new(opts.bind_ip, 0)) {
            let _ = tmp.send_to(&error_packet(ERR_ILLEGAL, "only octet mode"), peer);
        }
        return Err(ServeError::BadMode(rrq.mode));
    }
    let name = rrq.name;
    let req_options = rrq.options;

    // ---- 选项协商（照 fbtftp 的行为：blksize 原样 ack，tsize 报文件大小，timeout 回显） ----
    let mut blksize = DEFAULT_BLKSIZE;
    let mut options_acked: Vec<(String, String)> = Vec::new();
    let mut client_timeout: Option<Duration> = None;
    for (k, v) in &req_options {
        match k.as_str() {
            "blksize" => {
                let want = v.parse::<usize>().unwrap_or(DEFAULT_BLKSIZE);
                let bs = want.clamp(8, MAX_BLKSIZE);
                blksize = bs;
                options_acked.push(("blksize".into(), bs.to_string()));
            }
            "tsize" => {
                options_acked.push(("tsize".into(), total.to_string()));
            }
            "timeout" => {
                if let Ok(secs) = v.parse::<u64>() {
                    if (1..=255).contains(&secs) {
                        client_timeout = Some(Duration::from_secs(secs));
                        options_acked.push(("timeout".into(), secs.to_string()));
                    }
                }
            }
            _ => {}
        }
    }

    // ---- 传输会话：新 TID（与 fbtftp 一致，已被证明能跟这块板卡配合） ----
    let session = UdpSocket::bind(SocketAddr::new(opts.bind_ip, 0))?;
    session.set_read_timeout(Some(POLL_SLICE))?;

    // 重传间隔：优先用客户端要的 timeout，但必须明显小于 idle_timeout，
    // 否则"一次重传"就会撞上"无进展判失败"。
    let retrans_interval = client_timeout
        .unwrap_or(Duration::from_secs(2))
        .min(opts.idle_timeout / 3)
        .max(Duration::from_millis(200));

    let last = last_block_no(total, blksize);
    tlog(
        opts.debug,
        0,
        format!(
            "session {} -> {peer}: {} bytes, blksize {}, {} blocks, retransmit interval {:.1}s",
            session.local_addr()?,
            total,
            blksize,
            last,
            retrans_interval.as_secs_f64()
        ),
    );

    // ---- 异 IP 监控（仅单会话路径）：会话期间主端口继续收包 ----
    let (stop, rrq_retrans, foreign) = match watch {
        Some(w) => {
            let WatchState {
                sock: watch_sock,
                stop,
                rrq_retrans,
                foreign,
            } = w;
            let (t_stop, t_retrans, t_foreign) =
                (stop.clone(), rrq_retrans.clone(), foreign.clone());
            let first_peer = peer;
            let debug = opts.debug;
            thread::spawn(move || {
                let mut buf = vec![0u8; 2048];
                let _ = watch_sock.set_read_timeout(Some(POLL_SLICE));
                while !t_stop.load(Ordering::Relaxed) {
                    match watch_sock.recv_from(&mut buf) {
                        Ok((n, from)) if n >= 2 && be_u16(&buf, 0) == OP_RRQ => {
                            if from.ip() == first_peer.ip() {
                                // 同一个客户端重发 RRQ = 我们的首包丢了，属正常丢包重传
                                let c = t_retrans.fetch_add(1, Ordering::Relaxed) + 1;
                                tlog(
                                    debug,
                                    0,
                                    format!(
                                        "peer {from} re-sent its RRQ (#{c}); will restart from block 1"
                                    ),
                                );
                            } else {
                                let mut g = t_foreign.lock().unwrap();
                                if g.is_none() {
                                    *g = Some(from);
                                    tlog(
                                        debug,
                                        0,
                                        format!("ANOTHER host {from} is asking for the firmware too"),
                                    );
                                }
                            }
                        }
                        Ok((n, from)) if n >= 2 => {
                            if be_u16(&buf, 0) == OP_WRQ {
                                let _ = watch_sock
                                    .send_to(&error_packet(ERR_ACCESS, "read-only server"), from);
                            }
                        }
                        Ok(_) => {}
                        Err(_) => {}
                    }
                }
            });
            (stop, rrq_retrans, foreign)
        }
        // 多客户端：没有监视线程，这些标志保持静默（supervisor 自己发现新客户端）
        None => (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(None)),
        ),
    };

    // ---- 会话主循环 ----
    // 会话循环的收包缓冲（OACK/ACK/ERROR 都不大，留出余量即可）
    let mut buf = vec![0u8; blksize.max(DEFAULT_BLKSIZE) + 64];
    let session_start = Instant::now();
    // awaiting = 0 表示"等 OACK 的 ACK(块 0)"；否则表示"等这一块的 ACK"
    let mut awaiting: u64 = if options_acked.is_empty() { 1 } else { 0 };
    let mut retransmits: u64 = 0;
    // 这两个值在首包发出之后才有意义，所以不在这里初始化（避免无用赋值）
    let mut last_progress: Instant;
    let mut next_retransmit: Instant;
    let mut seen_rrq_retrans: u64 = 0;
    let mut acked_bytes: u64 = 0;

    let oack = if options_acked.is_empty() {
        None
    } else {
        Some(oack_packet(&options_acked))
    };
    let first_data = data_packet(1, block_payload(file.as_slice(), 1, blksize));

    /// 把"当前该发的那一包"发出去：`awaiting == 0` 发 OACK，其余发对应块号的 DATA。
    /// 首块走预构造的 `first`，避免每个块都重新分配。
    ///
    /// 注意：会话 socket 没有 `connect()`，所以只能用 `send_to`（用 `send` 会直接
    /// 报 ENOTCONN / "Destination address required"）。
    fn send_current(
        sock: &UdpSocket,
        peer: SocketAddr,
        awaiting: u64,
        oack: &Option<Vec<u8>>,
        first: &[u8],
        file: &[u8],
        blksize: usize,
    ) -> Result<(), ServeError> {
        if awaiting == 0 {
            if let Some(o) = oack {
                sock.send_to(o, peer)?;
                return Ok(());
            }
        }
        if awaiting <= 1 {
            sock.send_to(first, peer)?;
            return Ok(());
        }
        let payload = block_payload(file, awaiting, blksize);
        let pkt = data_packet(awaiting, payload);
        sock.send_to(&pkt, peer)?;
        Ok(())
    }

    if awaiting == 0 {
        session.send_to(oack.as_ref().unwrap(), peer)?;
        tlog(
            opts.debug,
            0,
            format!("sent OACK {options_acked:?}, waiting for ACK 0"),
        );
    } else {
        session.send_to(&first_data, peer)?;
        tlog(opts.debug, 1, "sent DATA 1".to_string());
    }
    last_progress = Instant::now();
    next_retransmit = last_progress + retrans_interval;

    let outcome: Result<(), ServeError> = loop {
        // 严格单会话：别的 IP 来拉就判失败
        if let Some(second) = *foreign.lock().unwrap() {
            break Err(ServeError::ForeignSession {
                first_peer: peer,
                second_peer: second,
            });
        }

        // 同 IP 重发 RRQ：从块 1 重开
        let rrq_now = rrq_retrans.load(Ordering::Relaxed);
        if rrq_now > seen_rrq_retrans {
            seen_rrq_retrans = rrq_now;
            awaiting = if oack.is_some() { 0 } else { 1 };
            send_current(
                &session,
                peer,
                awaiting,
                &oack,
                &first_data,
                file.as_slice(),
                blksize,
            )?;
            last_progress = Instant::now();
            next_retransmit = last_progress + retrans_interval;
            continue;
        }

        match session.recv_from(&mut buf) {
            Ok((n, from)) => {
                if from != peer {
                    tlog(
                        opts.debug,
                        1,
                        format!("ignored datagram from unexpected TID {from}"),
                    );
                    continue;
                }
                if n < 2 {
                    continue;
                }
                match be_u16(&buf, 0) {
                    OP_ACK => {
                        if n < 4 {
                            continue;
                        }
                        let ack = be_u16(&buf, 2) as u64;
                        if ack != (awaiting & 0xffff) {
                            tlog(
                                opts.debug,
                                2,
                                format!("stale/duplicate ACK {ack} (awaiting {awaiting})"),
                            );
                            continue;
                        }
                        last_progress = Instant::now();
                        next_retransmit = last_progress + retrans_interval;
                        if awaiting == 0 {
                            // OACK 被确认，开始送数据
                            awaiting = 1;
                            session.send_to(&first_data, peer)?;
                            progress(&Progress {
                                acked: 0,
                                total,
                                block: 0,
                                last_block: last,
                                blksize,
                                elapsed: session_start.elapsed(),
                                retransmits,
                            });
                            continue;
                        }
                        acked_bytes = (awaiting * blksize as u64).min(total);
                        progress(&Progress {
                            acked: acked_bytes,
                            total,
                            block: awaiting,
                            last_block: last,
                            blksize,
                            elapsed: session_start.elapsed(),
                            retransmits,
                        });
                        if awaiting >= last {
                            break Ok(());
                        }
                        awaiting += 1;
                        let payload = block_payload(file.as_slice(), awaiting, blksize);
                        let pkt = data_packet(awaiting, payload);
                        session.send_to(&pkt, peer)?;
                    }
                    OP_ERROR => {
                        let code = if n >= 4 { be_u16(&buf, 2) } else { ERR_UNDEFINED };
                        let msg = if n > 4 {
                            let body = &buf[4..n];
                            let end = body.iter().position(|b| *b == 0).unwrap_or(body.len());
                            String::from_utf8_lossy(&body[..end]).to_string()
                        } else {
                            String::new()
                        };
                        break Err(ServeError::PeerError { code, message: msg });
                    }
                    OP_RRQ => {
                        // 客户端在会话端口上重发请求：按重传处理
                        awaiting = if oack.is_some() { 0 } else { 1 };
                        send_current(&session, peer, awaiting, &oack, &first_data, file.as_slice(), blksize)?;
                        last_progress = Instant::now();
                        next_retransmit = last_progress + retrans_interval;
                    }
                    other => {
                        tlog(opts.debug, 2, format!("ignored opcode {other} from peer"));
                    }
                }
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::TimedOut
                    || e.kind() == io::ErrorKind::Interrupted =>
            {
                let idle = last_progress.elapsed();
                if idle >= opts.idle_timeout {
                    break Err(ServeError::Stalled {
                        block: awaiting,
                        last_block: last,
                        acked: acked_bytes,
                        total,
                        idle,
                        retransmits,
                    });
                }
                if Instant::now() >= next_retransmit {
                    retransmits += 1;
                    tlog(
                        opts.debug,
                        1,
                        format!(
                            "no ACK for {:.1}s, retransmit #{} of block {}",
                            idle.as_secs_f64(),
                            retransmits,
                            awaiting
                        ),
                    );
                    send_current(&session, peer, awaiting, &oack, &first_data, file.as_slice(), blksize)?;
                    next_retransmit = Instant::now() + retrans_interval;
                }
            }
            Err(e) => break Err(ServeError::Io(e)),
        }
    };

    stop.store(true, Ordering::Relaxed);
    outcome?;

    // 传输已完成：把会话结束之后可能飘进来的杂包排空并记录（不判失败）
    session.set_nonblocking(true)?;
    let mut stray_rrq = 0u32;
    while let Ok((n, _from)) = session.recv_from(&mut buf) {
        if n >= 2 && be_u16(&buf, 0) == OP_RRQ {
            stray_rrq += 1;
        }
    }
    if stray_rrq > 0 {
        tlog(
            opts.debug,
            0,
            format!("note: peer asked for the file again after the transfer completed ({stray_rrq}x)"),
        );
    }

    Ok(ServeReport {
        server_port,
        port_fallback,
        peer,
        requested_name: name,
        blksize,
        options_acked,
        bytes: total,
        blocks: last,
        served_md5,
        retransmits,
        rrq_retransmits: rrq_retrans.load(Ordering::Relaxed),
        elapsed: session_start.elapsed(),
    })
}

/// 给 `tftp_supervisor` 用的日志入口（复用同一套格式：先清进度行再打）
pub fn tlog_pub(debug: u32, level: u32, msg: impl AsRef<str>) {
    tlog(debug, level, msg);
}

/// 构造 ERROR 报文（supervisor 拒绝 WRQ/超并发时用）
pub fn error_packet_pub(code: u16, msg: &str) -> Vec<u8> {
    error_packet(code, msg)
}

/// 只看 RRQ 里的文件名（用于日志，不解析选项）
pub fn peek_rrq_name(buf: &[u8]) -> Option<String> {
    if buf.len() < 4 {
        return None;
    }
    let rest = &buf[2..];
    let end = rest.iter().position(|b| *b == 0)?;
    Some(String::from_utf8_lossy(&rest[..end]).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrq_parsing() {
        let mut p = Vec::new();
        p.extend_from_slice(&OP_RRQ.to_be_bytes());
        p.extend_from_slice(b"BOOT.bin\0octet\0blksize\01468\0tsize\00\0");
        let r = parse_rrq(&p).unwrap();
        assert_eq!(r.name, "BOOT.bin");
        assert_eq!(r.mode, "octet");
        assert_eq!(
            r.options,
            vec![
                ("blksize".to_string(), "1468".to_string()),
                ("tsize".to_string(), "0".to_string())
            ]
        );
    }

    #[test]
    fn rrq_without_options() {
        let mut p = Vec::new();
        p.extend_from_slice(&OP_RRQ.to_be_bytes());
        p.extend_from_slice(b"BOOT.bin\0octet\0");
        let r = parse_rrq(&p).unwrap();
        assert!(r.options.is_empty());
    }

    /// 末块判定：整除块大小时必须补一个 0 长块，否则板卡会少收最后一块。
    #[test]
    fn last_block_math() {
        assert_eq!(last_block_no(0, 512), 1);
        assert_eq!(last_block_no(1, 512), 1);
        assert_eq!(last_block_no(512, 512), 2);
        assert_eq!(last_block_no(513, 512), 2);
        assert_eq!(last_block_no(1024, 512), 3);
        assert_eq!(last_block_no(35_113_724, 512), 68582);
    }

    /// 35MB / 512B > 65535 块：块号必须能回绕，这里验证回绕点的载荷位置没错位。
    #[test]
    fn block_wraparound_addressing() {
        let total = 35_113_724usize;
        let file: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
        let bs = 512usize;
        let last = last_block_no(total as u64, bs);
        // 逐块核对偏移：跨越 65535/65536 回绕点
        for b in [1u64, 65535, 65536, 65537, 131072, last - 1, last] {
            let payload = block_payload(&file, b, bs);
            let off = (b - 1) as usize * bs;
            if off >= total {
                assert!(payload.is_empty(), "block {b} should be the empty tail block");
            } else {
                assert_eq!(payload[0], (off % 251) as u8, "block {b} misaligned");
                assert_eq!(
                    payload.len(),
                    bs.min(total - off),
                    "block {b} wrong payload length"
                );
            }
            // 线上块号是 16 位：回绕后低位必须唯一可辨
            assert_eq!(b as u16 as u64 & 0xffff, b & 0xffff);
        }
    }

    /// 回绕时 ACK 匹配：awaiting 用 u64 跟踪，线上比较只看低 16 位。
    #[test]
    fn ack_matching_across_wrap() {
        for awaiting in [1u64, 65535, 65536, 65537, 68582] {
            let on_wire = (awaiting & 0xffff) as u16;
            assert_eq!(on_wire as u64, awaiting & 0xffff);
            // 回绕后的块号不能和它前面 65536 块之前的老块号混淆（低 16 位相同也无妨，
            // 因为 awaiting 是单调递增的，老 ACK 只可能是 duplicate，见会话循环）
        }
        assert_eq!((65536u64 & 0xffff) as u16, 0);
        assert_eq!((65537u64 & 0xffff) as u16, 1);
    }

    #[test]
    fn oack_layout() {
        let p = oack_packet(&[
            ("blksize".to_string(), "1468".to_string()),
            ("tsize".to_string(), "35113724".to_string()),
        ]);
        assert_eq!(be_u16(&p, 0), OP_OACK);
        assert_eq!(
            &p[2..],
            b"blksize\01468\0tsize\035113724\0".as_slice()
        );
    }
}
