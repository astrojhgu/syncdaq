#!/usr/bin/env python3
"""CMAC 链路质量采样器：连续读 GetCmacStat，自动差分算速率。

背景：固件里的 CMAC 统计寄存器是「Tick 快照 + 清零累加器」机制 ——
每次 GetCmacStat 都会写一次 TICK_REG：把累加器推入快照寄存器、同时清零，
然后读回快照。所以**每次回复里的绝对值就是「上次本命令至今」这个窗口的
计数，不需要再做差分**：速率 = 该值 / 两次调用的时间间隔。

（这一点实测踩过：最初按「快照=上一次窗口、需要差分」实现，结果连采两次
差值恒为 0 —— 因为每次读到的本来就是同一个窗口长度的计数。）

能测的东西分两层：
  · 物理层（零业务流量也在动）：block_lock / lane_sync / BIP / RS-FEC 码字
  · MAC 层（需要主机往板子 100G 口发点包）：rx_* 帧统计

用法：
  utils/cmac_stat.py -a 192.168.5.164:3000                # 采 10 次、间隔 1 s
  utils/cmac_stat.py -a 192.168.5.164:3000 -n 60 -i 0.5   # 跑 30 s
  utils/cmac_stat.py -a ... --raw                         # 只打原始计数，不差分

判据（经验值，320 工程 + 当前模块实测）：
  · aligned=1、block_lock=0xf、lane_sync=0xf   → RX 方向 PCS 已对齐
  · BIP 速率 = 0 且 FEC 未纠正 = 0             → 链路干净
  · FEC 纠正码字率 < 1e-7                       → 余量充足（实测 0）
  · 未纠正码字 > 0                              → 已经在丢包，别再加大流量
"""

import argparse
import socket
import struct
import sys
import time

REQ_MAGIC = 0x00001201
REPLY_LEN = 280

# 与 ctrl_msg.rs 的 GetCmacStatReply 一一对应：msg_type, msg_id, 30×u64, 8×u32
_FMT = "<II" + "Q" * 30 + "I" * 8

_U64_NAMES = [
    "bip0", "bip1", "bip2", "bip3",
    "framing0", "framing1", "framing2", "framing3",
    "fec_err0", "fec_err1", "fec_err2", "fec_err3",
    "fec_corrected_cw", "fec_uncorrected_cw", "fec_total_cw",
    "rx_total_packets", "rx_good_packets", "rx_bad_fcs", "rx_stomped_fcs",
    "rx_undersize", "rx_oversize", "rx_truncated", "rx_jabber",
    "rx_broadcast", "rx_unicast", "rx_multicast",
    "tx_total_packets", "tx_bad_fcs", "tx_frame_error",
    "cycle_count",
]
_U32_NAMES = [
    "rx_status", "block_lock", "lane_sync", "lane_sync_err",
    "am_err", "am_len_err", "am_repeat_err", "rsfec_status",
]


def query(addr, timeout=3.0, msg_id=0x5A5A):
    """发一次 GetCmacStat，返回 dict。"""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(timeout)
    try:
        sock.sendto(struct.pack("<II", REQ_MAGIC, msg_id), addr)
        while True:
            data, _ = sock.recvfrom(2048)
            if len(data) != REPLY_LEN:
                continue  # 上一轮的迟到回复/别的命令，丢掉
            vals = struct.unpack(_FMT, data)
            if vals[0] != 0xFF001201 or vals[1] != msg_id:
                continue
            break
    finally:
        sock.close()

    st = dict(zip(_U64_NAMES, vals[2:32]))
    st.update(zip(_U32_NAMES, vals[32:40]))
    st["bip_sum"] = sum(st[f"bip{n}"] for n in range(4))
    st["framing_sum"] = sum(st[f"framing{n}"] for n in range(4))
    return st


def link_ok(st):
    """PCS/RS-FEC 是否认为链路可用。"""
    aligned = (st["rx_status"] >> 1) & 1
    return aligned and st["block_lock"] == 0xF and st["lane_sync"] == 0xF


