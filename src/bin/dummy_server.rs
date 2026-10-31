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

    // 时钟：真固件的 setclk_handler 写 PS GPIO pin33/34 与 PPS_SEL 寄存器，
    // get_clk_handler 读的是这些硬件的实际值（不缓存）。dummy 没有硬件，
    // 只能把最近一次 SetClk 的入参存下来当"当前值"。
    let mut clk_src: u32 = 0; // 0 = BUILTIN_10M
    let mut pps_src: u32 = 0; // 0 = GPS_PPS

    // GT 均衡参数：真固件写/读 PL 的三个 AXI GPIO 寄存器（XPAR_GT_*_BASEADDR），
    // 每路 5 bit；dummy 没有硬件，只能记住最后一次 GtSet 的入参。
    let mut gt_precursor: [u8; 4] = [0; 4];
    let mut gt_postcursor: [u8; 4] = [0; 4];
    let mut gt_txdiff: [u8; 4] = [0; 4];

    // QSFP 控制引脚的 dummy 状态。真板卡这三根是 AXI IIC 的 GPO 寄存器
    // （XPAR_AXI_IIC_0_BASEADDR+0x124）的 bit0/1/2，分别驱动 RstL /
    // ModSelL / LPMode，低有效；这里存的就是引脚电平。
    // restl 只能由 QsfpReset 改动，QsfpSet 不碰它 —— 对应固件的剥离。
    let mut qsfp_restl: u32 = 1;
    let mut qsfp_modsell: u32 = 0;
    let mut qsfp_lpmode: u32 = 0;
    let mut qsfp_cdr_ctrl: [u8; 4] = [0xff, 0, 0, 0];
    let mut qsfp_eq_ctrl: [i8; 4] = [0; 4];
    let mut qsfp_adapt_eq: [u8; 4] = [0; 4];
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
            // 时钟：只记下 SetClk 的入参，GetClk 原样回读（真固件读 GPIO 与
            // PPS_SEL 寄存器的实际值，见 gpio.c 的 set_clk_sel / set_pps_sel）。
            SetClk {
                msg_id,
                clk_src: cs,
                pps_src: ps,
            } => {
                println!("SetClk: clk_src={cs} pps_src={ps}");
                clk_src = cs;
                pps_src = ps;
                SetClkReply {
                    msg_id,
                    clk_state: 1, // dummy 假装 LMK04828 已锁定
                }
            }

            GetClk { msg_id } => GetClkReply {
                msg_id,
                clk_src,
                pps_src,
                clk_state: 1,
            },

            // GT 均衡：真固件写/读 PL 的三个 AXI GPIO 寄存器（XPAR_GT_*_BASEADDR），
            // 每路 5 bit 按 pack_gpio20 打包；dummy 直接存数组。
            GtSet {
                msg_id,
                precursor,
                postcursor,
                txdiff,
            } => {
                println!(
                    "GtSet: precursor={precursor:?} postcursor={postcursor:?} txdiff={txdiff:?}"
                );
                gt_precursor = precursor;
                gt_postcursor = postcursor;
                gt_txdiff = txdiff;
                GtSetReply { msg_id }
            }

            GetGt { msg_id } => GetGtReply {
                msg_id,
                precursor: gt_precursor,
                postcursor: gt_postcursor,
                txdiff: gt_txdiff,
            },

            // CMAC 统计：dummy 没有 CMAC，只能给一组「链路健康、无计数」的静态值。
            // 计数器全 0 是刻意的 —— 真板卡上这些值要么在动（BIP/FEC），要么全 0
            // （链路干净），拿这个回复可以验证主机的解析与打印，但不能当链路证据。
            GetCmacStat { msg_id } => GetCmacStatReply {
                msg_id,
                bip_err: [0; 4],
                framing_err: [0; 4],
                fec_err_cnt: [0; 4],
                fec_corrected_cw: 0,
                fec_uncorrected_cw: 0,
                fec_total_cw: 0,
                rx_total_packets: 0,
                rx_good_packets: 0,
                rx_bad_fcs: 0,
                rx_stomped_fcs: 0,
                rx_undersize: 0,
                rx_oversize: 0,
                rx_truncated: 0,
                rx_jabber: 0,
                rx_broadcast: 0,
                rx_unicast: 0,
                rx_multicast: 0,
                tx_total_packets: 0,
                tx_bad_fcs: 0,
                tx_frame_error: 0,
                cycle_count: 0,
                // bit0 status + bit1 aligned，4 lane 全 block lock / lane sync
                rx_status: 0x3,
                block_lock: 0xf,
                lane_sync: 0xf,
                lane_sync_err: 0,
                am_err: 0,
                am_len_err: 0,
                am_repeat_err: 0,
                rsfec_status: 0,
            },

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

            // 只改 ModSelL/LPMode，绝不碰 qsfp_restl：RstL 归 QsfpReset。
            // 真固件的 qsfp_set_handler 也要同样改成 read-modify-write，
            // 否则写 bit1/bit2 会连带把 bit0 清零、顺手复位模块。
            QsfpSet {
                msg_id,
                modsell,
                lpmode,
                cdr_ctrl,
                eq_ctrl,
                adapt_eq,
            } => {
                println!(
                    "QsfpSet: modsell={modsell} lpmode={lpmode} cdr_ctrl={cdr_ctrl:?} \
                     eq_ctrl={eq_ctrl:?} adapt_eq={adapt_eq:?}"
                );
                qsfp_modsell = modsell;
                qsfp_lpmode = lpmode;
                qsfp_cdr_ctrl = cdr_ctrl;
                qsfp_eq_ctrl = eq_ctrl;
                qsfp_adapt_eq = adapt_eq;
                QsfpSetReply {
                    msg_id,
                    restl: qsfp_restl,
                    modsell: qsfp_modsell,
                    lpmode: qsfp_lpmode,
                    cdr_ctrl: qsfp_cdr_ctrl,
                    eq_ctrl: qsfp_eq_ctrl,
                    adapt_eq: qsfp_adapt_eq,
                }
            }

            // 一个动作：RstL 拉低 hold_ms -> 拉高（QSFP28 MSA 只要求 ≥1µs）。
            // hold_ms=0 取最小 1ms，上限 500ms：真固件的 handler 是持 cmd_mutex
            // 跑的，睡多久两个网口的命令线程就堵多久。
            QsfpReset { msg_id, hold_ms } => {
                let hold_ms = if hold_ms == 0 { 1 } else { hold_ms.min(500) };
                qsfp_restl = 0;
                println!("QsfpReset: RstL={qsfp_restl} (0 = 复位中)，保持 {hold_ms} ms");
                std::thread::sleep(std::time::Duration::from_millis(hold_ms as u64));
                qsfp_restl = 1;
                println!("QsfpReset: RstL={qsfp_restl} (1 = 已释放)");
                QsfpResetReply { msg_id }
            }

            // dummy 没有光模块，测量值给固定假数据；在位/控制位与设置项回读状态。
            // 注意 ModPrsL 低有效：1 = absent，0 = present。dummy 没有模块，给 1。
            QsfpInfo { msg_id } => QsfpInfoReply {
                msg_id,
                modsell: qsfp_modsell,
                lpmode: qsfp_lpmode,
                modpresent: 1,
                temperature: 40.0,
                vcc: 3.3,
                tx_bias: [6.0; 4],
                rx_power: [0.5; 4],
                tx_power: [0.5; 4],
                los_lol: [0; 4],
                vcc_temp_alarm: [0; 4],
                cdr_ctrl: qsfp_cdr_ctrl,
                eq_ctrl: qsfp_eq_ctrl,
                adapt_eq: qsfp_adapt_eq,
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
