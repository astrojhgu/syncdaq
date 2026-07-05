use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Display,
    io::Cursor,
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    time::Duration,
};

use binrw::{BinRead, BinWrite, binrw};
use chrono::Local;
use serde::{Deserialize, Serialize};

use rand::{RngExt, rng};

#[derive(Clone, Copy, Serialize, Deserialize, Debug)]
#[binrw]
#[brw(little)]
pub struct XGbeCfg {
    #[brw(pad_after(2))]
    pub dst_mac: [u8; 6],
    #[brw(pad_after(2))]
    pub src_mac: [u8; 6],

    pub dst_ip: [u8; 4], //20
    pub src_ip: [u8; 4], //24

    #[brw(pad_after(2))]
    pub dst_port: u16, //26
    #[brw(pad_after(2))]
    pub src_port: u16, //30
}

impl Display for XGbeCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}:{}",
            self.src_ip[0], self.src_ip[1], self.src_ip[2], self.src_ip[3], self.src_port
        )?;
        write!(f, "(")?;
        for x in self.src_mac {
            write!(f, "0x{x:02x},")?
        }
        write!(f, ") -> ")?;
        write!(
            f,
            "{}.{}.{}.{}:{}",
            self.dst_ip[0], self.dst_ip[1], self.dst_ip[2], self.dst_ip[3], self.dst_port
        )?;
        write!(f, "(")?;
        for x in self.dst_mac {
            write!(f, "0x{x:02x},")?
        }
        write!(f, ")")
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[binrw]
#[brw(little)]
pub enum Health {
    #[brw(magic(0x00_00_01_fe_u32))]
    T510Health {
        rfdc_restart_cnt: u32,
        //fifo_full_cnt: u32,
        temperature: f32,
        nports: u32,
        fifo_full_cnt: u32,
        #[br(count=nports)]
        pkt_cnt1: Vec<u64>,
        #[br(count=nports)]
        axi_frame_cnt1: Vec<u64>,
        #[br(count=nports)]
        pkt_cnt2: Vec<u64>,
        #[br(count=nports)]
        axi_frame_cnt2: Vec<u64>,
    },
}

impl Display for Health {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            &Health::T510Health {
                rfdc_restart_cnt,
                temperature,
                nports,
                fifo_full_cnt,
                ref pkt_cnt1,
                ref axi_frame_cnt1,
                ref pkt_cnt2,
                ref axi_frame_cnt2,
            } => {
                writeln!(f, "T510Health{{")?;
                writeln!(f, "rfdc_restart_cnt: {}, ", rfdc_restart_cnt)?;
                writeln!(f, "temperature: {} degC, ", temperature)?;
                writeln!(f, "nports: {}, ", nports)?;
                writeln!(f, "fifo_full_cnt: {}, ", fifo_full_cnt >> 16)?;
                writeln!(f, "fifo_len: {}, ", fifo_full_cnt & 0xffff)?;
                write!(f, "pkt_cnt1: \t\t[")?;
                for x in pkt_cnt1 {
                    write!(f, "{}, ", x)?;
                }
                writeln!(f, "], ")?;
                write!(f, "axi_frame_cnt1: \t[")?;
                for x in axi_frame_cnt1 {
                    write!(f, "{}, ", x)?;
                }
                writeln!(f, "], ")?;
                write!(f, "pkt_cnt2: \t\t[")?;
                for x in pkt_cnt2 {
                    write!(f, "{}, ", x)?;
                }
                writeln!(f, "], ")?;
                write!(f, "axi_frame_cnt2: \t[")?;
                for x in axi_frame_cnt2 {
                    write!(f, "{}, ", x)?;
                }
                writeln!(f, "]")?;
            }
        }
        writeln!(f, "}}")
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[binrw]
#[brw(little)]
pub enum CtrlMsg {
    #[brw(magic(0xff_ff_ff_ff_u32))]
    InvalidMsg {
        msg_id: u32,
        err_code: u32,
        len: u32,
        #[br(count=len)]
        description: Vec<u8>,
    },
    #[brw(magic(0x01_u32))]
    Query { msg_id: u32 },
    #[brw(magic(0xff_00_00_01_u32))]
    QueryReply {
        msg_id: u32,
        fm_ver: u32,
        tick_cnt1: u32,
        tick_cnt2: u32,
        trans_state: u32,
        locked: u32,
        health: Health,
    },
    #[brw(magic(0x02_u32))]
    Sync { msg_id: u32 },
    #[brw(magic(0xff_00_00_02_u32))]
    SyncReply { msg_id: u32 },
    #[brw(magic(0x03_u32))]
    XGbeCfg { msg_id: u32, cfg: [XGbeCfg; 4] },
    #[brw(magic(0xff_00_00_03_u32))]
    XgbeCfgReply { msg_id: u32 },
    #[brw(magic(0x04_u32))]
    I2CScan { msg_id: u32 },
    #[brw(magic(0xff_00_00_04_u32))]
    I2CScanReply {
        msg_id: u32,
        ndev: u32,
        #[br(count=ndev)]
        payload: Vec<u8>,
    },
    #[brw(magic(0x01_04_u32))]
    I2CWrite {
        msg_id: u32,
        dev_addr: u32,
        len: u32,
        #[br(count = len)]
        payload: Vec<u8>,
    },
    #[brw(magic(0xff_00_01_04_u32))]
    I2CWriteReply { msg_id: u32, err_code: u32 },
    #[brw(magic(0x02_04_u32))]
    I2CWriteReg {
        msg_id: u32,
        dev_addr: u32,
        reg_addr: u32,
        len: u32,
        #[br(count=len)]
        payload: Vec<u8>,
    },
    #[brw(magic(0xff_00_02_04_u32))]
    I2CWriteRegReply { msg_id: u32, err_code: u32 },
    #[brw(magic(0x03_04_u32))]
    I2CRead {
        msg_id: u32,
        dev_addr: u32,
        nbytes: u32,
    },
    #[brw(magic(0xff_00_03_04_u32))]
    I2CReadReply {
        msg_id: u32,
        err_code: u32,
        len: u32,
        #[br(count=len)]
        payload: Vec<u8>,
    },
    #[brw(magic(0x04_04_u32))]
    I2CReadReg {
        msg_id: u32,
        dev_addr: u32,
        reg_addr: u32,
        nbytes: u32,
    },
    #[brw(magic(0xff_00_04_04_u32))]
    I2CReadRegReply {
        msg_id: u32,
        err_code: u32,
        len: u32,
        #[br(count=len)]
        payload: Vec<u8>,
    },
    #[brw(magic(0x01_05_u32))]
    StreamStart { msg_id: u32 },
    #[brw(magic(0xff_00_01_05_u32))]
    StreamStartReply { msg_id: u32 },
    #[brw(magic(0x02_05_u32))]
    StreamStop { msg_id: u32 },
    #[brw(magic(0xff_00_02_05_u32))]
    StreamStopReply { msg_id: u32 },
    #[brw(magic(0x06_u32))]
    BitShift { msg_id: u32, shift_bits: u32 },
    #[brw(magic(0xff_00_00_06_u32))]
    BitShiftReply { msg_id: u32 },
    #[brw(magic(0x07_u32))]
    PwrCtrl { msg_id: u32, op_code: u32 },
    #[brw(magic(0xff_00_00_07_u32))]
    PwrCtrlReply { msg_id: u32 },

