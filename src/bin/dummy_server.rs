use clap::Parser;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// config
    #[clap(short = 'a', long = "addr", num_args(1..), value_name="ip:port")]
    addr: String,
}

use binrw::{BinRead, BinWrite};
use syncdaq::ctrl_msg::{
    print_bytes,
    CtrlMsg::{self, *},
    Health,
};
use std::{io::Cursor, net::UdpSocket};
fn main() {
    let args = Args::parse();
    let socket = UdpSocket::bind(args.addr).unwrap();
    socket.set_nonblocking(false).unwrap();
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
                health: Health::HLHealth {
                    nhealth: 10,
                    xgbe_state: [10, 10, 10, 10],
                    pkt_sent: [0x1234, 0x1234, 0x1234, 0x1234],
                    volt12_inner: 12000,
                    volt12_input: 12000,
                    vcc1v0: 120000,
                    vcc1v8: 12000,
                    mgtavtt1v2: 12000,
                    mgtavtt1v0: 12000,
                    temperatures: [40000, 30000],
                },
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
            Init { msg_id, .. } => InitReply { msg_id },

            PwrCtrl { msg_id, .. } => PwrCtrlReply { msg_id },

            XGbeCfgSingle { msg_id, port_id:_, cfg:_ }=>XGbeCfgSingleReply { msg_id },

            XGbeCfgQuery { msg_id }=>XGbeCfgQueryReply { msg_id, nports: 4, cfg: vec![syncdaq::ctrl_msg::XGbeCfg{
                dst_ip:[192,168,4,10],
                dst_mac:[0xaa,0xbb,0xcc,0xdd,0xee,0xff],
                dst_port:3000,
                src_ip:[192,168,10,11],
                src_mac:[0xaa,0xbb,0xcc,0xdd,0xee,0xfe],
                src_port:3000,
            }; 4] },

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
        socket.send_to(&buf, addr).unwrap();
    }
}
