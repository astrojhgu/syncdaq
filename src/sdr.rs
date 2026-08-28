use std::{
    fs::File,
    net::{SocketAddrV4, UdpSocket},
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    thread::JoinHandle,
    time::Duration,
};

use num::Complex;
use serde_yaml::from_reader;

use crossbeam::channel::{Receiver, bounded, unbounded};
use lockfree_object_pool::LinearOwnedReusable;

use crate::{
    ctrl_msg::{CmdReplySummary, CtrlMsg, send_cmd},
    firdecim2::{
        decim_pipeline::{start_decim_pipeline_chain, start_fir_pipeline},
        fir_coeffs::{fir_anti_aliasing_coeffs, fir_half_band_coeffs},
    },
    payload::{N_BYTE_PER_FRAME, Payload},
    pipeline::recv_pkt,
    utils::{claim_worker_core, pin_current_thread, pin_to_core, set_recv_buffer_size},
};

/// UDP 接收缓冲大小。默认 SO_RCVBUF 偏小，320 MSps 突发下容易丢包；
/// 丢包后 `recv_pkt` 会用全零补齐（`copy_header` 只拷头），形成时域整块零，
/// FFT 后表现为宽带干扰。在此显式放大。注意：值须能装进 `libc::c_int`（32 位），
/// 过大会溢出成负数导致 `setsockopt` 行为异常；内核仍会按 `net.core.rmem_max` 截断。
const RX_SOCKET_BUFFER: usize = 256 * 1024 * 1024;

pub struct SdrCtrl {
    pub remote_ctrl_addr: SocketAddrV4,
    pub local_ctrl_addr: SocketAddrV4,
}

impl SdrCtrl {
    pub fn send_cmd(&self, cmd: CtrlMsg) -> CmdReplySummary {
        send_cmd(
            cmd,
            &[self.remote_ctrl_addr],
            self.local_ctrl_addr,
            Some(Duration::from_secs(10)),
            1,
        )
    }

    pub fn query(&self) -> CmdReplySummary {
        let cmd = CtrlMsg::Query { msg_id: 0 };
        self.send_cmd(cmd)
    }

    pub fn init_device<P: std::fmt::Debug + AsRef<Path>>(&self, file_path: P) {
        let cmds: Vec<CtrlMsg> =
            from_reader(File::open(file_path).expect("file not open")).expect("failed to load cmd");
        for cmd in cmds {
            println!("sending cmd:");
            println!("{:?}", cmd);
            self.send_cmd(cmd);
        }
    }

    pub fn set_mixer_freq(&self, freq_mega_hz: f64, sync: u32) -> CmdReplySummary {
        if freq_mega_hz > -2000.0 && freq_mega_hz < 2000.0 {
            let cmd = CtrlMsg::MixerSet {
                msg_id: 0,
                nports: 8,
                freq: vec![-freq_mega_hz; 8],
                phase: vec![0.0; 8],
                sync: sync,
            };
            self.send_cmd(cmd)
        } else {
            panic!()
        }
    }

    pub fn stream_start(&self) -> CmdReplySummary {
        let cmd = CtrlMsg::StreamStart { msg_id: 0 };
        self.send_cmd(cmd)
    }

    pub fn stream_stop(&self) -> CmdReplySummary {
        println!("stopped");
        let cmd = CtrlMsg::StreamStop { msg_id: 0 };
        self.send_cmd(cmd)
    }
}

pub struct Sdr {
    rx_thread: Option<JoinHandle<()>>,
    pub ctrl: SdrCtrl,
    pub running: Arc<AtomicBool>,
}

impl Drop for Sdr {
    fn drop(&mut self) {
        eprintln!("dropped");
        self.ctrl.stream_stop();
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let h = self.rx_thread.take();
        if let Some(h1) = h
            && let Ok(()) = h1.join()
        {}
    }
}

impl Sdr {
    #[allow(clippy::type_complexity)]
    pub fn new<P, T>(
        remote_ctrl_addr: SocketAddrV4,
        local_ctrl_addr: SocketAddrV4,
        local_payload_addr: SocketAddrV4,
        init_file: Option<P>,
    ) -> (Sdr, Receiver<LinearOwnedReusable<Payload<T>>>)
    where
        P: std::fmt::Debug + AsRef<Path>,
        [T; N_BYTE_PER_FRAME / std::mem::size_of::<T>()]: Sized,
        T: Sized + Default + Copy + Send + Sync + 'static,
    {
        let ctrl = SdrCtrl {
            remote_ctrl_addr,
            local_ctrl_addr,
        };

        if let Some(init_file) = init_file {
            println!("init file: {init_file:?}");
            ctrl.init_device(init_file);
        }

        let payload_socket =
            UdpSocket::bind(local_payload_addr).expect("failed to bind payload socket");
        let _ = set_recv_buffer_size(&payload_socket, RX_SOCKET_BUFFER);

        send_cmd(
            CtrlMsg::StreamStop { msg_id: 0 },
            &[remote_ctrl_addr],
            local_ctrl_addr,
            Some(Duration::from_secs(10)),
            1,
        );
        let running = Arc::new(AtomicBool::new(true));
        let running1 = running.clone();
        let (tx_payload, rx_payload) = unbounded::<LinearOwnedReusable<Payload<T>>>();
        let rx_thread = std::thread::spawn(|| {
            pin_current_thread();
            recv_pkt(payload_socket.into(), tx_payload, running1)
        });
        (
            Sdr {
                rx_thread: Some(rx_thread),
                ctrl,
                running,
            },
            rx_payload,
        )
    }
}