    #[brw(magic(0x08_u32))]
    Init { msg_id: u32, reserved_zeros: u32 },
    #[brw(magic(0xff_00_00_08_u32))]
    InitReply { msg_id: u32 },

    #[brw(magic(0x0a_u32))]
    XGbeCfgSingle {
        msg_id: u32,
        port_id: u32,
        cfg: XGbeCfg,
    },

    #[brw(magic(0xff_00_00_0a_u32))]
    XGbeCfgSingleReply { msg_id: u32 },

    #[brw(magic(0x0b_u32))]
    XGbeCfgQuery { msg_id: u32 },

    #[brw(magic(0xff_00_00_0b_u32))]
    XGbeCfgQueryReply {
        msg_id: u32,
        nports: u32,
        #[br(count=nports)]
        cfg: Vec<XGbeCfg>,
    },
    #[brw(magic(0x00_00_00_0c_u32))]
    SetClk {
        msg_id: u32,
        clk_src: u32,
        pps_src: u32,
    },
    #[brw(magic(0xff_00_00_0c_u32))]
    SetClkReply { msg_id: u32, clk_state: u32 },
    #[brw(magic(0x00_00_00_0d_u32))]
    MixerSet {
        msg_id: u32,
        nports: u32,
        sync: u32,
        #[br(count=nports)]
        freq: Vec<f64>,
        #[br(count=nports)]
        phase: Vec<f64>,
    },
    #[brw(magic(0xff_00_00_0d_u32))]
    MixerSetReply { msg_id: u32 },
    #[brw(magic(0x00_00_00_0e_u32))]
    PortMask { msg_id: u32, mask: u32 },
    #[brw(magic(0xff_00_00_0e_u32))]
    PortMaskReply { msg_id: u32 },
    #[brw(magic(0x00_00_10_01_u32))]
    QsfpInfo { msg_id: u32 },
    #[brw(magic(0xff_00_10_01_u32))]
    QsfpInfoReply {
        msg_id: u32,
        restl: u32,
        modsell: u32,
        lpmode: u32,
        modpresent: u32,
        temperature: f32,
        vcc: f32,
        tx_bias: [f32; 4],
        rx_power: [f32; 4],
        tx_power: [f32; 4],
        los_lol: [u8; 4], //{tx los, rx los}, 0, {tx_lol, rx_lol}, 0
        vcc_temp_alarm: [u8; 4],
        cdr_ctrl: [u8; 4],
    },
    #[brw(magic(0x00_00_10_02_u32))]
    QsfpSet {
        msg_id: u32,
        restl: u32,
        modsell: u32,
        lpmode: u32,
        cdr_ctrl: [u8; 4],
    },
    #[brw(magic(0xff_00_10_02_u32))]
    QsfpSetReply {
        msg_id: u32,
        restl: u32,
        modsell: u32,
        lpmode: u32,
        cdr_ctrl: [u8; 4],
    },
    #[brw(magic(0x00_00_11_01_u32))]
    GtSet {
        msg_id: u32,
        precursor: [u8; 4],
        postcursor: [u8; 4],
        txdiff: [u8; 4],
    },
    #[brw(magic(0xff_00_11_01_u32))]
    GtSetReply { msg_id: u32 },
    #[brw(magic(0x00_00_20_01_u32))]
    FetchFW {
        msg_id: u32,
        srv_ip: [u8; 4],
        srv_port: u32,
    },
    #[brw(magic(0xff_00_20_01_u32))]
    FetchFWReply {
        msg_id: u32,
        nbytes_fetched: u32,
        md5checked: u32,
    },
    #[brw(magic(0x00_00_20_02_u32))]
    SwitchFW { msg_id: u32, md5sum: [u8; 16] },
    #[brw(magic(0xff_00_20_02_u32))]
    SwitchFWReply { msg_id: u32, succeeded: u32 },
    #[brw(magic(0x00_00_00_ff_u32))]
    Reboot { msg_id: u32 },
    #[brw(magic(0xff_00_00_ff_u32))]
    RebootReply { msg_id: u32 },
}

