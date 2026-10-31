use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Display,
    io::Cursor,
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    time::Duration,
};

use binrw::{BinRead, BinWrite, binrw};
use chrono::{Local, TimeZone, Utc};
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

fn boot_mode_name(m: u32) -> &'static str {
    match m & 0xF {
        0 => "JTAG",
        1 => "QSPI24",
        2 => "QSPI32",
        3 => "SD0(1.8V)",
        4 => "NAND",
        5 => "SD1",
        6 => "MMC",
        7 => "USB",
        8 => "EMMC",
        14 => "SD0(3.0V)",
        _ => "?",
    }
}

/// `GetClkReply.clk_src`：固件 gpio.h 的 ClockSource 枚举。
fn clk_src_name(s: u32) -> &'static str {
    match s {
        0 => "BUILTIN_10M",
        1 => "GPS_10M",
        2 => "EXT_10M",
        _ => "INVALID",
    }
}

/// `GetClkReply.pps_src`：固件 gpio.h 的 PpsSource 枚举。
fn pps_src_name(s: u32) -> &'static str {
    match s {
        0 => "GPS_PPS",
        1 => "EXT_PPS",
        _ => "INVALID",
    }
}

/// 解码 sw 版本位域（与 PL 版本同构：day@[31:27] month@[26:23] year@[22:17]
/// hour@[16:12] minute@[11:6] second@[5:0]，year 为 2000 偏移），
/// 其值是编译期 UTC 时间，转成本地时间显示。
fn sw_build_time_local(z: u32) -> String {
    let day = (z >> 27) & 0x1F;
    let month = (z >> 23) & 0x0F;
    let year = 2000 + ((z >> 17) & 0x3F);
    let hour = (z >> 12) & 0x1F;
    let minute = (z >> 6) & 0x3F;
    let second = z & 0x3F;
    match Utc
        .with_ymd_and_hms(year as i32, month, day, hour, minute, second)
        .single()
    {
        Some(utc) => utc
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        None => format!("0x{z:08x} (invalid)"),
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
        _z: u32,               /* sw 编译时间（UTC 位域，主机打印转本地） */
        rfdc_restart_cnt: u32,
        temperature: f32,
        nports: u32,
        boot_mode: u32,
        uptime_s: u32,
        fifo_full_cnt: u32,
        over_voltage_state: u32,
        over_range_state: u32,
        smp_rate: u32,
        smp_bits: u32,
        iq_mode: u32,
        fan_pulse_cnt: u32,
        #[br(count=nports)]
        pkt_cnt1: Vec<u64>,
        #[br(count=nports)]
        axi_frame_cnt1: Vec<u64>,
        #[br(count=nports)]
        pkt_cnt2: Vec<u64>,
        #[br(count=nports)]
        axi_frame_cnt2: Vec<u64>,
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
    BitShift {
        msg_id: u32,
        port_id: u32,
        lsb_bit_idx: u32,
    },
    #[brw(magic(0xff_00_00_06_u32))]
    BitShiftReply { msg_id: u32 },
    #[brw(magic(0x01_06_u32))]
    QueryBitShift { msg_id: u32, port_id: u32 },
    #[brw(magic(0xff_00_01_06_u32))]
    QueryBitShiftReply { msg_id: u32, lsb_bit_idx: u32 },
    #[brw(magic(0x07_u32))]
    PwrCtrl { msg_id: u32, op_code: u32 },
    #[brw(magic(0xff_00_00_07_u32))]
    PwrCtrlReply { msg_id: u32 },

    #[brw(magic(0x08_u32))]
    ClrOv { msg_id: u32 },
    #[brw(magic(0xff_00_00_08_u32))]
    ClrOvReply { msg_id: u32 },

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
    /// 读回当前时钟配置，与 `SetClk` 对称。读的是硬件实际值，不是软件缓存：
    /// `clk_src` 来自 PS GPIO pin33/34 的当前电平组合（0=BUILTIN_10M、
    /// 1=GPS_10M、2=EXT_10M），`pps_src` 来自 PPS_SEL 寄存器
    /// （0=GPS_PPS、1=EXT_PPS），`clk_state` 是 LMK04828 的锁定状态
    /// （与 `QueryReply.locked` 同源）。上电后从未 `SetClk` 过也能读。
    /// 线上布局：magic(4) + msg_id(4) + clk_src(4) + pps_src(4) + clk_state(4) = 20B。
    #[brw(magic(0x00_00_00_1c_u32))]
    GetClk { msg_id: u32 },
    #[brw(magic(0xff_00_00_1c_u32))]
    GetClkReply {
        msg_id: u32,
        clk_src: u32,
        pps_src: u32,
        clk_state: u32,
    },
    #[brw(magic(0x00_00_00_0d_u32))]
    SetMixer {
        msg_id: u32,
        nports: u32,
        sync: u32,
        #[br(count=nports)]
        freq: Vec<f64>,
        #[br(count=nports)]
        phase: Vec<f64>,
    },
    #[brw(magic(0xff_00_00_0d_u32))]
    SetMixerReply { msg_id: u32 },
    /// 读回板卡 RFDC 内部当前生效的本振值。固件每次都用 `XRFdc_GetMixerSettings()`
    /// 现读（不缓存），所以 MTS 重排相位、或别处改动 NCO 之后，返回的是硬件真实值
    /// —— 上电未设过时是 IP 的默认 NCO，不是 0。
    /// 板卡按 `NPORTS` 个通道回包（与 `SetMixer` 请求的数组长度对称）。
    #[brw(magic(0x00_00_00_10_u32))]
    GetMixer { msg_id: u32 },
    #[brw(magic(0xff_00_00_10_u32))]
    GetMixerReply {
        msg_id: u32,
        /// `nports` 之后有 4 字节保留位：C 端 `struct GetMixerReply` 里
        /// `double` 要求 8 字节对齐，编译器会在 12 处插 4 字节 padding，
        /// sizeof 是 144 而不是 140。固件把该填充显式写成 `u32_t _pad`，
        /// 这里用 pad_after 对齐，别把 `12` 当数组起点。
        #[brw(pad_after(4))]
        nports: u32,
        #[br(count=nports)]
        freq: Vec<f64>,
        #[br(count=nports)]
        phase: Vec<f64>,
    },
    #[brw(magic(0x00_00_00_0e_u32))]
    GetDSA { msg_id: u32, port_id: u32 },
    #[brw(magic(0xff_00_00_0e_u32))]
    GetDSAReply { msg_id: u32, dsa_value: f32 },
    #[brw(magic(0x00_00_00_0f_u32))]
    SetDSA {
        msg_id: u32,
        port_id: u32,
        dsa_value: f32,
    },
    #[brw(magic(0xff_00_00_0f_u32))]
    SetDSAReply { msg_id: u32 },
    #[brw(magic(0x00_00_10_01_u32))]
    QsfpInfo { msg_id: u32 },
    /// 回读 QSFP 光模块状态。`restl` 已删除：RstL 电平归 `QsfpReset` 独占，
    /// 在这里回读没有意义（`QsfpSetReply` 的 `restl` 是另一回事，那是写完后
    /// 三根控制位的整体回显）。`modpresent` 是 ModPrsL 引脚电平，低有效：
    /// 0 = 模块在位（插入后引脚被拉低），1 = 不在位。
    /// 线上布局 96B（比旧版少 4B，删掉的就是 `restl`）。
    #[brw(magic(0xff_00_10_01_u32))]
    QsfpInfoReply {
        msg_id: u32,
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
        eq_ctrl: [i8; 4],
        adapt_eq: [u8; 4],
    },
    /// QSFP 的 ModSelL / LPMode / CDR / 均衡设置。
    ///
    /// **不含 `restl`**：RstL 已剥离到 `QsfpReset`。固件 `qsfp_set_handler`
    /// 必须改成 read-modify-write（读 GPO 寄存器 → 只改 bit1/bit2 → 写回），
    /// 否则一次 QsfpSet 会把 bit0（RstL）连带清零、顺手把光模块复位掉。
    #[brw(magic(0x00_00_10_02_u32))]
    QsfpSet {
        msg_id: u32,
        modsell: u32,
        lpmode: u32,
        cdr_ctrl: [u8; 4],
        eq_ctrl: [i8; 4],
        adapt_eq: [u8; 4],
    },
    /// 回读三根控制引脚电平与模块当前设置。字段布局与 C 端
    /// `struct QsfpSetReply` 一一对应，`restl` 仍在（回读值，不是入参）：
    /// 写入后 RstL 应保持上一次 `QsfpReset` 释放后的高电平。
    #[brw(magic(0xff_00_10_02_u32))]
    QsfpSetReply {
        msg_id: u32,
        restl: u32,
        modsell: u32,
        lpmode: u32,
        cdr_ctrl: [u8; 4],
        eq_ctrl: [i8; 4],
        adapt_eq: [u8; 4],
    },
    /// 复位 QSFP 光模块。语义是**一个动作**：固件读 GPO → 把 RstL(bit0) 拉低
    /// `hold_ms` 毫秒 → 拉高 → 回包。主机不显式操作信号线，只给保持时间
    /// （RstL 为低有效：1 = 正常工作、0 = 复位中）。
    ///
    /// `hold_ms = 0` 表示最小拉低时间 1 ms（QSFP28 MSA 只要求 ≥1 µs，1 ms 是
    /// 固件能保证的最短脉冲；不是"不保持"）。固件侧把 `hold_ms` 钳到 500 ms
    /// 上限：handler 是持 `cmd_mutex` 跑的，睡多久两个网口的命令线程就堵多久。
    /// 线上布局：magic(4) + msg_id(4) + hold_ms(4) = 12B。
    #[brw(magic(0x00_00_10_03_u32))]
    QsfpReset {
        msg_id: u32,
        /// RstL 拉低的持续时间（ms）
        hold_ms: u32,
    },
    #[brw(magic(0xff_00_10_03_u32))]
    QsfpResetReply { msg_id: u32 },
    #[brw(magic(0x00_00_11_01_u32))]
    GtSet {
        msg_id: u32,
        precursor: [u8; 4],
        postcursor: [u8; 4],
        txdiff: [u8; 4],
    },
    #[brw(magic(0xff_00_11_01_u32))]
    GtSetReply { msg_id: u32 },
    /// 读回四路 GT 的 pre-cursor / post-cursor / txdiff，与 `GtSet` 对仗。
    /// 读的是 PL 里三个 AXI GPIO 寄存器（`XPAR_GT_*_BASEADDR`）的实际值：
    /// 每路 5 bit，按固件 `pack_gpio20` 的顺序打包 —— lane0@[4:0]、
    /// lane1@[9:5]、lane2@[14:10]、lane3@[19:15]。所以 PL 重配、链路重训
    /// 把参数复位之后，这里读到的是复位值，不是最后一次 Set 的缓存。
    /// 线上布局：magic(4) + msg_id(4) = 8B。
    #[brw(magic(0x00_00_11_02_u32))]
    GetGt { msg_id: u32 },
    /// 20B：msg_id(4) + precursor[4] + postcursor[4] + txdiff[4]，
    /// 与 `GtSet` 的请求体布局完全相同，只换 magic。
    #[brw(magic(0xff_00_11_02_u32))]
    GetGtReply {
        msg_id: u32,
        precursor: [u8; 4],
        postcursor: [u8; 4],
        txdiff: [u8; 4],
    },
    #[brw(magic(0x00_00_20_01_u32))]
    FetchFW {
        msg_id: u32,
        srv_ip: [u8; 4],
        srv_port: u32,
        storage: u32,
    },
    #[brw(magic(0xff_00_20_01_u32))]
    FetchFWReply {
        msg_id: u32,
        nbytes_fetched: u32,
        md5checked: u32,
    },
    #[brw(magic(0x00_00_20_02_u32))]
    SwitchFW {
        msg_id: u32,
        md5sum: [u8; 16],
        storage: u32,
    },
    #[brw(magic(0xff_00_20_02_u32))]
    SwitchFWReply { msg_id: u32, succeeded: u32 },
    /// 重启板子 / 控制看门狗喂狗。`mode` 决定用哪种机制：
    ///   1 = 停止喂狗，让看门狗超时（约 30s）后复位（用于验证看门狗；
    ///       收到 RebootReply 后板子**不会立刻**掉线，约 30s 后才掉）
    ///   2 = 恢复喂狗，**本命令不重启板子**（给 mode=1 一个反悔手段）
    ///   其它取值（含 0）= 普通重启：立即触发全芯片软 POR
    ///       （写 CRL_APB.RESET_CTRL 的 External POR 触发位，含重新配置 PL）
    ///
    /// 未定义的 mode 值一律回落到"普通重启"，是为了**方便回落**。
    ///
    /// 线上布局（小端，共 12B）：magic(4) + msg_id(4) + mode(4)，`mode` 在偏移 8。
    /// 固件侧把 `mode` 当成**可选字段**（只要求前 8 字节），
    /// 读不到就按 0 处理（= 普通重启，与老行为一致）。
    #[brw(magic(0x00_00_00_ff_u32))]
    Reboot {
        msg_id: u32,
        /// YAML 里漏写 `mode` 时按 0 处理（= 普通重启）。
        /// 注意 `#[serde(default)]` 要加在**字段**上；加在变体上 serde 会报
        /// `unknown serde variant attribute 'default'`。
        #[serde(default)]
        mode: u32,
    },
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

// fn ascii_from_fixed(buf: &[u8]) -> &str {
//     let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());

//     std::str::from_utf8(&buf[..len]).unwrap()
// }

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
                _z,
                rfdc_restart_cnt,
                temperature,
                nports,
                boot_mode,
                uptime_s,
                fifo_full_cnt,
                over_voltage_state,
                over_range_state,
                smp_rate,
                smp_bits,
                iq_mode,
                fan_pulse_cnt,
                pkt_cnt1,
                axi_frame_cnt1,
                pkt_cnt2,
                axi_frame_cnt2,
            } => {
                let day: u32 = (fm_ver >> 27) & 0x1F;
                let month: u32 = (fm_ver >> 23) & 0x0F;
                let year: u32 = 2000 + ((fm_ver >> 17) & 0x3F);
                let hour: u32 = (fm_ver >> 12) & 0x1F;
                let minute: u32 = (fm_ver >> 6) & 0x3F;
                let second: u32 = fm_ver & 0x3F;

                writeln!(f, "QueryReply{{")?;
                writeln!(f, "  msg_id: {msg_id}")?;
                writeln!(f, "  fm_ver: 0x{fm_ver:x} ({year}-{month:02}-{day:02} {hour}:{minute}:{second:02})")?;
                writeln!(f, "  tick_cnt1: {tick_cnt1}")?;
                writeln!(f, "  tick_cnt2: {tick_cnt2}")?;
                writeln!(f, "  trans_state: 0x{trans_state:x}")?;
                writeln!(f, "  locked: 0x{locked:x}")?;
                writeln!(f, "  sw_ver: {}", sw_build_time_local(*_z))?;
                writeln!(f, "  rfdc_restart_cnt: {rfdc_restart_cnt}")?;
                writeln!(f, "  temperature: {temperature} degC")?;
                writeln!(f, "  fan_pulse_cnt: {fan_pulse_cnt}")?;
                writeln!(f, "  nports: {nports}")?;
                writeln!(f, "  boot_mode: {boot_mode} ({})", boot_mode_name(*boot_mode))?;
                writeln!(f, "  uptime_s: {uptime_s} s")?;
                writeln!(f, "  smp_rate: {smp_rate} MSps")?;
                writeln!(f, "  smp_bits: {smp_bits}")?;
                writeln!(f, "  fmt: {}", if *iq_mode == 1 { "IQ" } else { "real" })?;
                writeln!(f, "  fifo_full_cnt: {}", fifo_full_cnt >> 16)?;
                writeln!(f, "  fifo_len: {}", fifo_full_cnt & 0xffff)?;
                writeln!(f, "  over_voltage_state: 0x{over_voltage_state:x}")?;
                writeln!(f, "  over_range_state: 0x{over_range_state:x}")?;
                write!(f, "  pkt_cnt1: [")?;
                for x in pkt_cnt1 {
                    write!(f, "{x}, ")?;
                }
                writeln!(f, "]")?;
                write!(f, "  axi_frame_cnt1: [")?;
                for x in axi_frame_cnt1 {
                    write!(f, "{x}, ")?;
                }
                writeln!(f, "]")?;
                write!(f, "  pkt_cnt2: [")?;
                for x in pkt_cnt2 {
                    write!(f, "{x}, ")?;
                }
                writeln!(f, "]")?;
                write!(f, "  axi_frame_cnt2: [")?;
                for x in axi_frame_cnt2 {
                    write!(f, "{x}, ")?;
                }
                writeln!(f, "]")?;
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
            CtrlMsg::BitShift {
                msg_id,
                port_id,
                lsb_bit_idx,
            } => {
                writeln!(
                    f,
                    "BitShift{{ msg_id: {msg_id}, port_id: {port_id}, lsb_bit_idx: {lsb_bit_idx} }}"
                )
            }
            CtrlMsg::BitShiftReply { msg_id } => {
                writeln!(f, "BitShiftReply{{msg_id: {msg_id}}}")
            }
            CtrlMsg::QueryBitShift { msg_id, port_id } => {
                writeln!(f, "QueryBitShift{{ msg_id: {msg_id}, port_id: {port_id} }}")
            }
            CtrlMsg::QueryBitShiftReply {
                msg_id,
                lsb_bit_idx,
            } => {
                writeln!(
                    f,
                    "QueryBitShiftReply{{ msg_id: {msg_id}, lsb_bit_idx: {lsb_bit_idx} }}"
                )
            }
            CtrlMsg::PwrCtrl { msg_id, op_code } => {
                writeln!(f, "PwrCtrl{{msg_id: {msg_id}, op_code: {op_code}}}")
            }
            CtrlMsg::PwrCtrlReply { msg_id } => {
                writeln!(f, "PwrCtrlReply{{msg_id: {msg_id}}}")
            }

            CtrlMsg::ClrOv { msg_id } => {
                writeln!(f, "ClrOv {{msg_id: {msg_id}}}")
            }

            CtrlMsg::ClrOvReply { msg_id } => {
                writeln!(f, "ClrOvReply {{msg_id: {msg_id}}}")
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
            CtrlMsg::GetClk { msg_id } => {
                writeln!(f, "GetClk{{msg_id:{msg_id}}}")
            }
            CtrlMsg::GetClkReply {
                msg_id,
                clk_src,
                pps_src,
                clk_state,
            } => {
                writeln!(
                    f,
                    "GetClkReply{{
                    msg_id: {msg_id},
                    clk_src: {clk_src} ({}),
                    pps_src: {pps_src} ({}),
                    clk_state: {clk_state},
                    }}",
                    clk_src_name(*clk_src),
                    pps_src_name(*pps_src),
                )
            }
            CtrlMsg::SetMixer {
                msg_id,
                nports: _,
                freq,
                phase,
                sync,
            } => {
                writeln!(
                    f,
                    "SetMixer {{msg_id: {msg_id}, freq: {freq:?}, phase:{phase:?}, sync:{sync} }}"
                )
            }
            CtrlMsg::SetMixerReply { msg_id } => {
                writeln!(f, "SetMixerReply {{msg_id: {msg_id}}}")
            }
            CtrlMsg::GetMixer { msg_id } => {
                writeln!(f, "GetMixer {{msg_id: {msg_id}}}")
            }
            CtrlMsg::GetMixerReply {
                msg_id,
                nports,
                freq,
                phase,
            } => {
                write!(
                    f,
                    "GetMixerReply {{msg_id: {msg_id}, nports: {nports}, freq: ["
                )?;
                // 回读值来自 NCO 频率字的反算，带 ~1e-11 MHz 的浮点尾数；
                // 按 1 Hz 显示（6 位小数）就够，别把 100.49999999999727 摆出来。
                for x in freq {
                    write!(f, "{x:.6}, ")?;
                }
                write!(f, "], phase: [")?;
                for x in phase {
                    write!(f, "{x:.6}, ")?;
                }
                writeln!(f, "]}}")
            }
            CtrlMsg::GetDSA { msg_id, port_id } => {
                writeln!(f, "GetDSA {{msg_id:{msg_id}, port_id: {port_id} }}")
            }
            CtrlMsg::GetDSAReply { msg_id, dsa_value } => {
                writeln!(f, "GetDSAReply{{msg_id:{msg_id}, dsa_value: {dsa_value}}}")
            }
            CtrlMsg::SetDSA {
                msg_id,
                port_id,
                dsa_value,
            } => {
                writeln!(
                    f,
                    "SetDSA{{msg_id:{msg_id}, port_id:{port_id} dsa_value: {dsa_value}}}"
                )
            }
            CtrlMsg::SetDSAReply { msg_id } => {
                writeln!(f, "SetDSAReply{{msg_id:{msg_id}}}")
            }
            CtrlMsg::QsfpInfo { msg_id } => {
                writeln!(f, "QsfpInfo{{msg_id:{msg_id}}}")
            }
            CtrlMsg::QsfpInfoReply {
                msg_id,
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
                eq_ctrl,
                adapt_eq,
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
                    l_temp_alarm:{},
                    l_vcc_alarm:{},
                    modsell:{modsell},
                    lpmode:{lpmode},
                    modpresent:{},
                    cdr_ctrl: [{:x}, {:x}, {:x}, {:x}],
                    eq_ctrl: [{:x}, {:x}, {:x}, {:x}],
                    adapt_ctrl: [{:x}, {:x}, {:x}, {:x}],
                    }}",
                    bits_to_string(los_lol[0]),
                    bits_to_string(los_lol[2]),
                    bits_to_string(vcc_temp_alarm[1]),
                    bits_to_string(vcc_temp_alarm[3]),
                    // ModPrsL 低有效：插上模块后引脚被拉低 -> 0 = 在位
                    if *modpresent == 0 { "present" } else { "absent" },
                    cdr_ctrl[0],
                    cdr_ctrl[1],
                    cdr_ctrl[2],
                    cdr_ctrl[3],
                    eq_ctrl[0],
                    eq_ctrl[1],
                    eq_ctrl[2],
                    eq_ctrl[3],
                    adapt_eq[0],
                    adapt_eq[1],
                    adapt_eq[2],
                    adapt_eq[3],
                )
            }
            CtrlMsg::QsfpSet {
                msg_id,
                modsell,
                lpmode,
                cdr_ctrl,
                eq_ctrl,
                adapt_eq,
            } => {
                writeln!(
                    f,
                    "QsfpSet{{
                    msg_id: {msg_id},
                    modsell: {modsell},
                    lpmode: {lpmode},
                    cdr_ctrl: [{:x},{:x},{:x},{:x}],
                    eq_ctrl: [{:x},{:x},{:x},{:x}],
                    adapt_eq: [{:x},{:x},{:x},{:x}],
                }}",
                    cdr_ctrl[0],
                    cdr_ctrl[1],
                    cdr_ctrl[2],
                    cdr_ctrl[3],
                    eq_ctrl[0],
                    eq_ctrl[1],
                    eq_ctrl[2],
                    eq_ctrl[3],
                    adapt_eq[0],
                    adapt_eq[1],
                    adapt_eq[2],
                    adapt_eq[3],
                )
            }
            CtrlMsg::QsfpSetReply {
                msg_id,
                restl,
                modsell,
                lpmode,
                cdr_ctrl,
                eq_ctrl,
                adapt_eq,
            } => {
                writeln!(
                    f,
                    "QsfpSetReply{{
                    msg_id: {msg_id},
                    restl: {restl},
                    modsell: {modsell},
                    lpmode: {lpmode},
                    cdr_ctrl: [{:x},{:x},{:x},{:x}],
                    eq_ctrl: [{:x},{:x},{:x},{:x}],
                    adapt_ctrl: [{:x},{:x},{:x},{:x}]
            }}",
                    cdr_ctrl[0],
                    cdr_ctrl[1],
                    cdr_ctrl[2],
                    cdr_ctrl[3],
                    eq_ctrl[0],
                    eq_ctrl[1],
                    eq_ctrl[2],
                    eq_ctrl[3],
                    adapt_eq[0],
                    adapt_eq[1],
                    adapt_eq[2],
                    adapt_eq[3],
                )
            }
            CtrlMsg::QsfpReset { msg_id, hold_ms } => {
                writeln!(f, "QsfpReset{{msg_id:{msg_id}, hold_ms:{hold_ms}}}")
            }
            CtrlMsg::QsfpResetReply { msg_id } => {
                writeln!(f, "QsfpResetReply{{msg_id:{msg_id}}}")
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
            CtrlMsg::GetGt { msg_id } => {
                writeln!(f, "GetGt{{msg_id:{msg_id}}}")
            }
            CtrlMsg::GetGtReply {
                msg_id,
                precursor,
                postcursor,
                txdiff,
            } => {
                writeln!(
                    f,
                    "GetGtReply{{
                    msg_id: {msg_id},
                    precursor: {precursor:?},
                    postcursor: {postcursor:?},
                    txdiff: {txdiff:?},
                    }}"
                )
            }
            CtrlMsg::FetchFW {
                msg_id,
                srv_ip,
                srv_port,
                storage,
            } => {
                writeln!(
                    f,
                    "
                    FetchFW{{
                    msg_id: {msg_id},
                    srv_ip: {srv_ip:?},
                    srv_port: {srv_port},
                    storage: {storage}
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
            CtrlMsg::SwitchFW {
                msg_id,
                md5sum,
                storage,
            } => {
                writeln!(
                    f,
                    "
                    SwitchFW{{
                    msg_id: {msg_id},
                    md5sum: {md5sum:x?},
                    storage: {storage}
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
            CtrlMsg::Reboot { msg_id, mode } => {
                writeln!(f, "Reboot{{msg_id:{msg_id} mode:{mode}}}")
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
            QueryBitShift { msg_id, .. } => *msg_id = mid,
            QueryBitShiftReply { msg_id, .. } => *msg_id = mid,
            PwrCtrl { msg_id, .. } => *msg_id = mid,
            PwrCtrlReply { msg_id, .. } => *msg_id = mid,
            ClrOv { msg_id, .. } => *msg_id = mid,
            ClrOvReply { msg_id, .. } => *msg_id = mid,
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
            GetClk { msg_id, .. } => *msg_id = mid,
            GetClkReply { msg_id, .. } => *msg_id = mid,
            SetMixer { msg_id, .. } => *msg_id = mid,
            SetMixerReply { msg_id } => *msg_id = mid,
            GetMixer { msg_id } => *msg_id = mid,
            GetMixerReply { msg_id, .. } => *msg_id = mid,
            GetDSA { msg_id, .. } => *msg_id = mid,
            GetDSAReply { msg_id, .. } => *msg_id = mid,
            SetDSA { msg_id, .. } => *msg_id = mid,
            SetDSAReply { msg_id } => *msg_id = mid,
            QsfpInfo { msg_id } => *msg_id = mid,
            QsfpInfoReply { msg_id, .. } => *msg_id = mid,
            QsfpSet { msg_id, .. } => *msg_id = mid,
            QsfpSetReply { msg_id, .. } => *msg_id = mid,
            QsfpReset { msg_id, .. } => *msg_id = mid,
            QsfpResetReply { msg_id, .. } => *msg_id = mid,
            GtSet { msg_id, .. } => *msg_id = mid,
            GtSetReply { msg_id, .. } => *msg_id = mid,
            GetGt { msg_id, .. } => *msg_id = mid,
            GetGtReply { msg_id, .. } => *msg_id = mid,
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
            QueryBitShift { msg_id, .. } => *msg_id,
            QueryBitShiftReply { msg_id, .. } => *msg_id,
            PwrCtrl { msg_id, .. } => *msg_id,
            PwrCtrlReply { msg_id } => *msg_id,
            ClrOv { msg_id } => *msg_id,
            ClrOvReply { msg_id } => *msg_id,
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
            GetClk { msg_id, .. } => *msg_id,
            GetClkReply { msg_id, .. } => *msg_id,
            SetMixer { msg_id, .. } => *msg_id,
            SetMixerReply { msg_id } => *msg_id,
            GetMixer { msg_id } => *msg_id,
            GetMixerReply { msg_id, .. } => *msg_id,
            GetDSA { msg_id, .. } => *msg_id,
            GetDSAReply { msg_id, .. } => *msg_id,
            SetDSA { msg_id, .. } => *msg_id,
            SetDSAReply { msg_id } => *msg_id,
            QsfpInfo { msg_id } => *msg_id,
            QsfpInfoReply { msg_id, .. } => *msg_id,
            QsfpSet { msg_id, .. } => *msg_id,
            QsfpSetReply { msg_id, .. } => *msg_id,
            QsfpReset { msg_id, .. } => *msg_id,
            QsfpResetReply { msg_id, .. } => *msg_id,
            GtSet { msg_id, .. } => *msg_id,
            GtSetReply { msg_id } => *msg_id,
            GetGt { msg_id } => *msg_id,
            GetGtReply { msg_id, .. } => *msg_id,
            FetchFW { msg_id, .. } => *msg_id,
            FetchFWReply { msg_id, .. } => *msg_id,
            SwitchFW { msg_id, .. } => *msg_id,
            SwitchFWReply { msg_id, .. } => *msg_id,
            Reboot { msg_id, .. } => *msg_id,
            RebootReply { msg_id, .. } => *msg_id,
        }
    }
}

/// 校验定长 / 变长 reply 的线上长度（QueryReply 20260913 加入，配合 smp_bits/iq_mode；
/// GetMixerReply 随 GetMixer 一起加入）。
/// C 端 udp_server.h 的对应结构体一旦出现隐式填充、字段增删或顺序变化，
/// 这里会立刻报出来，避免 host 静默按错位解析。
/// QueryReply 期望长度 = 80B 前缀 + 4 个 u64 数组 × nports；
/// GetMixerReply 期望长度 = 16B 前缀（含 nports 后的 4B 对齐保留位）+ 2 个 f64 数组 × nports。
pub fn check_reply_layout(reply: &CtrlMsg, consumed: usize, recv_len: usize) {
    if let CtrlMsg::QueryReply { nports, .. } = reply
    {
        let expect = 80 + 32 * (*nports as usize);
        if consumed != expect || recv_len != expect {
            eprintln!(
                "⚠ QueryReply 长度异常：期望 {expect}B（nports={nports}），实收 {recv_len}B、解析消耗 {consumed}B \
                 —— 固件与主机的 QueryReply 布局不一致（查 udp_server.h 字段/对齐 与 ctrl_msg.rs 的 QueryReply）"
            );
        }
    }
    if let CtrlMsg::GetMixerReply { nports, .. } = reply
    {
        let expect = 16 + 16 * (*nports as usize);
        if consumed != expect || recv_len != expect {
            eprintln!(
                "⚠ GetMixerReply 长度异常：期望 {expect}B（nports={nports}），实收 {recv_len}B、解析消耗 {consumed}B \
                 —— 固件与主机的 GetMixerReply 布局不一致（查 udp_server.h 的 nports/freq/phase 与 ctrl_msg.rs 的 GetMixerReply）"
            );
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
            check_reply_layout(&reply, cursor.position() as usize, l);

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
            check_reply_layout(&reply, cursor.position() as usize, l);
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
        check_reply_layout(&reply, cursor.position() as usize, l);

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
        check_reply_layout(&reply, cursor.position() as usize, l);
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

#[cfg(test)]
mod layout_tests {
    use super::*;
    use std::io::Cursor;

    /// 锁定 QueryReply 的线上布局（与 C 端 udp_server.h 一一对应）：
    /// 总长 336B，boot_mode 在偏移 44，uptime_s 在偏移 48，
    /// smp_bits 在偏移 68，iq_mode 在偏移 72。
    #[test]
    fn query_reply_wire_layout() {
        let msg = CtrlMsg::QueryReply {
            msg_id: 1,
            fm_ver: 2,
            tick_cnt1: 3,
            tick_cnt2: 4,
            trans_state: 5,
            locked: 6,
            _z: 0x6D340B4A,  /* 2026-10-13 00:45:10 UTC（位域样例） */
            rfdc_restart_cnt: 7,
            temperature: 40.0,
            nports: 8,
            boot_mode: 3,
            uptime_s: 12345,
            fifo_full_cnt: 0,
            over_voltage_state: 0,
            over_range_state: 0,
            smp_rate: 100,
            smp_bits: 0xAABB_CCDD,
            iq_mode: 0x1122_3344,
            fan_pulse_cnt: 9,
            pkt_cnt1: vec![0; 8],
            axi_frame_cnt1: vec![0; 8],
            pkt_cnt2: vec![0; 8],
            axi_frame_cnt2: vec![0; 8],
        };
        let mut buf = Cursor::new(Vec::new());
        msg.write(&mut buf).expect("write");
        let b = buf.into_inner();
        assert_eq!(b.len(), 336, "QueryReply 线上长度应为 336B");
        assert_eq!(&b[0..4], &0xff_00_00_01_u32.to_le_bytes(), "msg_type@0");
        assert_eq!(&b[28..32], &0x6D340B4A_u32.to_le_bytes(), "_z@28 (sw 版本)");
        assert_eq!(&b[44..48], &3_u32.to_le_bytes(), "boot_mode@44");
        assert_eq!(&b[48..52], &12345_u32.to_le_bytes(), "uptime_s@48");
        assert_eq!(&b[64..68], &100_u32.to_le_bytes(), "smp_rate@64");
        assert_eq!(&b[68..72], &0xAABB_CCDD_u32.to_le_bytes(), "smp_bits@68");
        assert_eq!(&b[72..76], &0x1122_3344_u32.to_le_bytes(), "iq_mode@72");
    }

    /// 锁定 SetMixer 的线上布局（与 C 端 udp_server.h 的 `struct SetMixer` 一一对应）：
    /// 总长 16 + 16×NPORTS = 144B（NPORTS=8），freq 起于偏移 16，phase 起于偏移 80。
    #[test]
    fn set_mixer_wire_layout() {
        let msg = CtrlMsg::SetMixer {
            msg_id: 0x1234,
            nports: 8,
            sync: 1,
            freq: vec![257.0; 8],
            phase: vec![0.0; 8],
        };
        let mut buf = Cursor::new(Vec::new());
        msg.write(&mut buf).expect("write");
        let b = buf.into_inner();
        assert_eq!(b.len(), 144, "SetMixer 线上长度应为 144B");
        assert_eq!(&b[0..4], &0x00_00_00_0d_u32.to_le_bytes(), "msg_type@0");
        assert_eq!(&b[4..8], &0x1234_u32.to_le_bytes(), "msg_id@4");
        assert_eq!(&b[8..12], &8_u32.to_le_bytes(), "nports@8");
        assert_eq!(&b[12..16], &1_u32.to_le_bytes(), "sync@12");
        assert_eq!(&b[16..24], &257.0_f64.to_le_bytes(), "freq[0]@16");
        assert_eq!(&b[80..88], &0.0_f64.to_le_bytes(), "phase[0]@80");
    }

    /// 锁定 GetMixer / GetMixerReply 的线上布局（与 C 端 udp_server.h 的
    /// `struct GetMixer` / `struct GetMixerReply` 一一对应）：
    /// 请求 8B；回复 16 + 16×NPORTS = 144B（NPORTS=8），freq 起于偏移 16，phase 起于偏移 80。
    /// 16 而不是 12：`nports` 后有 4 字节保留位（double 的 8 字节对齐），
    /// C 端是显式的 `u32_t _pad`，两边必须一致。
    #[test]
    fn get_mixer_wire_layout() {
        let req = CtrlMsg::GetMixer { msg_id: 7 };
        let mut buf = Cursor::new(Vec::new());
        req.write(&mut buf).expect("write");
        let b = buf.into_inner();
        assert_eq!(b.len(), 8, "GetMixer 线上长度应为 8B");
        assert_eq!(&b[0..4], &0x00_00_00_10_u32.to_le_bytes(), "msg_type@0");

        let reply = CtrlMsg::GetMixerReply {
            msg_id: 7,
            nports: 8,
            freq: vec![257.0; 8],
            phase: vec![1.5; 8],
        };
        let mut buf = Cursor::new(Vec::new());
        reply.write(&mut buf).expect("write");
        let b = buf.into_inner();
        assert_eq!(b.len(), 144, "GetMixerReply 线上长度应为 144B");
        assert_eq!(&b[0..4], &0xff_00_00_10_u32.to_le_bytes(), "msg_type@0");
        assert_eq!(&b[4..8], &7_u32.to_le_bytes(), "msg_id@4");
        assert_eq!(&b[8..12], &8_u32.to_le_bytes(), "nports@8");
        assert_eq!(&b[12..16], &0_u32.to_le_bytes(), "_pad@12 必须是 0");
        assert_eq!(&b[16..24], &257.0_f64.to_le_bytes(), "freq[0]@16");
        assert_eq!(&b[80..88], &1.5_f64.to_le_bytes(), "phase[0]@80");
    }

    /// 锁定 QsfpInfoReply 的线上布局（与 C 端 `struct QsfpInfoReply` 一一对应）：
    /// 96B（删掉 `restl` 后比旧版少 4B）。字段顺序 modsell@8, lpmode@12,
    /// modpresent@16, temperature@20, vcc@24, tx_bias@28, rx_power@44,
    /// tx_power@60, los_lol@76, vcc_temp_alarm@80, cdr_ctrl@84, eq_ctrl@88,
    /// adapt_eq@92。
    #[test]
    fn qsfp_info_reply_wire_layout() {
        let reply = CtrlMsg::QsfpInfoReply {
            msg_id: 0x9,
            modsell: 1,
            lpmode: 0,
            modpresent: 0,
            temperature: 40.0,
            vcc: 3.3,
            tx_bias: [1.0; 4],
            rx_power: [2.0; 4],
            tx_power: [3.0; 4],
            los_lol: [0xaa, 0, 0xbb, 0],
            vcc_temp_alarm: [0, 0xcc, 0, 0xdd],
            cdr_ctrl: [0xff, 0, 0, 0],
            eq_ctrl: [5; 4],
            adapt_eq: [1; 4],
        };
        let mut buf = Cursor::new(Vec::new());
        reply.write(&mut buf).expect("write");
        let b = buf.into_inner();
        assert_eq!(b.len(), 96, "QsfpInfoReply 线上长度应为 96B（已去掉 restl）");
        assert_eq!(&b[0..4], &0xff_00_10_01_u32.to_le_bytes(), "msg_type@0");
        assert_eq!(&b[4..8], &0x9_u32.to_le_bytes(), "msg_id@4");
        assert_eq!(&b[8..12], &1_u32.to_le_bytes(), "modsell@8");
        assert_eq!(&b[12..16], &0_u32.to_le_bytes(), "lpmode@12");
        assert_eq!(&b[16..20], &0_u32.to_le_bytes(), "modpresent@16");
        assert_eq!(&b[20..24], &40.0_f32.to_le_bytes(), "temperature@20");
        assert_eq!(&b[24..28], &3.3_f32.to_le_bytes(), "vcc@24");
        assert_eq!(&b[28..32], &1.0_f32.to_le_bytes(), "tx_bias[0]@28");
        assert_eq!(&b[44..48], &2.0_f32.to_le_bytes(), "rx_power[0]@44");
        assert_eq!(&b[60..64], &3.0_f32.to_le_bytes(), "tx_power[0]@60");
        assert_eq!(&b[76..80], &[0xaa, 0, 0xbb, 0], "los_lol@76");
        assert_eq!(&b[80..84], &[0, 0xcc, 0, 0xdd], "vcc_temp_alarm@80");
        assert_eq!(&b[84..88], &[0xff, 0, 0, 0], "cdr_ctrl@84");
        assert_eq!(&b[88..92], &[5_u8; 4], "eq_ctrl@88");
    }

    /// 锁定 QsfpSet / QsfpSetReply 的线上布局（与 C 端 `struct QsfpSet` /
    /// `struct QsfpSetReply` 一一对应）。
    /// QsfpSet 剥离 `restl` 后是 28B：msg_id@4, modsell@8, lpmode@12,
    /// cdr_ctrl@16, eq_ctrl@20, adapt_eq@24。
    /// QsfpSetReply 仍回读 `restl`，是 32B：restl@8, modsell@12。
    #[test]
    fn qsfp_set_wire_layout() {
        let req = CtrlMsg::QsfpSet {
            msg_id: 0x1234,
            modsell: 1,
            lpmode: 0,
            cdr_ctrl: [0xff, 0, 0, 0],
            eq_ctrl: [5; 4],
            adapt_eq: [1; 4],
        };
        let mut buf = Cursor::new(Vec::new());
        req.write(&mut buf).expect("write");
        let b = buf.into_inner();
        assert_eq!(b.len(), 28, "QsfpSet 线上长度应为 28B（已去掉 restl）");
        assert_eq!(&b[0..4], &0x00_00_10_02_u32.to_le_bytes(), "msg_type@0");
        assert_eq!(&b[4..8], &0x1234_u32.to_le_bytes(), "msg_id@4");
        assert_eq!(&b[8..12], &1_u32.to_le_bytes(), "modsell@8");
        assert_eq!(&b[12..16], &0_u32.to_le_bytes(), "lpmode@12");
        assert_eq!(&b[16..20], &[0xff, 0, 0, 0], "cdr_ctrl@16");
        assert_eq!(&b[20..24], &[5_u8; 4], "eq_ctrl@20");

        let reply = CtrlMsg::QsfpSetReply {
            msg_id: 0x1234,
            restl: 1,
            modsell: 1,
            lpmode: 0,
            cdr_ctrl: [0xff, 0, 0, 0],
            eq_ctrl: [5; 4],
            adapt_eq: [1; 4],
        };
        let mut buf = Cursor::new(Vec::new());
        reply.write(&mut buf).expect("write");
        let b = buf.into_inner();
        assert_eq!(b.len(), 32, "QsfpSetReply 线上长度应为 32B");
        assert_eq!(&b[0..4], &0xff_00_10_02_u32.to_le_bytes(), "msg_type@0");
        assert_eq!(&b[8..12], &1_u32.to_le_bytes(), "restl@8");
        assert_eq!(&b[12..16], &1_u32.to_le_bytes(), "modsell@12");
    }

    /// 锁定 QsfpReset / QsfpResetReply 的线上布局：
    /// 请求 12B（magic + msg_id + hold_ms@8），回复 8B（magic + msg_id）。
    #[test]
    fn qsfp_reset_wire_layout() {
        let req = CtrlMsg::QsfpReset {
            msg_id: 0x55,
            hold_ms: 10,
        };
        let mut buf = Cursor::new(Vec::new());
        req.write(&mut buf).expect("write");
        let b = buf.into_inner();
        assert_eq!(b.len(), 12, "QsfpReset 线上长度应为 12B");
        assert_eq!(&b[0..4], &0x00_00_10_03_u32.to_le_bytes(), "msg_type@0");
        assert_eq!(&b[4..8], &0x55_u32.to_le_bytes(), "msg_id@4");
        assert_eq!(&b[8..12], &10_u32.to_le_bytes(), "hold_ms@8");

        let reply = CtrlMsg::QsfpResetReply { msg_id: 0x55 };
        let mut buf = Cursor::new(Vec::new());
        reply.write(&mut buf).expect("write");
        let b = buf.into_inner();
        assert_eq!(b.len(), 8, "QsfpResetReply 线上长度应为 8B");
        assert_eq!(&b[0..4], &0xff_00_10_03_u32.to_le_bytes(), "msg_type@0");
    }

    /// 锁定 GetClk / GetClkReply 的线上布局（与 C 端 `struct GetClk` /
    /// `struct GetClkReply` 一一对应）：请求 8B；回复 20B
    /// （msg_type@0, msg_id@4, clk_src@8, pps_src@12, clk_state@16）。
    #[test]
    fn get_clk_wire_layout() {
        let req = CtrlMsg::GetClk { msg_id: 0x77 };
        let mut buf = Cursor::new(Vec::new());
        req.write(&mut buf).expect("write");
        let b = buf.into_inner();
        assert_eq!(b.len(), 8, "GetClk 线上长度应为 8B");
        assert_eq!(&b[0..4], &0x00_00_00_1c_u32.to_le_bytes(), "msg_type@0");
        assert_eq!(&b[4..8], &0x77_u32.to_le_bytes(), "msg_id@4");

        let reply = CtrlMsg::GetClkReply {
            msg_id: 0x77,
            clk_src: 2,
            pps_src: 1,
            clk_state: 1,
        };
        let mut buf = Cursor::new(Vec::new());
        reply.write(&mut buf).expect("write");
        let b = buf.into_inner();
        assert_eq!(b.len(), 20, "GetClkReply 线上长度应为 20B");
        assert_eq!(&b[0..4], &0xff_00_00_1c_u32.to_le_bytes(), "msg_type@0");
        assert_eq!(&b[4..8], &0x77_u32.to_le_bytes(), "msg_id@4");
        assert_eq!(&b[8..12], &2_u32.to_le_bytes(), "clk_src@8");
        assert_eq!(&b[12..16], &1_u32.to_le_bytes(), "pps_src@12");
        assert_eq!(&b[16..20], &1_u32.to_le_bytes(), "clk_state@16");
    }

    /// 锁定 GetGt / GetGtReply 的线上布局（与 C 端 `struct GetGt` /
    /// `struct GetGtReply` 一一对应）：请求 8B；回复 20B
    /// （msg_type@0, msg_id@4, precursor@8, postcursor@12, txdiff@16）。
    /// 回复体与 `GtSet` 的请求体同构，这里一并钉住 20B。
    #[test]
    fn get_gt_wire_layout() {
        let req = CtrlMsg::GetGt { msg_id: 0x33 };
        let mut buf = Cursor::new(Vec::new());
        req.write(&mut buf).expect("write");
        let b = buf.into_inner();
        assert_eq!(b.len(), 8, "GetGt 线上长度应为 8B");
        assert_eq!(&b[0..4], &0x00_00_11_02_u32.to_le_bytes(), "msg_type@0");
        assert_eq!(&b[4..8], &0x33_u32.to_le_bytes(), "msg_id@4");

        let reply = CtrlMsg::GetGtReply {
            msg_id: 0x33,
            precursor: [7, 6, 5, 4],
            postcursor: [1, 2, 3, 4],
            txdiff: [0, 31, 0, 0],
        };
        let mut buf = Cursor::new(Vec::new());
        reply.write(&mut buf).expect("write");
        let b = buf.into_inner();
        assert_eq!(b.len(), 20, "GetGtReply 线上长度应为 20B");
        assert_eq!(&b[0..4], &0xff_00_11_02_u32.to_le_bytes(), "msg_type@0");
        assert_eq!(&b[4..8], &0x33_u32.to_le_bytes(), "msg_id@4");
        assert_eq!(&b[8..12], &[7, 6, 5, 4], "precursor@8");
        assert_eq!(&b[12..16], &[1, 2, 3, 4], "postcursor@12");
        assert_eq!(&b[16..20], &[0, 31, 0, 0], "txdiff@16");

        // GtSet 请求体与 GetGtReply 同构（只差 magic），同样是 20B
        let set = CtrlMsg::GtSet {
            msg_id: 0x33,
            precursor: [7, 6, 5, 4],
            postcursor: [1, 2, 3, 4],
            txdiff: [0, 31, 0, 0],
        };
        let mut buf = Cursor::new(Vec::new());
        set.write(&mut buf).expect("write");
        let b = buf.into_inner();
        assert_eq!(b.len(), 20, "GtSet 线上长度应为 20B");
        assert_eq!(&b[0..4], &0x00_00_11_01_u32.to_le_bytes(), "msg_type@0");
        assert_eq!(&b[8..12], &[7, 6, 5, 4], "precursor@8");
    }
}
