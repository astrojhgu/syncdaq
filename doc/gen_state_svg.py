#!/usr/bin/env python3
"""生成 SyncDAQ 板卡状态转移图（单张 SVG）。

布局是手工摆的：两行 + 一个数据流子状态，蛇形走向。
- 行 1（从左到右）：关机 → 开机 → MAC → 等待 DHCP → 在线
- 行 2（从右到左）：在线 → 已同步 → 本振 → 100G → 就绪
- 行 3：就绪 ↔ 数据传输中（竖直）

改图只改 NODES / EDGES / 坐标即可，不依赖布局引擎。
"""
from pathlib import Path

W, H = 1500, 820

STYLE = {
    "power":  ("#fdecea", "#c0392b"),
    "boot":   ("#fff4e0", "#d48806"),
    "net":    ("#eaf3fb", "#1a4f8a"),
    "cfg":    ("#e8f5e9", "#2e7d32"),
    "stream": ("#f3e8fd", "#6a1b9a"),
}

# id, cx, cy, 标题, 副标题, 类别, 是否胶囊
NODES = [
    ("off",    190, 165, "关机", "", "power", True),
    ("boot",   450, 165, "开机 / 启动中", "FSBL → PL → PS", "boot", False),
    ("mac",    710, 165, "MAC 已配置", "（自动：PL DNA → MAC）", "boot", False),
    ("dhcp",   970, 165, "等待 DHCP", "（自动）", "net", False),
    ("online", 1230, 165, "在线", "UDP :3000", "net", False),
    ("synced", 1230, 510, "已同步", "MTS", "cfg", False),
    ("lo",     970, 510, "本振已设", "", "cfg", False),
    ("xgbe",   710, 510, "100G 已配置", "8 端口 MAC / IP / 端口", "cfg", False),
    ("ready",  450, 510, "就绪", "（SetDSA 已设）", "cfg", True),
    ("tx",     450, 690, "数据传输中", "", "stream", True),
]

NW, NH = 210, 80

# a, b, 标签, 路径点列表（空 = 直线），标签位置（None = 边中点）
EDGES = [
    ("off", "boot", "上电", [], None),
    ("boot", "mac", "自动", [], None),
    ("mac", "dhcp", "自动", [], None),
    ("dhcp", "online", "拿到 IP", [], None),
    ("online", "synced", "Sync.yaml", [], None),
    ("synced", "lo", "SetMixer.yaml", [], None),
    ("lo", "xgbe", "XGbeCfgSingle ×8", [], None),
    ("xgbe", "ready", "SetDSA.yaml", [], None),
    # 竖直线对：左侧向下 StreamStart，右侧向上 StreamStop
    ("ready", "tx", "StreamStart.yaml",
     [(430, 550), (430, 650)], (418, 600)),
    ("tx", "ready", "StreamStop.yaml",
     [(470, 650), (470, 550)], (482, 600)),
    # Reboot：从在线顶部绕到开机顶部
    ("online", "boot", "Reboot.yaml（任意状态）",
     [(1230, 125), (1230, 80), (450, 80), (450, 125)], (840, 71)),
    # 断电：从就绪左侧绕到关机左侧
    ("ready", "off", "断电",
     [(345, 510), (20, 510), (20, 165), (85, 165)], (200, 495)),
]


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def node_svg(nid, cx, cy, title, sub, kind, capsule, by_id):
    fill, stroke = STYLE[kind]
    x, y = cx - NW / 2, cy - NH / 2
    rx = 40 if capsule else 8
    out = [f'<rect x="{x}" y="{y}" width="{NW}" height="{NH}" rx="{rx}" '
           f'fill="{fill}" stroke="{stroke}" stroke-width="1.8"/>']
    if sub:
        out.append(f'<text x="{cx}" y="{cy - 4}" text-anchor="middle" '
                   f'font-size="15" font-weight="600" fill="#1a1a1a">{esc(title)}</text>')
        out.append(f'<text x="{cx}" y="{cy + 20}" text-anchor="middle" '
                   f'font-size="12" fill="#444">{esc(sub)}</text>')
    else:
        out.append(f'<text x="{cx}" y="{cy + 5}" text-anchor="middle" '
                   f'font-size="15" font-weight="600" fill="#1a1a1a">{esc(title)}</text>')
    return "".join(out)