fn bits_to_string(x: u8) -> String {
    let mut s = String::with_capacity(8);
    for i in (0..8).rev() {
        s.push_str(&(((x >> i) & 1).to_string()));
    }
    s
}

fn ascii_from_fixed(buf: &[u8]) -> &str {
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());

    std::str::from_utf8(&buf[..len]).unwrap()
}

impl Display for CtrlMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=====================")?;
        match self {
            CtrlMsg::InvalidMsg {
                msg_id,
                err_code,
                len: _,
                description,
            } => {
                let desc =
                    String::from_utf8(description.clone()).expect("failed to convert to utf8 str");
                writeln!(
                    f,
                    "InvalidMsg:{{ msg_id: {msg_id}, err_code: {err_code}, desc: {desc} }}"
                )
            }
            CtrlMsg::Query { msg_id } => {
                writeln!(f, "Query{{ msg_id: {msg_id} }}")
            }
            CtrlMsg::QueryReply {
                msg_id,
                fm_ver,
                tick_cnt1,
                tick_cnt2,
                trans_state,
                locked,
                health,
            } => {
                let day: u32 = (fm_ver >> 27) & 0x1F;
                let month: u32 = (fm_ver >> 23) & 0x0F;
                let year: u32 = 2000 + ((fm_ver >> 17) & 0x3F);
                let hour: u32 = (fm_ver >> 12) & 0x1F;
                let minute: u32 = (fm_ver >> 6) & 0x3F;
                let second: u32 = fm_ver & 0x3F;

                write!(
                    f,
                    "QueryReply{{
                    msg_id: {msg_id},
                    fm_ver: 0x{fm_ver:x} ({year}-{month:02}-{day:02} {hour}:{minute}:{second:02}),
                    tick_cnt1: {tick_cnt1},
                    tick_cnt2: {tick_cnt2},
                    trans_state: 0x{trans_state:x},
                    locked: 0x{locked:x},
                    Health: {health}"
                )?;
                writeln!(f, "}}")?;
                if *locked & 0x00_00_00_0f != 0x0f {
                    writeln!(f, "Lock stat abnormal!")?;
                }
                if tick_cnt2 - tick_cnt1 != 10_000_000 {
                    writeln!(f, "Warning, tick cnt diff != 10M ")
                } else {
                    writeln!(f, "tick cnt diff OK")
                }
            }
            CtrlMsg::Sync { msg_id } => {
                writeln!(f, "Sync {{msg_id: {msg_id}}}")
            }
            CtrlMsg::SyncReply { msg_id } => {
                writeln!(f, "SyncReply{{msg_id: {msg_id}}}")
            }
            CtrlMsg::XGbeCfg { msg_id, cfg } => {
                writeln!(f, "XGbeCfg{{msg_id: {msg_id}")?;
                for x in cfg {
                    writeln!(f, "{x}")?;
                }
                writeln!(f, "}}")
            }
            CtrlMsg::XgbeCfgReply { msg_id } => {
                writeln!(f, "XgbeCfgReply{{msg_id: {msg_id}}}")
            }
            CtrlMsg::I2CScan { msg_id } => {
                writeln!(f, "I2CScan{{msg_id: {msg_id}}}")
            }
            CtrlMsg::I2CScanReply {
                msg_id,
                ndev,
                payload,
            } => {
                write!(f, "I2CScanReply{{msg_id: {msg_id}, ndev: {ndev}, payload: ")?;
                for &x in payload {
                    write!(f, "0x{x:02x} ")?;
                }
                writeln!(f, "}}")
            }
            CtrlMsg::I2CWrite {
                msg_id,
                dev_addr,
                len: _,
                payload,
            } => {
                write!(f, "I2CWrite{{ msg_id: {msg_id}, dev_addr: 0x{dev_addr:x}, ")?;
                for &x in payload {
                    write!(f, " {x:02x}")?;
                }
                writeln!(f, "}}")
            }
            CtrlMsg::I2CWriteReply { msg_id, err_code } => {
                writeln!(
                    f,
                    "I2CWriteReply{{msg_id: {msg_id}, err_code: 0x{err_code:x}}}"
                )
            }
            CtrlMsg::I2CWriteReg {
                msg_id,
                dev_addr,
                reg_addr,
                len: _,
                payload,
            } => {
                write!(
                    f,
                    "I2CWriteReg{{ msg_id: {msg_id}, dev_addr: 0x{dev_addr:x}, reg_addr: 0x{reg_addr:x}"
                )?;
                for &x in payload {
                    write!(f, " {x:02x}")?;
                }
                writeln!(f, "}}")
            }
            CtrlMsg::I2CWriteRegReply { msg_id, err_code } => {
                writeln!(
                    f,
                    "I2CWriteRegReply{{msg_id: {msg_id}, err_code: 0x{err_code:x}}}"
                )
            }
            CtrlMsg::I2CRead {
                msg_id,
                dev_addr,
                nbytes,
            } => {
                writeln!(
                    f,
                    "I2CRead{{msg_id: {msg_id}, dev_addr: 0x{dev_addr:x}, nbytes:{nbytes}}}"
                )
            }
            CtrlMsg::I2CReadReply {
                msg_id,
                err_code,
                len: _,
                payload,
            } => {
                write!(
                    f,
                    "I2CReadReply{{ msg_id: {msg_id}, err_code: {err_code:x}, payload:"
                )?;
                for &x in payload {
                    write!(f, " {x:02x}")?;
                }
                writeln!(f, "}}")
            }
            CtrlMsg::I2CReadReg {
                msg_id,
                dev_addr,
                reg_addr,
                nbytes,
            } => {
                writeln!(
                    f,
                    "I2CReadReg{{msg_id: {msg_id}, dev_addr: 0x{dev_addr:x}, reg_addr: 0x{reg_addr:x} nbytes:{nbytes}}}"
                )
            }
            CtrlMsg::I2CReadRegReply {
                msg_id,
                err_code,
                len,
                payload,
            } => {
                write!(
                    f,
                    "I2CReadRegReply{{ msg_id: {msg_id}, len:{len}, err_code: {err_code:x} payload:"
                )?;
                for &x in payload {
                    write!(f, " {x:02x}")?;
                }
                writeln!(f, "}}")
            }
            CtrlMsg::StreamStart { msg_id } => {
                writeln!(f, "StreamStart{{msg_id: {msg_id}}}")
            }
            CtrlMsg::StreamStartReply { msg_id } => {
                writeln!(f, "StreamStartReply{{msg_id: {msg_id}}}")
            }
            CtrlMsg::StreamStop { msg_id } => {
                writeln!(f, "StreamStop{{msg_id: {msg_id}}}")
            }
            CtrlMsg::StreamStopReply { msg_id } => {
                writeln!(f, "StreamStopReply{{msg_id: {msg_id}}}")
            }
            CtrlMsg::BitShift { msg_id, shift_bits } => {
                write!(f, "Bitshift{{ msg_id: {msg_id} shift_bits:{shift_bits},")?;
                writeln!(f, "}}")
            }
            CtrlMsg::BitShiftReply { msg_id } => {
                writeln!(f, "Bitshift{{msg_id: {msg_id}}}")
            }
            CtrlMsg::PwrCtrl { msg_id, op_code } => {
                writeln!(f, "PwrCtrl{{msg_id: {msg_id}, op_code: {op_code}}}")
            }
            CtrlMsg::PwrCtrlReply { msg_id } => {
                writeln!(f, "PwrCtrlReply{{msg_id: {msg_id}}}")
            }

            CtrlMsg::Init {
                msg_id,
                reserved_zeros: _,
            } => {
                writeln!(f, "Init {{msg_id: {msg_id}}}")
            }

            CtrlMsg::InitReply { msg_id } => {
                writeln!(f, "InitReply {{msg_id: {msg_id}}}")
            }

            CtrlMsg::XGbeCfgSingle {
                msg_id,
                port_id,
                cfg,
            } => {
                writeln!(
                    f,
                    "XgbeCfgSingle {{msg_id: {msg_id}, port_id: {port_id}, cfg: {cfg}}}"
                )
            }

            CtrlMsg::XGbeCfgSingleReply { msg_id } => {
                writeln!(f, "XgbeCfgSingleReply {{msg_id: {msg_id}}}")
            }

            CtrlMsg::XGbeCfgQuery { msg_id } => {
                writeln!(f, "XgbeCfgQuery {{msg_id: {msg_id}}}")
            }

            CtrlMsg::XGbeCfgQueryReply {
                msg_id,
                nports,
                cfg,
            } => {
                writeln!(
                    f,
                    "XGbeCfgQueryReply {{msg_id: {msg_id}, nports: {nports}, "
                )?;
                writeln!(f, "cfg:[")?;
                for x in cfg {
                    writeln!(f, "{x}")?;
                }
                writeln!(f, "]}}")
            }

            CtrlMsg::SetClk {
                msg_id,
                clk_src,
                pps_src,
            } => {
                writeln!(
                    f,
                    "SetClk {{msg_id: {msg_id}, clk_src: {clk_src}, pps_src:{pps_src} }}"
                )
            }

            CtrlMsg::SetClkReply { msg_id, clk_state } => {
                writeln!(
                    f,
                    "SetClkReply {{msg_id: {msg_id}, clk_state: {clk_state}}}"
                )
            }
            CtrlMsg::MixerSet {
                msg_id,
                nports: _,
                freq,
                phase,
                sync,
            } => {
                writeln!(
                    f,
                    "MixerSet {{msg_id: {msg_id}, freq: {freq:?}, phase:{phase:?}, sync:{sync} }}"
                )
            }
            CtrlMsg::MixerSetReply { msg_id } => {
                writeln!(f, "MixerSetReply {{msg_id: {msg_id}}}")
            }
            CtrlMsg::PortMask { msg_id, mask } => {
                writeln!(f, "PortMask {{msg_id:{msg_id}, mask:{mask:x}}}")
            }
            CtrlMsg::PortMaskReply { msg_id } => {
                writeln!(f, "PortMaskReply{{msg_id:{msg_id}}}")
            }
            CtrlMsg::QsfpInfo { msg_id } => {
                writeln!(f, "QsfpInfo{{msg_id:{msg_id}}}")
            }
            CtrlMsg::QsfpInfoReply {
                msg_id,
                restl,
                modsell,
                lpmode,
                modpresent,
                temperature,
                vcc,
                tx_bias,
                rx_power,
                tx_power,
                los_lol,
                vcc_temp_alarm,
                cdr_ctrl,
            } => {
                writeln!(
                    f,
                    "QsfpInfoReply{{
                    msg_id: {msg_id},
                    temperature: {temperature},
                    vcc:{vcc},
                    tx_bias: {tx_bias:?},
                    rx_power: {rx_power:?},
                    tx_power: {tx_power:?},
                    los: {},
                    lol: {},
                    temp_alarm:{},
                    l_temp_alarm:{},
                    vcc_alarm:{},
                    l_vcc_alarm:{},
                    restl:{restl},
                    modsell:{modsell},
                    lpmode:{lpmode},
                    modpresent:{modpresent},
                    cdr_ctrl: [{:x}, {:x}, {:x}, {:x}],
                    }}",
                    bits_to_string(los_lol[0]),
                    bits_to_string(los_lol[2]),
                    bits_to_string(vcc_temp_alarm[0]),
                    bits_to_string(vcc_temp_alarm[1]),
                    bits_to_string(vcc_temp_alarm[2]),
                    bits_to_string(vcc_temp_alarm[3]),
                    cdr_ctrl[0],
                    cdr_ctrl[1],
                    cdr_ctrl[2],
                    cdr_ctrl[3],
                )
            }
            CtrlMsg::QsfpSet {
                msg_id,
                restl,
                modsell,
                lpmode,
                cdr_ctrl,
            } => {
                writeln!(
                    f,
                    "QsfpSet{{
                    msg_id: {msg_id},
                    restl: {restl},
                    modsell: {modsell},
                    lpmode: {lpmode},
                    cdr_ctrl: [{:x},{:x},{:x},{:x}]
                }}",
                    cdr_ctrl[0], cdr_ctrl[1], cdr_ctrl[2], cdr_ctrl[3],
                )
            }
            CtrlMsg::QsfpSetReply {
                msg_id,
                restl,
                modsell,
                lpmode,
                cdr_ctrl,
            } => {
                writeln!(
                    f,
                    "QsfpSet{{
                    msg_id: {msg_id},
                    restl: {restl},
                    modsell: {modsell},
                    lpmode: {lpmode},
                    cdr_ctrl: [{:x},{:x},{:x},{:x}]
            }}",
                    cdr_ctrl[0], cdr_ctrl[1], cdr_ctrl[2], cdr_ctrl[3],
                )
            }
            CtrlMsg::GtSet {
                msg_id,
                precursor,
                postcursor,
                txdiff,
            } => {
                writeln!(
                    f,
                    "GtSet{{
                    msg_id: {msg_id},
                    precursor: {precursor:?},
                    postcursor: {postcursor:?},
                    txdiff: {txdiff:?}
                    }}
                    "
                )
            }
            CtrlMsg::GtSetReply { msg_id } => {
                writeln!(
                    f,
                    "GtSetReply {{
                    msg_id: {msg_id},
                    }}"
                )
            }
            CtrlMsg::FetchFW {
                msg_id,
                srv_ip,
                srv_port,
            } => {
                writeln!(
                    f,
                    "
                    FetchFW{{
                    msg_id: {msg_id},
                    srv_ip: {srv_ip:?},
                    srv_port: {srv_port},
                    }}
                    "
                )
            }
            CtrlMsg::FetchFWReply {
                msg_id,
                nbytes_fetched,
                md5checked,
            } => {
                writeln!(
                    f,
                    "
                    FetchFWReply {{
                    msg_id: {msg_id},
                    nbytes_fetched: {nbytes_fetched}
                    md5checked: {md5checked},
                    }}
                    "
                )
            }
            CtrlMsg::SwitchFW { msg_id, md5sum } => {
                writeln!(
                    f,
                    "
                    SwitchFW{{
                    msg_id: {msg_id},
                    md5sum: {md5sum:x?},
                    }}
                    "
                )
            }
            CtrlMsg::SwitchFWReply { msg_id, succeeded } => {
                writeln!(
                    f,
                    "
                    SwitchFWReply{{
                    msg_id: {msg_id},
                    succeeded: {succeeded}
                    }}
                    "
                )
            }
            CtrlMsg::Reboot { msg_id } => {
                writeln!(f, "Reboot{{msg_id:{msg_id}}}")
            }
            CtrlMsg::RebootReply { msg_id } => {
                writeln!(f, "RebootReply{{msg_id:{msg_id}}}")
            }
        }?;
        writeln!(f, "=====================")
    }
}

