//! 多客户端 TFTP 服务端：在**同一个**监听端口上并发服务多块板卡。
//!
//! 分工：`tftp_server` 的单会话引擎负责"怎么把一个会话完整送完"（分块、选项协商、
//! 重传、末块判定、块号回绕）；这里只负责调度：
//!
//!   * 主端口持续收 RRQ，每个新对端 fork 一个会话（各自新 TID，与 fbtftp 一致）；
//!   * 并发上限（超过就回 ERROR，并记入 `rejected`）、静默窗口、全程超时；
//!   * 所有会话共享**同一份**内存镜像（`Arc<Vec<u8>>`）：各板卡拿到的字节完全一致、
//!     md5 天然统一，也不会为 N 块板卡各拷一份 35MB；
//!   * 聚合进度（总字节 / 总目标 / 完成数）与逐会话结果。
//!
//! 与单会话路径的差别：这里**不会**因为"第二个客户端来拉"而判失败——那正是它的用途。
//! 同 IP 重复 RRQ 视为丢包重传（会话自己会重传并继续）；已完成的对端再次请求会被
//! 当成新会话（记一条日志），不复用旧会话。

use crate::tftp_server::{self, ServeOptions, Server};
/// 复用单会话引擎的错误类型（调用方按同一套语义判断"一条 RRQ 都没来"等致命情况）
pub use crate::tftp_server::ServeError;
use std::{
    net::{IpAddr, SocketAddr, UdpSocket},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

/// 主循环轮询粒度：每片醒一次，检查新 RRQ / 完成条件 / 超时
const POLL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone)]
pub struct MultiOptions {
    /// 要送出去的文件（多客户端共享同一份镜像）
    pub path: PathBuf,
    pub bind_ip: IpAddr,
    /// 期望端口（被占则退让）；被写进 FetchFW 的就是它
    pub port: u16,
    /// 等第一条 RRQ 的上限
    pub rrq_timeout: Duration,
    /// 每个会话"无进展"上限
    pub idle_timeout: Duration,
    /// 并发上限（=子网上板卡数的合理上界）
    pub max_sessions: usize,
    /// 所有已知会话都完成后，再静默这么久没有新 RRQ 就收工
    pub quiet_window: Duration,
    /// 全程上限（None = 不限）
    pub overall_timeout: Option<Duration>,
    pub debug: u32,
}

impl MultiOptions {
    fn to_serve_options(&self, preload: Option<Arc<Vec<u8>>>, port: u16) -> ServeOptions {
        ServeOptions {
            path: self.path.clone(),
            bind_ip: self.bind_ip,
            port,
            rrq_timeout: self.rrq_timeout,
            idle_timeout: self.idle_timeout,
            preload,
            debug: self.debug,
        }
    }
}

/// 一个会话的最终结果（`completed == false` 表示"传起来了但没传完"）
#[derive(Debug, Clone)]
pub struct SessionOutcome {
    pub peer: SocketAddr,
    pub requested_name: String,
    pub blksize: usize,
    pub bytes: u64,
    pub blocks: u64,
    pub retransmits: u64,
    pub elapsed: Duration,
    pub completed: bool,
    pub note: Option<String>,
}

#[derive(Debug)]
pub struct MultiReport {
    pub port: u16,
    pub port_fallback: bool,
    /// 真正送出去的镜像的 md5（所有会话共用，SwitchFW 必须用它）
    pub served_md5: [u8; 16],
    pub image_bytes: u64,
    pub sessions: Vec<SessionOutcome>,
    /// 因超过并发上限被拒的对端
    pub rejected: Vec<SocketAddr>,
    pub elapsed: Duration,
}

impl MultiReport {
    /// 完整传完的会话数
    pub fn completed(&self) -> usize {
        self.sessions.iter().filter(|s| s.completed).count()
    }

