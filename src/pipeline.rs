use std::net::SocketAddrV4;
use std::time::{Duration, Instant};
use std::{
    net::{Ipv4Addr, UdpSocket},
    ops::Deref,
    sync::Arc,
};

use chrono::Local;
use crossbeam::channel::{Receiver, Sender};
use lockfree_object_pool::{LinearObjectPool, LinearOwnedReusable};
use rustfft::num_complex::Complex;

use crate::{
    payload::{N_PT_PER_FRAME, Payload},
    utils::as_mut_u8_slice,
};

pub struct MaybeMulticastReceiver {
    socket: UdpSocket,
    group_and_iface: Option<(Ipv4Addr, Ipv4Addr)>, // (group, iface)
}

impl MaybeMulticastReceiver {
    pub fn new(
        bind_addr: SocketAddrV4,
        group_and_iface: Option<(Ipv4Addr, Ipv4Addr)>,
    ) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(bind_addr)?;

        if let Some((group, iface)) = group_and_iface {
            socket.join_multicast_v4(&group, &iface)?;
        }

        Ok(Self {
            socket,
            group_and_iface,
        })
    }
}

impl Drop for MaybeMulticastReceiver {
    fn drop(&mut self) {
        if let Some((group, iface)) = self.group_and_iface {
            let _ = self.socket.leave_multicast_v4(&group, &iface);
            println!("Left multicast group {} on interface {}", group, iface);
        }
    }
}

impl Deref for MaybeMulticastReceiver {
    type Target = UdpSocket;
    fn deref(&self) -> &Self::Target {
        &self.socket
    }
}

impl From<UdpSocket> for MaybeMulticastReceiver {
    fn from(socket: UdpSocket) -> Self {
        Self {
            socket,
            group_and_iface: None,
        }
    }
}

pub enum RecvCmd {
    Destroy,
}

pub fn fake_dev(tx_payload: Sender<LinearOwnedReusable<Payload>>, rx_cmd: Receiver<RecvCmd>) {
    let mut last_print_time = Instant::now();
    let t0 = Instant::now();
    let print_interval = Duration::from_secs(2);

    let pool: Arc<LinearObjectPool<Payload>> = Arc::new(LinearObjectPool::new(
        move || {
            //eprint!("o");
            Payload::default()
        },
        |v| {
            v.pkt_cnt = 0;
            v.data.fill(Complex::default());
        },
    ));
    //socket.set_nonblocking(true).unwrap();
    for pkt_cnt in 0.. {
        if !rx_cmd.is_empty() {
            match rx_cmd.recv().expect("failed to recv cmd") {
                RecvCmd::Destroy => break,
            }
        }
        let mut payload = pool.pull_owned();
        payload.pkt_cnt = pkt_cnt;

        let now = Instant::now();

        if payload.pkt_cnt == 0 {
            let local_time = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            println!();
            println!("==================================");
            println!("start time:{local_time}");
            println!("==================================");
        } else if now.duration_since(last_print_time) >= print_interval {
            let dt = now.duration_since(t0).as_secs_f64();
            let npkts = pkt_cnt as usize;
            let nsamp = npkts * N_PT_PER_FRAME;
            let smp_rate = nsamp as f64 / dt;
            println!("smp_rate: {} MSps q={}", smp_rate / 1e6, tx_payload.len());
            last_print_time = now;
        }

        if tx_payload.send(payload).is_err() {
            return;
        }
    }
}

pub fn recv_pkt(
    socket: MaybeMulticastReceiver,
    tx_payload: Sender<LinearOwnedReusable<Payload>>,
    rx_cmd: Receiver<RecvCmd>,
) {
    let mut last_print_time = Instant::now();
    let print_interval = Duration::from_secs(2);

    let mut next_cnt = None;
    let mut ndropped = 0;
    let mut nreceived = 0;
    let pool: Arc<LinearObjectPool<Payload>> = Arc::new(LinearObjectPool::new(
        move || {
            //eprint!("o");
            Payload::default()
        },
        |v| {
            v.pkt_cnt = 0;
            v.data.fill(Complex::default());
        },
    ));
    //socket.set_nonblocking(true).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("failed to set timeout");
    loop {
        if !rx_cmd.is_empty() {
            match rx_cmd.recv().expect("failed to recv cmd") {
                RecvCmd::Destroy => break,
            }
        }
        let mut payload = pool.pull_owned();
        let buf = as_mut_u8_slice(&mut payload as &mut Payload);
        match socket.recv_from(buf) {
            Ok((s, _a)) => {
                if s != std::mem::size_of::<Payload>() {
                    continue;
                }
            }
            _ => continue,
        }

        let now = Instant::now();

        if now.duration_since(last_print_time) >= print_interval {
            let local_time = Local::now().format("%Y-%m-%d %H:%M:%S");
            println!(
                "{local_time} {ndropped} pkts dropped q={} ratio<{:e}",
                tx_payload.len(),
                (1 + ndropped) as f64 / nreceived as f64
            );
            last_print_time = now;
        }

        if next_cnt.is_none() {
            next_cnt = Some(payload.pkt_cnt);
            ndropped = 0;
        }

        if payload.pkt_cnt == 0 {
            ndropped = 0;
            nreceived = 0;
            let local_time = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            println!();
            println!("==================================");
            println!("start time:{local_time}");
            println!("==================================");
        }

        while let Some(ref mut c) = next_cnt {
            //let current_cnt = c + 1;
            if *c >= payload.pkt_cnt {
                //actually = is sufficient.
                *c = payload.pkt_cnt + 1;
                if tx_payload.is_full() {
                    //eprint!("O");
                    if !rx_cmd.is_empty() {
                        match rx_cmd.recv().expect("failed to recv cmd") {
                            RecvCmd::Destroy => return,
                        }
                    }
                    continue;
                }
                nreceived += 1;
                if let Ok(()) = tx_payload.send(payload) {
                    break;
                } else {
                    return;
                }
            }

            ndropped += 1;

            let mut payload1 = pool.pull_owned();
            payload1.copy_header(&payload);
            payload1.pkt_cnt = *c;
            if tx_payload.is_full() {
                //eprint!("O");
                if !rx_cmd.is_empty() {
                    match rx_cmd.recv().expect("failed to recv cmd") {
                        RecvCmd::Destroy => return,
                    }
                }
                continue;
            }
            nreceived += 1;
            if tx_payload.send(payload1).is_err() {
                return;
            }

            *c += 1;
        }
    }
}