impl CtrlMsg {
    pub fn set_msg_id(&mut self, mid: u32) {
        use CtrlMsg::*;
        match self {
            InvalidMsg { msg_id, .. } => *msg_id = mid,
            Query { msg_id } => *msg_id = mid,
            QueryReply { msg_id, .. } => *msg_id = mid,
            Sync { msg_id } => *msg_id = mid,
            SyncReply { msg_id } => *msg_id = mid,
            XGbeCfg { msg_id, .. } => *msg_id = mid,
            XgbeCfgReply { msg_id } => *msg_id = mid,
            I2CScan { msg_id } => *msg_id = mid,
            I2CScanReply { msg_id, .. } => *msg_id = mid,
            I2CWrite { msg_id, .. } => *msg_id = mid,
            I2CWriteReply { msg_id, .. } => *msg_id = mid,
            I2CWriteReg { msg_id, .. } => *msg_id = mid,
            I2CWriteRegReply { msg_id, .. } => *msg_id = mid,
            I2CRead { msg_id, .. } => *msg_id = mid,
            I2CReadReply { msg_id, .. } => *msg_id = mid,
            I2CReadReg { msg_id, .. } => *msg_id = mid,
            I2CReadRegReply { msg_id, .. } => *msg_id = mid,
            StreamStart { msg_id } => *msg_id = mid,
            StreamStartReply { msg_id } => *msg_id = mid,
            StreamStop { msg_id } => *msg_id = mid,
            StreamStopReply { msg_id } => *msg_id = mid,
            BitShift { msg_id, .. } => *msg_id = mid,
            BitShiftReply { msg_id, .. } => *msg_id = mid,
            PwrCtrl { msg_id, .. } => *msg_id = mid,
            PwrCtrlReply { msg_id, .. } => *msg_id = mid,
            Init { msg_id, .. } => *msg_id = mid,
            InitReply { msg_id, .. } => *msg_id = mid,
            XGbeCfgQuery { msg_id } => *msg_id = mid,
            XGbeCfgQueryReply {
                msg_id,
                nports: _,
                cfg: _a,
            } => *msg_id = mid,
            XGbeCfgSingle {
                msg_id,
                port_id: _,
                cfg: _,
            } => *msg_id = mid,
            XGbeCfgSingleReply { msg_id } => *msg_id = mid,
            SetClk { msg_id, .. } => *msg_id = mid,
            SetClkReply { msg_id, .. } => *msg_id = mid,
            MixerSet { msg_id, .. } => *msg_id = mid,
            MixerSetReply { msg_id } => *msg_id = mid,
            PortMask { msg_id, .. } => *msg_id = mid,
            PortMaskReply { msg_id } => *msg_id = mid,
            QsfpInfo { msg_id } => *msg_id = mid,
            QsfpInfoReply { msg_id, .. } => *msg_id = mid,
            QsfpSet { msg_id, .. } => *msg_id = mid,
            QsfpSetReply { msg_id, .. } => *msg_id = mid,
            GtSet { msg_id, .. } => *msg_id = mid,
            GtSetReply { msg_id, .. } => *msg_id = mid,
            FetchFW { msg_id, .. } => *msg_id = mid,
            FetchFWReply { msg_id, .. } => *msg_id = mid,
            SwitchFW { msg_id, .. } => *msg_id = mid,
            SwitchFWReply { msg_id, .. } => *msg_id = mid,
            Reboot { msg_id, .. } => *msg_id = mid,
            RebootReply { msg_id, .. } => *msg_id = mid,
        }
    }