    /// 已经完整传完的那些板卡（即"可以发 SwitchFW"的候选）
    pub fn completed_peers(&self) -> Vec<SocketAddr> {
        self.sessions
            .iter()
            .filter(|s| s.completed)
            .map(|s| s.peer)
            .collect()
    }
}

/// 聚合进度快照（并发会话的总和）
#[derive(Debug, Clone, Copy)]
pub struct MultiProgress {
    pub started: usize,
    pub done: usize,
    pub ok: usize,
    pub failed: usize,
    pub acked_bytes: u64,
    pub target_bytes: u64,
    pub elapsed: Duration,
}

struct Slot {
    peer: SocketAddr,
    acked: Arc<AtomicU64>,
    done: bool,
}

/// 已绑定主端口的 supervisor。
pub struct Supervisor {
    inner: Server,
}

/// 绑主端口（被占用时退让），端口会被写进 FetchFW。
pub fn bind(opts: &MultiOptions) -> Result<Supervisor, ServeError> {
    // 只是借 tftp_server 的绑定逻辑；preload 由各会话自己带上
    let inner = tftp_server::bind(&opts.to_serve_options(None, opts.port))?;
    Ok(Supervisor { inner })
}

impl Supervisor {
    /// 实际监听端口（写进 FetchFW 的那个）
    pub fn port(&self) -> u16 {
        self.inner.port()
    }

    pub fn port_fallback(&self) -> bool {
        self.inner.port_fallback()
    }