def edge_svg(a, b, label, waypoints, label_pos, by_id):
    ax, ay = by_id[a][1], by_id[a][2]
    bx, by = by_id[b][1], by_id[b][2]
    if waypoints:
        d = "M " + " L ".join(f"{x} {y}" for x, y in waypoints)
        mx, my = label_pos if label_pos else waypoints[0]
    else:
        # 水平或竖直线：从节点边缘出发
        if abs(ay - by) < 1:  # 水平
            sx = ax + (NW / 2 if bx > ax else -NW / 2)
            ex = bx + (-NW / 2 if bx > ax else NW / 2)
            d = f"M {sx} {ay} L {ex} {by}"
            mx, my = (sx + ex) / 2, ay - 50
        else:  # 竖直
            sy = ay + (NH / 2 if by > ay else -NH / 2)
            ey = by + (-NH / 2 if by > ay else NH / 2)
            d = f"M {ax} {sy} L {bx} {ey}"
            mx, my = ax + 8, (sy + ey) / 2
    out = [f'<path d="{d}" fill="none" stroke="#2b5f9e" stroke-width="1.8" '
           f'marker-end="url(#arrow)"/>']
    anchor = "middle"
    if waypoints and label_pos:
        anchor = "start" if mx > 450 else "end"
        mx += 6 if anchor == "start" else -6
    out.append(f'<text x="{mx}" y="{my}" text-anchor="{anchor}" font-size="12.5" '
               f'fill="#2b5f9e" paint-order="stroke" stroke="#ffffff" stroke-width="4" '
               f'stroke-linejoin="round">{esc(label)}</text>')
    return "".join(out)


def main():
    by_id = {n[0]: n for n in NODES}
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="100%" height="100%" '
        f'viewBox="0 0 {W} {H}" preserveAspectRatio="xMidYMid meet" '
        f'font-family="Noto Sans CJK SC, Source Han Sans SC, sans-serif">',
        '<defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" '
        'markerWidth="7" markerHeight="7" orient="auto-start-reverse">'
        '<path d="M 0 0 L 10 5 L 0 10 z" fill="#2b5f9e"/></marker></defs>',
        f'<rect x="0" y="0" width="{W}" height="{H}" fill="#ffffff"/>',
        # 标题
        f'<text x="{W / 2}" y="34" text-anchor="middle" font-size="24" '
        f'font-weight="700" fill="#163d6b">SyncDAQ 板卡状态转移图</text>',
        # 容器
        '<rect x="60" y="95" width="1340" height="140" rx="10" fill="#fffdf7" '
        'stroke="#d48806" stroke-width="1.5" stroke-dasharray="7 5"/>',
        '<text x="90" y="122" font-size="17" font-weight="600" fill="#8a5a00">① 上电与联网</text>',
        '<rect x="60" y="430" width="1340" height="330" rx="10" fill="#f7fdf8" '
        'stroke="#2e7d32" stroke-width="1.5" stroke-dasharray="7 5"/>',
        '<text x="90" y="457" font-size="17" font-weight="600" fill="#1b5e20">② 同步与数据流</text>',
    ]
    for e in EDGES:
        parts.append(edge_svg(*e, by_id=by_id))
    for n in NODES:
        parts.append(node_svg(*n, by_id=by_id))
    parts.append("</svg>")
    out = Path(__file__).with_name("syncdaq_state.svg")
    out.write_text("\n".join(parts), encoding="utf-8")
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