    pub fn get_msg_id(&self) -> u32 {
        use CtrlMsg::*;
        match self {
            InvalidMsg { msg_id, .. } => *msg_id,
            Query { msg_id } => *msg_id,
            QueryReply { msg_id, .. } => *msg_id,
            Sync { msg_id } => *msg_id,
            SyncReply { msg_id } => *msg_id,
            XGbeCfg { msg_id, .. } => *msg_id,
            XgbeCfgReply { msg_id } => *msg_id,
            I2CScan { msg_id } => *msg_id,
            I2CScanReply { msg_id, .. } => *msg_id,
            I2CWrite { msg_id, .. } => *msg_id,
            I2CWriteReply { msg_id, .. } => *msg_id,
            I2CWriteReg { msg_id, .. } => *msg_id,
            I2CWriteRegReply { msg_id, .. } => *msg_id,
            I2CRead { msg_id, .. } => *msg_id,
            I2CReadReply { msg_id, .. } => *msg_id,
            I2CReadReg { msg_id, .. } => *msg_id,
            I2CReadRegReply { msg_id, .. } => *msg_id,
            StreamStart { msg_id } => *msg_id,
            StreamStartReply { msg_id } => *msg_id,
            StreamStop { msg_id } => *msg_id,
            StreamStopReply { msg_id } => *msg_id,
            BitShift { msg_id, .. } => *msg_id,
            BitShiftReply { msg_id, .. } => *msg_id,
            PwrCtrl { msg_id, .. } => *msg_id,
            PwrCtrlReply { msg_id } => *msg_id,
            Init {
                msg_id,
                reserved_zeros: _,
            } => *msg_id,
            InitReply { msg_id } => *msg_id,

            XGbeCfgQuery { msg_id } => *msg_id,
            XGbeCfgQueryReply {
                msg_id,
                nports: _,
                cfg: _a,
            } => *msg_id,
            XGbeCfgSingle {
                msg_id,
                port_id: _,
                cfg: _,
            } => *msg_id,
            XGbeCfgSingleReply { msg_id } => *msg_id,
            SetClk { msg_id, .. } => *msg_id,
            SetClkReply { msg_id, .. } => *msg_id,
            MixerSet { msg_id, .. } => *msg_id,
            MixerSetReply { msg_id } => *msg_id,
            PortMask { msg_id, .. } => *msg_id,
            PortMaskReply { msg_id } => *msg_id,
            QsfpInfo { msg_id } => *msg_id,
            QsfpInfoReply { msg_id, .. } => *msg_id,
            QsfpSet { msg_id, .. } => *msg_id,
            QsfpSetReply { msg_id, .. } => *msg_id,
            GtSet { msg_id, .. } => *msg_id,
            GtSetReply { msg_id } => *msg_id,
            FetchFW { msg_id, .. } => *msg_id,
            FetchFWReply { msg_id, .. } => *msg_id,
            SwitchFW { msg_id, .. } => *msg_id,
            SwitchFWReply { msg_id, .. } => *msg_id,
            Reboot { msg_id, .. } => *msg_id,
            RebootReply { msg_id, .. } => *msg_id,
        }
    }
}

