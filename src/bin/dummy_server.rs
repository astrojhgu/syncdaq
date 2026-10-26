use clap::Parser;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// config
    #[clap(short = 'a', long = "addr", num_args(1..), value_name="ip:port")]
    addr: String,
}

use binrw::{BinRead, BinWrite};
use std::{io::Cursor, net::UdpSocket};
use syncdaq::ctrl_msg::{
    CtrlMsg::{self, *},
    print_bytes,
};
fn main() {
    let args = Args::parse();
    let socket = UdpSocket::bind(args.addr).unwrap();
    socket.set_nonblocking(false).unwrap();
    let mut shifts = std::collections::BTreeMap::<u32, u32>::new();
    // dummy 没有 RFDC 硬件可读，只能保留最后一次 SetMixer 的值来冒充「当前值」。
    // 真板卡的 GetMixer 是每次调 XRFdc_GetMixerSettings() 现读硬件、不缓存，
    // 所以上电未设过时真板卡回的是 IP 默认 NCO，这里回的是全 0。
    let mut mixer_freq: Vec<f64> = vec![0.0; 8];
    let mut mixer_phase: Vec<f64> = vec![0.0; 8];
    loop {
        let mut buf = vec![0_u8; 9000];
        let (sz, addr) = socket.recv_from(&mut buf).unwrap();
        println!("received {sz} Bytes from {addr}");
        print_bytes(&buf[..sz]);
        let mut cursor = Cursor::new(buf);
        let msg = CtrlMsg::read(&mut cursor).unwrap();
        //println!("{msg:?}");
        println!("{msg}");

        let reply = match msg {
            Query { msg_id } => QueryReply {
                msg_id,
                fm_ver: 0x24122420,
                tick_cnt1: 10,
                tick_cnt2: 10,
                trans_state: 0,
                locked: 0,
                _z: 0x1fe,
                rfdc_restart_cnt: 0,
                temperature: 40.0,
                nports: 8,
                boot_mode: 3,
                uptime_s: 0,
                over_voltage_state: 0,
                over_range_state: 0,
                fan_pulse_cnt: 1000,
                smp_rate: 320,
                smp_bits: 16,
                iq_mode: 1,
                fifo_full_cnt: 0,
                pkt_cnt1: vec![0; 8],
                axi_frame_cnt1: vec![0; 8],
                pkt_cnt2: vec![0; 8],
                axi_frame_cnt2: vec![0; 8],
            },
            //QueryReply { msg_id } => *msg_id = mid,
            Sync { msg_id } => SyncReply { msg_id },
            //SyncReply { msg_id } => *msg_id = mid,
            XGbeCfg { msg_id, .. } => XgbeCfgReply { msg_id },
            //XgbeCfgReply { msg_id } => *msg_id = mid,
            I2CScan { msg_id } => I2CScanReply {
                msg_id,
                ndev: 4,
                payload: vec![0x11, 0x22, 0x33, 0x44],
            },
            //I2CScanReply { msg_id, .. } => *msg_id = mid,
            I2CWrite { msg_id, .. } => I2CWriteReply {
                msg_id,
                err_code: 0,
            },
            //I2CWriteReply { msg_id, .. } => *msg_id = mid,
            I2CWriteReg { msg_id, .. } => I2CWriteRegReply {
                msg_id,
                err_code: 0,
            },
            //I2CWriteRegReply { msg_id, .. } => *msg_id = mid,
            I2CRead { msg_id, .. } => I2CReadReply {
                msg_id,
                err_code: 0,
                len: 10,
                payload: vec![0; 10],
            },
            //I2CReadReply { msg_id, .. } => *msg_id = mid,
            I2CReadReg { msg_id, .. } => I2CReadRegReply {
                msg_id,
                err_code: 0,
                len: 10,
                payload: vec![0; 10],
            },
            //I2CReadRegReply { msg_id, .. } => *msg_id = mid,
            StreamStart { msg_id } => StreamStartReply { msg_id },
            //StreamStartReply { msg_id } => *msg_id = mid,
            StreamStop { msg_id } => StreamStopReply { msg_id },
            //StreamStopReply { msg_id } => *msg_id = mid,
            BitShift {
                msg_id,
                port_id,
                lsb_bit_idx,
            } => {
                shifts.insert(port_id, lsb_bit_idx);
                BitShiftReply { msg_id }
            }
            QueryBitShift { msg_id, port_id } => QueryBitShiftReply {
                msg_id,
                lsb_bit_idx: shifts.get(&port_id).copied().unwrap_or(0),
            },
            ClrOv { msg_id } => ClrOvReply { msg_id },

            PwrCtrl { msg_id, .. } => PwrCtrlReply { msg_id },

            XGbeCfgSingle {
                msg_id,
                port_id: _,
                cfg: _,
            } => XGbeCfgSingleReply { msg_id },

            XGbeCfgQuery { msg_id } => XGbeCfgQueryReply {
                msg_id,
                nports: 4,
                cfg: vec![
                    syncdaq::ctrl_msg::XGbeCfg {
                        dst_ip: [192, 168, 4, 10],
                        dst_mac: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
                        dst_port: 3000,
                        src_ip: [192, 168, 10, 11],
                        src_mac: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xfe],
                        src_port: 3000,
                    };
                    4
                ],
            },

            // 本振：缓存最后一次 SetMixer 的值，GetMixer 原样回读。
            // （真固件是每次调 XRFdc_GetMixerSettings() 现读，见 rfdc.c；
            //   dummy 没有硬件，只能这样模拟。）
            SetMixer {
                msg_id,
                nports,
                sync,
                freq,
                phase,
            } => {
                println!("SetMixer: nports={nports} sync={sync} freq={freq:?} phase={phase:?}");
                mixer_freq.clone_from(&freq);
                mixer_phase.clone_from(&phase);
                SetMixerReply { msg_id }
            }

            GetMixer { msg_id } => GetMixerReply {
                msg_id,
                nports: mixer_freq.len() as u32,
                freq: mixer_freq.clone(),
                phase: mixer_phase.clone(),
            },

            x => {
                let desc = "invalid".to_string().as_bytes().to_vec();
                InvalidMsg {
                    msg_id: x.get_msg_id(),
                    err_code: 0,
                    len: desc.len() as u32,
                    description: desc,
                }
            }
        };

        let mut cursor = Cursor::new(Vec::new());
        reply.write(&mut cursor).unwrap();
        let buf = cursor.into_inner();
        println!("sending {} Bytes to {addr}", buf.len());
        print_bytes(&buf);
        socket.send_to(&buf, addr).unwrap();
    }
}