    /// 并发服务到"所有会话完成 + 静默窗口"或全程超时，返回逐会话结果。
    pub fn serve(
        self,
        opts: &MultiOptions,
        on_progress: &mut dyn FnMut(&MultiProgress),
    ) -> Result<MultiReport, ServeError> {
        let started_at = Instant::now();
        let port = self.inner.port();
        let port_fallback = self.inner.port_fallback();

        // 镜像只读一次，所有会话共享（保证字节完全一致、md5 天然相同）
        let image = Arc::new(std::fs::read(&opts.path)?);
        let image_bytes = image.len() as u64;
        let served_md5 = {
            use md5::{Digest, Md5};
            let mut h = Md5::new();
            h.update(image.as_slice());
            let mut a = [0u8; 16];
            a.copy_from_slice(&h.finalize());
            a
        };

        let slots: Arc<Mutex<Vec<Slot>>> = Arc::new(Mutex::new(Vec::new()));
        let global_stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<(usize, SessionOutcome)>();
        let mut handles: Vec<thread::JoinHandle<()>> = Vec::new();
        let mut rejected: Vec<SocketAddr> = Vec::new();

        let main_sock: &UdpSocket = self.inner.socket();
        let first_deadline = Instant::now() + opts.rrq_timeout;
        let mut started_any = false;
        let mut last_request_at = Instant::now();
        let mut buf = vec![0u8; 2048];
        let mut outcomes: Vec<Option<SessionOutcome>> = Vec::new();

        let finish = |on_progress: &mut dyn FnMut(&MultiProgress),
                      slots: &Arc<Mutex<Vec<Slot>>>,
                      outcomes: &Vec<Option<SessionOutcome>>,
                      started_at: Instant,
                      force_done: Option<usize>| {
            let g = slots.lock().unwrap();
            let done = if let Some(f) = force_done { f } else { g.iter().filter(|s| s.done).count() };
            let acked: u64 = g.iter().map(|s| s.acked.load(Ordering::Relaxed)).sum();
            let ok = outcomes
                .iter()
                .filter(|o| o.as_ref().map(|o| o.completed).unwrap_or(false))
                .count();
            on_progress(&MultiProgress {
                started: g.len(),
                done,
                ok,
                failed: done.saturating_sub(ok),
                acked_bytes: acked,
                target_bytes: image_bytes * g.len() as u64,
                elapsed: started_at.elapsed(),
            });
        };

        loop {
            // 1) 收会话结果（会话收尾也算"有活动"，静默窗口从这里重新计时）
            while let Ok((idx, out)) = rx.try_recv() {
                last_request_at = Instant::now();
                {
                    let mut g = slots.lock().unwrap();
                    if let Some(s) = g.get_mut(idx) {
                        s.done = true;
                    }
                }
                // 每块板卡抓完（或失败）立刻给一行结果，不等全局收工
                if out.completed {
                    crate::tftp_server::tlog_pub(
                        opts.debug,
                        0,
                        format!(
                            "session #{idx} {} DONE: {} bytes in {:.1}s (blksize {}, {} blocks, {} retransmits)",
                            out.peer,
                            out.bytes,
                            out.elapsed.as_secs_f64(),
                            out.blksize,
                            out.blocks,
                            out.retransmits
                        ),
                    );
                } else {
                    crate::tftp_server::tlog_pub(
                        opts.debug,
                        0,
                        format!(
                            "session #{idx} {} FAILED: {}",
                            out.peer,
                            out.note.as_deref().unwrap_or("did not complete")
                        ),
                    );
                }
                if outcomes.len() <= idx {
                    outcomes.resize(idx + 1, None);
                }
                outcomes[idx] = Some(out);
            }

            // 2) 完成判据：所有已开会话都收尾，且静默窗口内没有新 RRQ
            let (all_done, count) = {
                let g = slots.lock().unwrap();
                (
                    !g.is_empty() && g.iter().all(|s| s.done),
                    g.len(),
                )
            };
            if all_done && last_request_at.elapsed() >= opts.quiet_window {
                crate::tftp_server::tlog_pub(
                    opts.debug,
                    0,
                    format!(
                        "all {count} session(s) finished; quiet for {:.1}s, stopping",
                        last_request_at.elapsed().as_secs_f64()
                    ),
                );
                break;
            }

            // 3) 全程超时
            if let Some(limit) = opts.overall_timeout {
                if started_at.elapsed() >= limit {
                    crate::tftp_server::tlog_pub(
                        opts.debug,
                        0,
                        format!(
                            "overall timeout {:.0}s reached; stopping with {} session(s) unfinished",
                            limit.as_secs_f64(),
                            slots.lock().unwrap().iter().filter(|s| !s.done).count()
                        ),
                    );
                    break;
                }
            }

            // 4) 一条 RRQ 都没等到
            if !started_any && Instant::now() >= first_deadline {
                global_stop.store(true, Ordering::Relaxed);
                return Err(ServeError::NoRequest {
                    waited: opts.rrq_timeout,
                    port,
                });
            }

            // 5) 收主端口（新客户端从这里来）
            let slice = if !started_any {
                first_deadline
                    .saturating_duration_since(Instant::now())
                    .min(POLL)
            } else {
                POLL
            };
            let _ = main_sock.set_read_timeout(Some(slice.max(Duration::from_millis(1))));
            match main_sock.recv_from(&mut buf) {
                Ok((n, from)) => {
                    if n < 2 {
                        continue;
                    }
                    match u16::from_be_bytes([buf[0], buf[1]]) {
                        1 => {
                            // RRQ
                            last_request_at = Instant::now();
                            let known = {
                                let g = slots.lock().unwrap();
                                g.iter().position(|s| s.peer == from)
                            };
                            if let Some(_idx) = known {
                                // 同一个对端再发 RRQ：丢包重传（会话自己会继续），也可能是
                                // 它已经完成后的二次请求 —— 两种情况都不再开新会话。
                                crate::tftp_server::tlog_pub(
                                    opts.debug,
                                    0,
                                    format!("peer {from} re-sent RRQ; its session keeps running"),
                                );
                                continue;
                            }
                            let g_len = slots.lock().unwrap().len();
                            if g_len >= opts.max_sessions {
                                let _ = main_sock.send_to(
                                    &crate::tftp_server::error_packet_pub(0, "server busy"),
                                    from,
                                );
                                rejected.push(from);
                                crate::tftp_server::tlog_pub(
                                    opts.debug,
                                    0,
                                    format!(
                                        "REFUSED {from}: concurrency limit {} reached",
                                        opts.max_sessions
                                    ),
                                );
                                continue;
                            }
                            // 开新会话
                            let idx = g_len;
                            let acked = Arc::new(AtomicU64::new(0));
                            slots.lock().unwrap().push(Slot {
                                peer: from,
                                acked: acked.clone(),
                                done: false,
                            });
                            started_any = true;
                            last_request_at = Instant::now();
                            let session_opts = opts.to_serve_options(Some(image.clone()), 0);
                            let rrq = buf[..n].to_vec();
                            let tx = tx.clone();
                            let acked_cb = acked.clone();
                            let debug = opts.debug;
                            handles.push(thread::spawn(move || {
                                let outcome = run_session(
                                    &session_opts, rrq, from, acked_cb, debug,
                                );
                                let _ = tx.send((idx, outcome));
                            }));
                            crate::tftp_server::tlog_pub(
                                opts.debug,
                                0,
                                format!("accepted {from} as session #{idx}"),
                            );
                        }
                        2 => {
                            // WRQ：只读服务
                            let _ = main_sock.send_to(
                                &crate::tftp_server::error_packet_pub(2, "read-only server"),
                                from,
                            );
                        }
                        _ => {}
                    }
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut
                        || e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    global_stop.store(true, Ordering::Relaxed);
                    return Err(ServeError::Io(e));
                }
            }

            finish(on_progress, &slots, &outcomes, started_at, None);
        }

        // 收工：等会话线程退出（会话自身有 idle 超时，最多再等一个 idle_timeout）
        global_stop.store(true, Ordering::Relaxed);
        for h in handles {
            let _ = h.join();
        }
        while let Ok((idx, out)) = rx.try_recv() {
            if outcomes.len() <= idx {
                outcomes.resize(idx + 1, None);
            }
            outcomes[idx] = Some(out);
        }

        let sessions: Vec<SessionOutcome> = outcomes.into_iter().flatten().collect();
        Ok(MultiReport {
            port,
            port_fallback,
            served_md5,
            image_bytes,
            sessions,
            rejected,
            elapsed: started_at.elapsed(),
        })
    }
}

