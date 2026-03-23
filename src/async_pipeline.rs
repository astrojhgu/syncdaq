use async_stream::stream;
use chrono::Local;
use futures_core::Stream;
use lockfree_object_pool::{LinearObjectPool, LinearOwnedReusable};

use std::{
    sync::Arc,
    net::{Ipv4Addr, SocketAddrV4},
    ops::Deref,
    time::{Duration, Instant},
};
use tokio::net::UdpSocket;

use crate::{payload::{N_BYTE_PER_FRAME, Payload}, utils::as_mut_u8_slice};

pub struct MaybeMulticastReceiver {
    socket: UdpSocket,
    group_and_iface: Option<(Ipv4Addr, Ipv4Addr)>, // (group, iface)
}

impl MaybeMulticastReceiver {
    pub async fn new(
        bind_addr: SocketAddrV4,
        group_and_iface: Option<(Ipv4Addr, Ipv4Addr)>,
    ) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(bind_addr).await?;

        if let Some((group, iface)) = group_and_iface {
            socket.join_multicast_v4(group, iface)?;
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
            let _ = self.socket.leave_multicast_v4(group, iface);
            println!("Left multicast group {group} on interface {iface}");
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

pub fn recv_pkt<T>(
    socket: MaybeMulticastReceiver,
) -> impl Stream<Item = LinearOwnedReusable<Payload<T>>> 
where T: Sized+Default+Copy+'static,
[T; N_BYTE_PER_FRAME / std::mem::size_of::<T>()]: Sized,
{
    let mut last_print_time = Instant::now();
    let print_interval = Duration::from_secs(2);

    let mut next_cnt = None;
    let mut ndropped = 0;
    let mut nreceived = 0;
    let pool: Arc<LinearObjectPool<Payload<T>>> = Arc::new(LinearObjectPool::new(
        move || {
            //eprint!("o");
            Payload::<T>::default()
        },
        |v| {
            v.pkt_cnt = 0;
            v.data.fill(T::default());
        },
    ));
    //socket.set_nonblocking(true).unwrap();

    stream! {
        loop {
            let mut payload = pool.pull_owned();
            let buf = as_mut_u8_slice(&mut payload as &mut Payload<T>);

            match socket.recv_from(buf).await {
                Ok((s, _a)) => {
                    if s != std::mem::size_of::<Payload<T>>() {
                        continue;
                    }
                }
                _ => continue,
            }

            let now = Instant::now();

            if now.duration_since(last_print_time) >= print_interval {
                let local_time = Local::now().format("%Y-%m-%d %H:%M:%S");
                println!(
                    "{local_time} {ndropped} pkts dropped, ratio<{:e}",
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

                    nreceived += 1;
                    yield payload;
                    break;
                }

                ndropped += 1;

                let mut payload1 = pool.pull_owned();
                payload1.copy_header(&payload);
                payload1.pkt_cnt = *c;

                nreceived += 1;
                yield payload1;
                *c += 1;

                if ndropped % 1000 == 0 {
                    tokio::task::yield_now().await;
                }
            }
        }
    }
}