pub struct Sdr16Decim {
    rx_threads: Option<Vec<JoinHandle<()>>>,
    pub ctrl: SdrCtrl,
    pub local_payload_addr: SocketAddrV4,
    pub running: Arc<AtomicBool>,
}

impl Drop for Sdr16Decim {
    fn drop(&mut self) {
        eprintln!("dropped");
        self.destroy_recv_thread();
        self.ctrl.stream_stop();
        // let h = self.rx_thread.take();
        // if let Some(h1) = h
        //     && let Ok(()) = h1.join()
        // {}
    }
}

impl Sdr16Decim {
    #[allow(clippy::type_complexity)]
    pub fn new<P>(
        remote_ctrl_addr: SocketAddrV4,
        local_ctrl_addr: SocketAddrV4,
        local_payload_addr: SocketAddrV4,
        init_file: Option<P>,
    ) -> Sdr16Decim
    where
        P: std::fmt::Debug + AsRef<Path>,
        //[T; N_BYTE_PER_FRAME / std::mem::size_of::<T>()]: Sized,
        //T: Sized + Default + Copy + Send + Sync + 'static,
    {
        let ctrl = SdrCtrl {
            remote_ctrl_addr,
            local_ctrl_addr,
        };

        if let Some(init_file) = init_file {
            println!("init file: {init_file:?}");
            ctrl.init_device(init_file);
        }

        send_cmd(
            CtrlMsg::StreamStop { msg_id: 0 },
            &[remote_ctrl_addr],
            local_ctrl_addr,
            Some(Duration::from_secs(10)),
            1,
        );
        let running = Arc::new(AtomicBool::new(true));

        Sdr16Decim {
            rx_threads: None,
            ctrl,
            local_payload_addr,
            running,
        }
    }

    pub fn destroy_recv_thread(&mut self) {
        //self.ctrl.stream_stop();
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let mut rx_threads = self.rx_threads.take();
        if let Some(ref mut rx_threads) = rx_threads {
            for h in rx_threads.drain(..) {
                if let Ok(()) = h.join() {
                    //eprintln!("rx thread joined");
                } else {
                    //eprintln!("failed to join rx thread");
                }
            }
        }
    }

    pub fn setup_stream(
        &mut self,
        decim_shifts: &[u32],
        anti_aliasing_shift: Option<u32>,
    ) -> Receiver<LinearOwnedReusable<Payload<Complex<i16>>>> {
        self.destroy_recv_thread();
        let payload_socket =
            UdpSocket::bind(self.local_payload_addr).expect("failed to bind payload socket");
        let _ = set_recv_buffer_size(&payload_socket, RX_SOCKET_BUFFER);

        let nbuf = 128;

        let (tx_payload, rx_payload) = unbounded::<LinearOwnedReusable<Payload<Complex<i16>>>>();
        self.running
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let running1 = self.running.clone();
        let rx_thread = std::thread::spawn(|| {
            // recv 线程也钉到专用核，避免被其它（如 GQRX 取数）线程挤占导致 UDP 丢包
            if let Some(core) = claim_worker_core() {
                pin_to_core(core);
            }
            recv_pkt::<Complex<i16>>(payload_socket.into(), tx_payload, running1)
        });
        let fir_coeffs = fir_half_band_coeffs();
        let (mut rx_threads, rx) =
            start_decim_pipeline_chain(rx_payload, &fir_coeffs, decim_shifts);
        rx_threads.push(rx_thread);

        let rx = if let Some(anti_aliasing_shift) = anti_aliasing_shift {
            let anti_aliasing_coeffs = fir_anti_aliasing_coeffs();
            let (tx1, rx1) = bounded::<LinearOwnedReusable<Payload<Complex<i16>>>>(nbuf);
            let rx_thread = start_fir_pipeline(rx, tx1, &anti_aliasing_coeffs, anti_aliasing_shift);
            rx_threads.push(rx_thread);
            rx1
        } else {
            rx
        };
        self.rx_threads = Some(rx_threads);
        rx
    }
}
