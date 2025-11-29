use std::{
    fs::File,
    net::{SocketAddrV4, UdpSocket},
    path::Path,
    thread::JoinHandle,
    time::Duration,
};

use serde_yaml::from_reader;

use crossbeam::channel::{Receiver, Sender, bounded};
use lockfree_object_pool::LinearOwnedReusable;
use sdaa_ctrl::ctrl_msg::{CmdReplySummary, CtrlMsg, send_cmd};

use crate::{
    payload::Payload,
    pipeline::{RecvCmd, recv_pkt},
};

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

    pub fn init_device<P: AsRef<Path>>(&self, file_path: P) {
        let cmds: Vec<CtrlMsg> =
            from_reader(File::open(file_path).expect("file not open")).expect("failed to load cmd");
        for cmd in cmds {
            println!("sending cmd:");
            println!("{:?}", cmd);
            self.send_cmd(cmd);
        }
    }

    pub fn set_mixer_freq(&self, freq_mega_hz: f64, sync: u32) -> CmdReplySummary {
        let cmd = CtrlMsg::MixerSet { msg_id: 0, freq: -freq_mega_hz, phase: 0.0, sync: sync } ;
        self.send_cmd(cmd)
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
}

impl Drop for Sdr {
    fn drop(&mut self) {
        eprintln!("dropped");
        self.ctrl.stream_stop();
        let h = self.rx_thread.take();
        if let Some(h1) = h
            && let Ok(()) = h1.join()
        {}
    }
}

impl Sdr {
    #[allow(clippy::type_complexity)]
    pub fn new<P: AsRef<Path>>(
        remote_ctrl_addr: SocketAddrV4,
        local_ctrl_addr: SocketAddrV4,
        local_payload_addr: SocketAddrV4,
        init_file: P,
    ) -> (Sdr, Receiver<LinearOwnedReusable<Payload>>, Sender<RecvCmd>) {
        let ctrl = SdrCtrl {
            remote_ctrl_addr,
            local_ctrl_addr,
        };

        ctrl.init_device(init_file);

        let payload_socket =
            UdpSocket::bind(local_payload_addr).expect("failed to bind payload socket");

        send_cmd(
            CtrlMsg::StreamStop { msg_id: 0 },
            &[remote_ctrl_addr],
            local_ctrl_addr,
            Some(Duration::from_secs(10)),
            1,
        );
        let (tx_payload, rx_payload) = bounded::<LinearOwnedReusable<Payload>>(8192);
        let (tx_recv_cmd, rx_recv_cmd) = bounded::<RecvCmd>(32);
        let rx_thread =
            std::thread::spawn(|| recv_pkt(payload_socket.into(), tx_payload, rx_recv_cmd));
        (
            Sdr {
                rx_thread: Some(rx_thread),
                ctrl,
            },
            rx_payload,
            tx_recv_cmd,
        )
    }
}