pub fn print_bytes(x: &[u8]) {
    for (i, w) in x.chunks(4).enumerate() {
        for &b in w {
            print!("{b:02x} ");
        }
        print!("| {i:02} {}:{}", i * 4, i * 4 + 3);
        println!();
    }
}

#[derive(Default, Debug)]
pub struct CmdReplySummary {
    pub no_reply: Vec<(Vec<SocketAddr>, u32)>,
    pub invalid_reply: Vec<(SocketAddr, CtrlMsg)>,
    pub normal_reply: Vec<(SocketAddr, CtrlMsg)>,
}

pub fn send_cmd<A, B>(
    mut cmd: CtrlMsg,
    targets: &[A],
    local_addr: B,
    timeout: Option<Duration>,
    debug_level: u32,
) -> CmdReplySummary
where
    A: ToSocketAddrs,
    B: ToSocketAddrs,
{
    let socket = UdpSocket::bind(local_addr).expect("faild to bind addr");
    socket.set_broadcast(true).expect("broadcast set failed");
    socket
        .set_nonblocking(true)
        .expect("nonblocking set failed");

    socket
        .set_read_timeout(timeout)
        .expect("failed to set timeout");

    let mut rng1 = rng();
    let mut msg_set = BTreeSet::new();
    let mut addr_msg_id_map = BTreeMap::<u32, Vec<SocketAddr>>::new();
    let mut reply_summary = CmdReplySummary::default();
    for addr in targets.iter() {
        let msg_id: u32 = rng1.random();
        cmd.set_msg_id(msg_id);
        msg_set.insert(msg_id);
        addr_msg_id_map.insert(
            msg_id,
            addr.to_socket_addrs()
                .expect("faild to get socket addr")
                .collect::<Vec<_>>(),
        );
        let mut buf = Cursor::new(Vec::new());
        cmd.write(&mut buf).expect("failed to write cmd to buf");
        let buf = buf.into_inner();
        socket.send_to(&buf, addr).expect("send error");

        println!(
            "{} msg with id={} sent",
            Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            msg_id,
        );
        if debug_level > 0 {
            print_bytes(&buf);
        }

        println!("{cmd}");

        let mut buf = vec![0_u8; 9000];
        while let Ok((l, a)) = socket.recv_from(&mut buf) {
            //let (_s, _a)=socket.recv_from(&mut buf).unwrap();
            if debug_level > 0 {
                println!(
                    "{} received {} bytes, {} words from {:?}:",
                    Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                    l,
                    l / 4,
                    a
                );
                print_bytes(&buf[..l]);
            }
            let buf1 = std::mem::replace(&mut buf, vec![0_u8; 9000]);
            let mut cursor = Cursor::new(buf1);
            let reply = CtrlMsg::read(&mut cursor).expect("failed to read reply");

            let msg_id = reply.get_msg_id();
            if let CtrlMsg::InvalidMsg { .. } = reply {
                println!(
                    "{} Invalid msg {:?}",
                    Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                    reply
                );
                reply_summary.invalid_reply.push((a, reply));
            } else {
                reply_summary.normal_reply.push((a, reply));
            }

            println!(
                "{} msg with id={} replied from {:?}",
                Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                msg_id,
                a
            );
            let x = msg_set.remove(&msg_id);
            assert!(x);
        }
    }

    println!("==waiting for the rest replies==");
    socket
        .set_nonblocking(false)
        .expect("nonblocking set failed");

    let mut buf = vec![0_u8; 9000];

    if !msg_set.is_empty() {
        while let Ok((l, a)) = socket.recv_from(&mut buf) {
            if debug_level > 0 {
                println!(
                    "{} received {} bytes, {} words from {:?}:",
                    Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                    l,
                    l / 4,
                    a
                );
                print_bytes(&buf[..l]);
            }

            let mut cursor = Cursor::new(buf.clone());
            let reply = CtrlMsg::read(&mut cursor).expect("failed to read reply");
            println!(
                "{} \n{}",
                Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                reply
            );

            let msg_id = reply.get_msg_id();

            if let CtrlMsg::InvalidMsg { .. } = reply {
                println!("Invalid msg received");
                reply_summary.invalid_reply.push((a, reply));
            } else {
                reply_summary.normal_reply.push((a, reply));
            }

            println!(
                "{} msg with id={} replied from {:?}",
                Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                msg_id,
                a
            );
            let x = msg_set.remove(&msg_id);
            assert!(x);
            if msg_set.is_empty() {
                break;
            }
        }
    }
    //reply_summary.no_reply = msg_set.into_iter().map(|i| i as usize).collect();
    reply_summary.no_reply = addr_msg_id_map
        .iter()
        .filter(|&(k, _v)| msg_set.contains(k))
        .map(|(&k, v)| (v.clone(), k))
        .collect();
    reply_summary
}