/// 跑一个会话：新 TID + 复用单会话引擎，返回逐会话结果（不再把"没传完"当致命错误）。
fn run_session(
    opts: &ServeOptions,
    rrq: Vec<u8>,
    peer: SocketAddr,
    acked: Arc<AtomicU64>,
    debug: u32,
) -> SessionOutcome {
    let name = crate::tftp_server::peek_rrq_name(&rrq).unwrap_or_else(|| "?".to_string());
    let srv = match tftp_server::bind(opts) {
        Ok(s) => s,
        Err(e) => {
            return SessionOutcome {
                peer,
                requested_name: name,
                blksize: 0,
                bytes: 0,
                blocks: 0,
                retransmits: 0,
                elapsed: Duration::ZERO,
                completed: false,
                note: Some(format!("cannot open session socket: {e}")),
            };
        }
    };
    let mut progress = |p: &crate::tftp_server::Progress| {
        acked.store(p.acked, Ordering::Relaxed);
    };
    match srv.serve_from_rrq(opts, rrq, peer, &mut progress) {
        Ok(rep) => SessionOutcome {
            peer,
            requested_name: rep.requested_name,
            blksize: rep.blksize,
            bytes: rep.bytes,
            blocks: rep.blocks,
            retransmits: rep.retransmits,
            elapsed: rep.elapsed,
            completed: true,
            note: None,
        },
        Err(e) => {
            crate::tftp_server::tlog_pub(
                debug,
                0,
                format!("session with {peer} did NOT complete: {e}"),
            );
            SessionOutcome {
                peer,
                requested_name: name,
                blksize: 0,
                bytes: 0,
                blocks: 0,
                retransmits: 0,
                elapsed: Duration::ZERO,
                completed: false,
                note: Some(e.to_string()),
            }
        }
    }
}