def rate(st, secs):
    """把「本窗口的绝对计数」换算成每秒速率；secs<=0 返回 0。"""
    if secs <= 0:
        return {k: 0.0 for k in ["bip", "framing", "ccw", "ucw", "cw", "rxpkt", "txpkt", "cycle"]}
    return {
        "bip": st["bip_sum"] / secs,
        "framing": st["framing_sum"] / secs,
        "ccw": st["fec_corrected_cw"] / secs,
        "ucw": st["fec_uncorrected_cw"] / secs,
        "cw": st["fec_total_cw"] / secs,
        "rxpkt": st["rx_total_packets"] / secs,
        "txpkt": st["tx_total_packets"] / secs,
        "cycle": st["cycle_count"] / secs,
    }


def judge(st, r):
    """给一行结论。"""
    if not link_ok(st):
        return "LINK DOWN（aligned/block_lock/lane_sync 未全 1）"
    if st["fec_uncorrected_cw"] > 0:
        return "!! 有未纠正码字：正在丢包"
    if r["cw"] > 0 and r["ccw"] / r["cw"] > 1e-7:
        return "码字错误率偏高，余量不足"
    if r["bip"] > 0:
        return "BIP 在涨（前 FEC 有误码）"
    return "OK"


def main():
    ap = argparse.ArgumentParser(description="CMAC 链路质量采样器")
    ap.add_argument("-a", "--addr", required=True, help="板卡控制口 ip:port")
    ap.add_argument("-n", "--count", type=int, default=10, help="采样次数（默认 10）")
    ap.add_argument("-i", "--interval", type=float, default=1.0, help="采样间隔秒（默认 1）")
    ap.add_argument("-t", "--timeout", type=float, default=3.0, help="单次超时秒")
    ap.add_argument("--raw", action="store_true", help="只打印原始计数，不差分")
    args = ap.parse_args()

    host, _, port = args.addr.rpartition(":")
    addr = (host, int(port))

    prev_t = None
    for i in range(args.count):
        t = time.time()
        try:
            st = query(addr, args.timeout)
        except socket.timeout:
            print(f"[{i:3d}] 超时：{args.addr} 没有回复", file=sys.stderr)
            return 1
        except Exception as e:  # noqa: BLE001
            print(f"[{i:3d}] 失败：{e}", file=sys.stderr)
            return 1

        dt = (t - prev_t) if prev_t is not None else 0.0

        if args.raw or dt <= 0:
            # 第一次拿到的窗口是「上电至今」（含启动期脏数据），只能当基线
            tag = "基线(上电至今)" if not args.raw else "raw"
            print(
                f"[{i:3d}] {tag} aligned={(st['rx_status']>>1)&1} "
                f"blk=0x{st['block_lock']:x} sync=0x{st['lane_sync']:x} "
                f"rx_status=0x{st['rx_status']:08x} "
                f"bip={st['bip_sum']} framing={st['framing_sum']} "
                f"fec_ccw={st['fec_corrected_cw']} fec_ucw={st['fec_uncorrected_cw']} "
                f"fec_cw={st['fec_total_cw']} "
                f"rx={st['rx_total_packets']} tx={st['tx_total_packets']} "
                f"cycle={st['cycle_count']}"
            )
        else:
            r = rate(st, dt)
            cw_err = (r["ccw"] / r["cw"]) if r["cw"] else 0.0
            print(
                f"[{i:3d}] 窗口={dt:5.2f}s  BIP/s={r['bip']:9.2f}  "
                f"FEC纠正/s={r['ccw']:10.1f}  未纠正/s={r['ucw']:6.1f}  "
                f"码字/s={r['cw']/1e6:6.2f}M 码字错误率={cw_err:.2e}  "
                f"RX pps={r['rxpkt']:9.1f} TX pps={r['txpkt']:9.1f}  "
                f"core_clk={r['cycle']/1e6:6.2f}MHz  {judge(st, r)}"
            )

        prev_t = t
        if i + 1 < args.count:
            time.sleep(args.interval)
    return 0


if __name__ == "__main__":
    sys.exit(main())