pub fn bcast_cmd<A, B>(
    mut cmd: CtrlMsg,
    baddr: A,
    local_addr: B,
    timeout: Option<Duration>,
    debug_level: u32,
) -> CmdReplySummary
where
    A: ToSocketAddrs,
    B: ToSocketAddrs,
{
    let mut rng1 = rng();
    let socket = UdpSocket::bind(local_addr).expect("failed to bind");
    socket.set_broadcast(true).expect("broadcast set failed");
    socket
        .set_nonblocking(true)
        .expect("nonblocking set failed");

    socket
        .set_read_timeout(timeout)
        .expect("failed to set timeout");

    let mut reply_summary = CmdReplySummary::default();

    let msg_id: u32 = rng1.random();
    cmd.set_msg_id(msg_id);

    let mut buf = Cursor::new(Vec::new());
    cmd.write(&mut buf).expect("failed to write cmd to buf");
    let buf = buf.into_inner();
    socket.send_to(&buf, baddr).expect("send error");

    println!(
        "{} msg with id={} sent",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        0,
    );

    if debug_level > 0 {
        print_bytes(&buf);
    }

    println!("{cmd:?}");

    let mut buf = vec![0_u8; 9000];
    while let Ok((l, a)) = socket.recv_from(&mut buf) {
        //let (_s, _a)=socket.recv_from(&mut buf).unwrap();
        if debug_level > 0 {
            println!(
                "{} received {} bytes, {} words from {:?}:",
                Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                l,
                l / 4,
                a
            );
            print_bytes(&buf[..l]);
        }
        let buf1 = std::mem::replace(&mut buf, vec![0_u8; 9000]);
        let mut cursor = Cursor::new(buf1);
        let reply = CtrlMsg::read(&mut cursor).expect("failed to read reply");

        let msg_id = reply.get_msg_id();
        if let CtrlMsg::InvalidMsg { .. } = reply {
            println!(
                "{} Invalid msg {:?}",
                Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                reply
            );
            reply_summary.invalid_reply.push((a, reply));
        } else {
            reply_summary.normal_reply.push((a, reply));
        }

        println!(
            "{} msg with id={} replied from {:?}",
            Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            msg_id,
            a
        );
    }

    println!("==waiting for the rest replies==");
    socket
        .set_nonblocking(false)
        .expect("nonblocking set failed");

    let mut buf = vec![0_u8; 9000];

    while let Ok((l, a)) = socket.recv_from(&mut buf) {
        if debug_level > 0 {
            println!(
                "{} received {} bytes, {} words from {:?}:",
                Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                l,
                l / 4,
                a
            );
            print_bytes(&buf[..l]);
        }

        let mut cursor = Cursor::new(buf.clone());
        let reply = CtrlMsg::read(&mut cursor).expect("failed to read reply");
        println!(
            "{} \n{}",
            Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            reply
        );

        let msg_id = reply.get_msg_id();

        if let CtrlMsg::InvalidMsg { .. } = reply {
            println!("Invalid msg received");
            reply_summary.invalid_reply.push((a, reply));
        } else {
            reply_summary.normal_reply.push((a, reply));
        }

        println!(
            "{} msg with id={} replied from {:?}",
            Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            msg_id,
            a
        );
    }
    reply_summary
}
