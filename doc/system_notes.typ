// T510 同步采集系统（syncdaq）操作与验证笔记 — Typst 排版
// 编译: /snap/typst/19/bin/typst compile system_notes.typ
// 源内容由 SYSTEM_NOTES.md 重新整理而来。

#set document(title: "T510 同步采集系统（syncdaq）操作与验证笔记", author: "syncdaq 组", date: datetime(year: 2026, month: 8, day: 24))

#set page(
  paper: "a4",
  margin: (x: 2.2cm, y: 2.4cm),
  numbering: "1 / 1",
  number-align: center + bottom,
)

#set text(font: ("Noto Serif CJK SC",), size: 10.5pt, lang: "zh", region: "cn")
#show raw.where(block: false): set text(font: ("DejaVu Sans Mono", "Noto Sans CJK SC"), size: 9pt)
#show raw.where(block: true): set text(font: ("DejaVu Sans Mono", "Noto Sans CJK SC"), size: 8.5pt)
#show raw.where(block: true): it => block(
  fill: rgb("#f5f5f5"),
  inset: 8pt,
  radius: 3pt,
  width: 100%,
  it,
)

#set heading(numbering: "1.1")
#show heading: set text(font: ("Noto Sans CJK SC",))
#show heading.where(level: 1): it => {
  v(6pt)
  block(above: 1.2em, below: 0.8em)[
    #text(size: 15pt, weight: "bold", fill: rgb("#1a4f8a"))[#it]
  ]
}
#show heading.where(level: 2): it => block(above: 1em, below: 0.6em)[
  #text(size: 12pt, weight: "bold", fill: rgb("#1a4f8a"))[#it]
]
#show heading.where(level: 3): it => block(above: 0.8em, below: 0.5em)[
  #text(size: 11pt, weight: "bold")[#it]
]

#set table(stroke: 0.5pt + rgb("#bbbbbb"), inset: 5pt)
#let cellfill = (x, y) => if y == 0 { rgb("#1a4f8a") } else if calc.even(y) { rgb("#f2f6fa") }
#show table.cell.where(y: 0): set text(weight: "bold", fill: white)

// 提示框（重点/警告）
#let tip(title, body) = block(
  fill: rgb("#eaf3fb"),
  stroke: (left: 3pt + rgb("#1a4f8a")),
  inset: (left: 10pt, top: 7pt, bottom: 7pt, right: 8pt),
  radius: 2pt,
  width: 100%,
)[
  #text(weight: "bold", fill: rgb("#1a4f8a"), font: ("Noto Sans CJK SC",))[#title]
  #v(2pt)
  #body
]

// 警告框
#let warn(title, body) = block(
  fill: rgb("#fff7e6"),
  stroke: (left: 3pt + rgb("#d48806")),
  inset: (left: 10pt, top: 7pt, bottom: 7pt, right: 8pt),
  radius: 2pt,
  width: 100%,
)[
  #text(weight: "bold", fill: rgb("#d48806"), font: ("Noto Sans CJK SC",))[#title]
  #v(2pt)
  #body
]

// ---------------- 标题区 ----------------
#align(center)[
  #v(14pt)
  #text(size: 20pt, weight: "bold", font: ("Noto Sans CJK SC",), fill: rgb("#163d6b"))[T510 同步采集系统（syncdaq）操作与验证笔记]
  #v(5pt)
  #text(size: 10.5pt, fill: rgb("#555555"))[MicroPhase ANTSDR T510 ｜ Xilinx Zynq UltraScale+ RFSoC（xczu47dr）｜ 8 路 I/Q ADC（320 MS/s）]
  #v(4pt)
  #text(size: 9.5pt, fill: rgb("#777777"))[
    2026-08-24 ｜ syncdaq（Rust 主机侧）＋ soapy_syncdaq（SoapySDR 驱动）｜ 100G CMAC（QSFP 4×25G）上行
  ]
  #v(10pt)
  #line(length: 100%, stroke: 0.8pt + rgb("#1a4f8a"))
]

#v(4pt)
#tip[*读法*][
  下次重新对话时读这一篇即可掌握本系统的结构 + 正确操作方法 + 已验证结论。硬件接口契约的详细版见同目录 `HANDOVER.md`。本笔记重点在 _如何操作这块板子、怎么验证确定性延迟/可复现性_。
]

= 一句话概览

这块板子（MicroPhase ANTSDR T510，Xilinx Zynq UltraScale+ RFSoC，器件 `xczu47dr`）是同步采集板：

- *8 路 I/Q ADC*，每路 *320 MS/s*（当前固件；另有 100 MS/s 变体），16-bit，RFDC 下变频。
- 采样数据经 FPGA（BSV）打包成 Eth/IPv4/UDP，从 *100G CMAC（QSFP 4×25G）* 上行给主机。
- 一块板只负责采集；*多板之间靠同一个 PPS（1 Hz）触发 + 共享时钟来实现同步*。
- 板卡受一个 PPS 驱动，同时又在偶数通道（0/2/4/6）采这个 PPS——所以每次 PPS 触发采集的数据应可复现，这正是"确定性延迟/板间同步"验证的基础。

= 软件架构

两个独立 git 仓库：

#table(
  columns: (auto, auto, 1fr),
  fill: cellfill,
  [仓库], [语言], [作用],
  [`syncdaq`], [Rust], [与板卡通信的核心库 + CLI。控制协议（`CtrlMsg`）、100G 数据接收/下抽样管线、C FFI。*主机侧"控制 + 收数据"这一端*。],
  [`soapy_syncdaq`], [C++], [把 `syncdaq` 通过 C FFI 封装成 *SoapySDR 插件*（`libsoapy_syncdaq.so`），给 GNU Radio / gqrx / SDRangel 等用。],
)

- `soapy_syncdaq` 链接 `../syncdaq/target/release/libsyncdaq.a`，两者需放在同一父目录下。
- `syncdaq` 编译：`cd syncdaq && cargo build --release`。

= 网络 / 控制面与数据面

主机侧有两块网卡用于两套通路（本机实测）：

#table(
  columns: (1fr, 1.2fr, 1.4fr, 1fr),
  fill: cellfill,
  [面], [主机网卡], [主机 IP], [说明],
  [*控制面*（1G 管理网）], [`eth0`], [`192.168.5.1/24`], [发指令、收状态],
  [*数据面*（100G）], [`eno1np0`], [`10.100.11.1/24`], [收 100G 数据（MAC `10:70:fd:b3:68:de`）],
)

- *板卡控制地址*：`192.168.5.174:3000`（本网络只有这一块板；若用广播可用 `192.168.5.255`）。
- *板卡 100G 数据打包地址*（`XGbeCfgQuery` 读到的 8 端口配置）：`10.100.11.20–27:3001`（源）→ `10.100.11.1:4000–4007`（目的，即本机 `eno1np0`）。
  - 即 *每路通道一个目的端口 4000–4007*，通道号 = `port_id` = 目的端口 − 4000。
- *设备发现*：`syncdaq` 的 `discover`/`find_device` 用广播 `Query` + `XGbeCfgQuery` 枚举板卡并匹配本机 MAC/IP。

= 控制指令

指令是 YAML 文件（`cmd/*.yaml`），通过 `send_cmd` 发送。常用：

#table(
  columns: (1.1fr, 1fr),
  fill: cellfill,
  [指令], [作用],
  [`Query`], [查状态（`Health::T510Health`：温度、风扇、`smp_rate`、`locked`、`pkt_cnt1`、`axi_frame_cnt1`…）],
  [`XGbeCfgQuery`], [查 8 个 100G 端口打包配置（源/目的 MAC/IP/端口）],
  [`StreamStart` / `StreamStop`], [控制数据流启动/停止（*PPS 对齐生效*，见 §6.3）],
  [`Sync`], [MTS 同步（*开机做一次即可*）],
  [`SetMixer`], [设本振 LO（`sync` 字段=1 表示等下一个 PPS 边沿再设）],
  [`GetMixer`], [读回 RFDC 内部当前本振（每通道 freq/phase）。每次调 `XRFdc_GetMixerSettings()` 现读，不缓存；上电未设过时是 IP 默认 NCO],
  [`SetDSA` / `GetDSA`], [设/查片上衰减（`dsa_value`，0=无衰减）],
  [`init_rfsoc.yaml`], [一个文件包含一套初始化：`SetClk`+`Sync`+8×`XGbeCfgSingle`+`SetMixer`],
)

#warn[*注意*][
  根目录 `SetDSA.yaml` = 全部端口 `dsa_value:0`；`cmd/SetDSA.yaml` = 全部 `dsa_value:12`。
]

= 状态转移（指令 / 状态视角）

板卡从上电到出数据是一条单向链：*上电 → 自动联网 → 主机依次下发 `Sync` / `SetMixer` / `XGbeCfgSingle` / `SetDSA` → `StreamStart`*；
`StreamStop` 只回到「就绪」，不回到「离线」。图、下面的表、以及同目录 `state_machine.md` 里的 YAML 是同一份数据（源：`gen_state_svg.py`）。

#image("syncdaq_state.svg", width: 100%)

== 状态

#table(
  columns: (auto, auto, 1fr),
  fill: cellfill,
  [状态], [进入方式], [含义],
  [关机], [断电], [板卡不在线],
  [开机 / 启动中], [上电], [FSBL → PL → PS，LMK/CMAC/RFDC 初始化],
  [MAC 已配置], [自动], [控制口 MAC 由 PL DNA 派生（失败用 fallback）],
  [等待 DHCP], [自动], [无地址时 UDP 服务不建 socket],
  [在线], [拿到 IP], [UDP :3000 可收指令、可 `Query`（328 B 回包）],
  [已同步], [`Sync.yaml`], [RFDC MTS 完成（前置 `SetClk` 选时钟/PPS 源）],
  [本振已设], [`SetMixer.yaml`], [各通道 LO 频率/相位],
  [100G 已配置], [`XGbeCfgSingle` ×8], [8 个端口的源/目的 MAC、IP、UDP 端口],
  [就绪], [`SetDSA.yaml`], [可开流；上电后必须显式 `SetDSA`（默认大衰减，`GetDSA` 假回读 0）],
  [数据传输中], [`StreamStart.yaml`], [PL `transmitting=1`，8 路 UDP 上行],
)

== 转移

#table(
  columns: (auto, auto, auto, 1fr),
  fill: cellfill,
  [from], [to], [触发], [说明],
  [关机], [开机], [上电], [启动链开始],
  [开机], [MAC 已配置], [自动], [`port_mac()`],
  [MAC 已配置], [等待 DHCP], [自动], [`dhcp_start()`],
  [等待 DHCP], [在线], [拿到 IP], [地址非 0 → UDP 服务建 socket],
  [在线], [已同步], [`Sync.yaml`], [MTS；内部先 `stop_stream()`],
  [已同步], [本振已设], [`SetMixer.yaml`], [`sync=1` 时等 PPS 边沿],
  [本振已设], [100G 已配置], [`XGbeCfgSingle` ×8], [逐口装订],
  [100G 已配置], [就绪], [`SetDSA.yaml`], [上电后必须做],
  [就绪], [数据传输中], [`StreamStart.yaml`], [PPS 对齐，0~1 s 生效；帧计数清零],
  [数据传输中], [就绪], [`StreamStop.yaml`], [PPS 对齐，等 `transmitting=0`],
  [在线], [开机], [`Reboot.yaml`], [软 POR / 看门狗],
  [就绪], [关机], [断电], [直接断电],
)

#tip[*两条容易踩的*][
  ① `Sync` / `SetClk` 内部先 `stop_stream()`——传输中发这两个会*强制停流*；
  ② `StreamStop` 后至少等 *2 s* 再数包，否则数到的是「还没生效期间」的包。
]

= 关键操作流程

== 开机后的初始化（只做一次）

```bash
cd syncdaq
./send_cmd.sh -a 192.168.5.174:3000 -c init_rfsoc.yaml -d 1 -t 10   # 完整初始化
./send_cmd.sh -a 192.168.5.174:3000 -c cmd/Sync.yaml   -d 1 -t 3    # Sync：开机一次足够，不要每次都做
```

#tip[*提醒*][
  `repeat_trig.sh` 里 `while` 循环每次迭代都发 `Sync`/`SetMixer`，那是旧习惯；正确做法是_开机做一次_即可。
]

== 用 `send_cmd.sh`，不要用裸 `send_cmd`

- *`send_cmd.sh`* 会把命令对齐到"下一个 整数秒 + 0.5s"的边界再发送（`run_at_half_second` 逻辑）。
- `send_cmd`（裸写）*任意时刻发*，PPS 对齐逻辑时灵时不灵——用裸 send_cmd 时 Stop 经常失效；改用 `send_cmd.sh` 后全部正确。
- 用法：`./send_cmd.sh -a 192.168.5.174:3000 -c cmd/Query.yaml -d 1 -t 3`

== Start/Stop 是 PPS 对齐的（核心机制）

- *任何时刻发送的 `StreamStart`/`StreamStop`，只在下一个 PPS 上升沿才真正生效* → 命令后有 0~1 s 延迟。
- 因此 *Stop 与 Start 之间必须间隔 ≥1 s*（让 Stop 真正生效后再 Start）。
- *Stop 生效的表现*：数据停发（收包数≈0）。验证方法：`StreamStop` 后 *等 2 s* 再数包。

== 采集一帧/一段数据的正确顺序

```text
1. StreamStop                         # 确保处于停止状态
2. 等待 ≥1 s（>1 个 PPS）并确认已停    # 收包数应为 0（若没停，需重新初始化）
3. 把接收 socket 先准备好（bind 并进入等待）
4. 再 StreamStart（用 send_cmd.sh，PPS 对齐）
5. 立即开始收包 → 抓到"第 0 帧"       # 首包 pkt_cnt 应为 0
6. 需要时 StreamStop
```

#warn[*必须*][
  必须在 StreamStart 之前就让接收程序就绪等待，否则抓不到第 0 帧，会随机抓到流中间的包（导致对比无效）。
]

== 帧计数复位

- *每次 Start（PPS 对齐）都会让帧计数清零，然后按帧递增*。
- 观察到的计数：`pkt_cnt` 与 `axi_frame_cnt1` 都复位。但*几乎不可能看到 `pkt_cnt==0`*（复位后马上在涨）；正确做法是用"先就绪 socket、再 Start"抓到真正的第 0 帧（那时 `pkt_cnt` 就是 0）。
- 每次采集之间（Stop→Start）`pkt_cnt` 会从 0 重新递增，所以*不同次采集的 `pkt_cnt` 起始应该都是 0/很小*，而非连续的大值。

= 确定性延迟 / 可复现性验证方法

因为板子受 PPS 驱动并采这个 PPS，所以：*若确定性延迟成立 → 每次 PPS 触发采集，数据应该大体一样*（第 0 帧波形一致、PPS 尖峰在固定位置/幅度）。

== 验证步骤

1. 开机 init + `Sync`（一次即可）。
2. 循环多次（每次一组）：
   - `StreamStop` → 等 ≥1 s（>1 PPS）→ 确认停（收包数≈0）。
   - *就绪接收 socket* → `StreamStart`（`send_cmd.sh`，PPS 对齐、帧计数清零）。
   - 收前 *N 帧*（如 20 帧，每帧 8192B 负载 → 4096 个 i16）。
   - `StreamStop`。
3. 对比各次的*第 0 帧*（首包负载 4096 个 i16）：
   - *Pearson 相关*（应 ≈ 1）。
   - *逐点差均值/最大*（相对信号幅度应很小）。
   - 检查每帧首包 `pkt_cnt == 0`。

== 已实测结论（DSA=0，板子健康）

```text
frame-0 pkt_cnt 均 = 0                      （帧计数正确清零）
信号幅度：mean|val|≈299  max|val|≈16557      （真实大信号，非噪声；PPS 尖峰）
a 组 vs b 组：corr≈0.999967
              mean|diff|≈6.0   max|diff|≈28 （占满量程 32767 的 ~0.17%）
```

#block[
  → *确定性延迟成立、采集可复现*。这就是多板互联同步的*单板前提*：只要多块板接同一 PPS、各自按上升沿启动，各板第 0 帧就对齐到同一时刻。*要最终确认"板间"同步，需 ≥2 块板接同一 PPS 对比*。
]

== 重新验证（2026-08-23 晚，`determinism_test.py`，5 组，通道 0/端口 4000，每帧 4096 i16）

```text
frame-0 pkt_cnt 均 = 0                        （帧计数复位正确）
信号幅度：mean|val|≈298（297.7~298.2）  max|val|≈16463（16450~16473）
Pearson 相关（全部 10 对）：≈0.99996（0.999963~0.999967）
逐点差：mean≈6.0~6.2   max≈30~105（满量程 0.09%~0.32%）
```

→ 结论与上次一致：*单板确定性延迟成立、采集可复现*。测试后板子仍健康（`locked=0xf`、PPS tick diff OK、`pkt_cnt2/axi_frame_cnt2=0`，未冻结）。

#tip[*一键复跑*][
  `cd syncdaq && python3 determinism_test.py --groups 5 --n 20`；单组探针：`--dryrun`。脚本已按「send_cmd 半秒对齐 + 先就绪 socket + Start 抓帧0 + Stop」实现。
]

=== 板间同步的判定逻辑（无需第二块板的逻辑推断 + 待实测项）

*单板确定性已证明什么*（§7 复测结论）：

- PPS 上升沿 → 板子送出第 0 帧采样，这段*延迟每次为常数*；
- *ADC 采样相位被锚定到 PPS*——直接证据是 frame-0 里的 PPS 尖峰*位置/幅度固定*（max≈16463），若相位没锁到 PPS，不同周期启动的尖峰会乱跳。
- 这是板间同步需要的"锚点"。

*板间同步（真·板间）还需要的那一个量*：

```text
板间相位偏移 = (B板 PPS→采样相位) − (A板 PPS→采样相位)
```

- 单板确定性只保证*每块板的这一项是常数*（→ 偏移为常数，可测/可校准），
- 但*不给出绝对零位差*：各板时钟走线、SYSREF 对齐、PPS 到达时延都可能引入固定的 off-by-几采样。
- 类比：单板确定性 ≈ "两块表都走得准且起步一致"；板间相位偏移 ≈ "两块表的零位差"。

*逻辑结论*：单板可复现性是板间同步的*必要*前提（已钉死）；剩余的是一个*待实测的常数*。若两块板共时钟/SYSREF + 共 PPS（同一接线），该值大概率≈0，但它得经验测量，不能从单板数据推导。

*待办（拿到第 2 块板时照做）*：

1. 两块板接*同一 PPS + 同一共享时钟/SYSREF*。
2. 各跑一遍 `determinism_test.py`（或主机同时 bind 两块板同通道端口），对比 *A 板 frame-0 vs B 板 frame-0* 的相关系数与逐点差。
3. 结果直接量出 *"板间相位偏移≈几采样 + corr"* —— 接近 1、首帧对齐即同步达成；否则按 offset 校准。

= 踩过的坑（务必记住）

1. *反复快速 Stop/Start 会把板子"搞冻结"*：数据路径卡死（Stop 失效、`pkt_cnt` 一直涨、收不到数据）。
   - *恢复方法*：重新初始化：`init_rfsoc.yaml` → `Sync` → `StreamStart`，即可恢复（恢复后 `pkt_cnt` 会从一个小值重新计）。
   - 用正确流程（`send_cmd.sh` + ≥1 s 间隔）就不会触发。
2. *裸 `send_cmd`（任意时刻）*：PPS 对齐的 Start/Stop 不可靠，Stop 可能不生效。*改用 `send_cmd.sh`。*
3. *Stop 之后立刻数包*：会数到"没生效期间的包"。*等 ≥2 s 再数。*
4. *没就绪就 Start*：抓不到第 0 帧，抓的是流中间的包（对比无效）。*先就绪 socket，再 Start。*
5. 看到 `pkt_cnt` 是几百万/1 亿：说明板子处于自由运行（或冻结）而非 PPS 对齐采集状态。*正确时应从 0/小值开始。*
6. 之前"偶数通道只有 ±7 噪声"的错觉：是板子*冻结/未对齐*时采到的；正确流程下是 *~16557 的大信号*。
7. *DSA 已是 0*：`GetDSA` 回读为 0；再设 0 不会让幅度变大。要对比 DSA 效果可设 12（幅度约降 4 倍）。

= 数据包格式（100G 载荷面，已验证）

UDP 载荷 = *8224 字节* = 32 字节 MetaData + *8192 字节*采样。MetaData（小端，与 `syncdaq/src/payload.rs` 的 `Payload` 结构一致）：

#table(
  columns: (auto, auto, 1.6fr),
  fill: cellfill,
  [字段], [值], [说明],
  [`head_magic`], [`0xAA55AA55`], [线上字节 `55 AA 55 AA`],
  [`version`], [`0x11223344`], [],
  [`port_id`], [`0..7`], [通道号],
  [`data_type`], [`16`], [],
  [`pkt_cnt`], [u64], [通道内包计数，Start 时清零],
  [`tail_magic`], [`0xAA55AA55_44332211`], [],
)

负载 = 16-bit I/Q 采样（`N_BYTE_PER_FRAME=8192`）。区分通道靠 `port_id` 及目的端口 4000–4007。

= 常用命令速查

```bash
cd /home/user/src/syncdaq

# 状态查询（最简单）
./send_cmd.sh -a 192.168.5.174:3000 -c cmd/Query.yaml -d 1 -t 3

# 查 100G 打包配置
./send_cmd.sh -a 192.168.5.174:3000 -c cmd/XGbeCfgQuery.yaml -d 1 -t 3

# 开机初始化（一次）+ Sync（一次）
./send_cmd.sh -a 192.168.5.174:3000 -c init_rfsoc.yaml -d 1 -t 10
./send_cmd.sh -a 192.168.5.174:3000 -c cmd/Sync.yaml -d 1 -t 3

# 启动/停止流
./send_cmd.sh -a 192.168.5.174:3000 -c cmd/StreamStart.yaml -d 1 -t 2
./send_cmd.sh -a 192.168.5.174:3000 -c cmd/StreamStop.yaml -d 1 -t 2

# 收数据（Python 直接 bind 主机 100G 口端口）：0.0.0.0:4000..4007
# 实验/对比脚本：见本会话用过的 Python（send_cmd.sh + 就绪 socket + StreamStart + 抓帧0）
```

主机上收数据：`bind(("0.0.0.0", 4000+port_id))` 即可收到该通道的 8224B UDP 包。

= 关键文件路径

- 交接文档：`/home/user/src/HANDOVER.md`（FPGA 硬件接口契约）
- 用户测试流程：`/home/user/src/syncdaq/repeat_trig.sh`（注意其循环里的 Sync 是旧习惯，开机一次即可）
- `send_cmd.sh`：半秒对齐的命令发送器（*务必用这个*）
- *`determinism_test.py`*：单板确定性/可复现性自动验证（半秒对齐 + 先就绪 socket + Start 抓帧0 + Stop + Pearson/逐点差对比）
- 命令模板：`/home/user/src/syncdaq/cmd/*.yaml`
- DSA：根 `SetDSA.yaml`(0) vs `cmd/SetDSA.yaml`(12)
- SoapySDR 驱动：`/home/user/src/soapy_syncdaq/`

= 当前已知状态（本次会话结束时）

- 板卡 `192.168.5.174` 健康：`locked=0xf`、`smp_rate=320 MSps`、PPS（tick diff=10M）正常、DSA=0。
- 网络上一共发现 *1 块*板。
- 确定性延迟/单板可复现性：*已验证成立*（corr≈0.99997，frame-0 一致）。
  - *2026-08-23 晚用 `determinism_test.py` 重新验证（5 组，通道0）*：corr≈0.99996、mean|diff|≈6.0、frame-0 pkt_cnt=0，结论依旧成立；测试后板子未冻结。
- 未做：*真"板间"同步对比*（需要第 2 块板接同一 PPS）。

= firdecim2 性能与丢包定位

== 性能为何本机跟不上

- *两级滤波的作用*：`resample2`（1/2 半带滤波）会把采样率降到输入的一半，*用于下抽样*；`fir_symmetric_full_rate`（全速率 FIR）*不改变采样率*，其用途是*处理前级 1/2 半带滤波并不彻底的带外截止*，以免最终数据的用户观察到*混叠（aliasing）*。两者级联：先 1/2 下抽样，再做全速率抗混叠。
- *结构*：`mod.rs` 用 `std::simd` portable SIMD，`Simd<i32,16>`（512-bit = AVX-512）与 `Simd<i32,8>`（256-bit = AVX2）。管线*单线程逐级串联*（收包→resample2→抗混叠FIR→主循环），`capture_pipeline_decim` *每个进程绑定单一 UDP 端口 = 单通道*，8 通道需开 8 进程各占一核。
- *本机二进制只含 `ymm`（256-bit/AVX2），无 `zmm`（AVX-512）*。`rustc -C target-cpu=native` 在本机解析出 `avx/avx2/fma`，*无 `avx512`*。i9-13900H（Raptor Lake 移动端 13 代）也没有 AVX-512 → *两台机器都用 AVX2（256-bit），512-bit `Simd` 在两边都被拆成 2×256-bit*，SIMD 宽度不是差异来源。
- *CPU 差异*：本机 Xeon E-2286M（8核/16线程，基频2.40GHz；实测 AVX2 多核持续 ≈3.4–3.5GHz，单核可到 ~4.6GHz，`powersave` governor）；i9-13900H（14核6P+8E、P-core 睿频~5.4GHz、IPC 更高）→ *单线程约 1.5–1.8×*。
- *实测单通道吞吐（本机 AVX2 预编译）*：resample2 `streams`≈391 Msamp/s（输出）、抗混叠FIR `streams`≈384 Msamp/s（满速率）。对单通道 320 MS/s 有富余（`fir q` 稳定 0/1）。
- 结论：单通道下 FIR/抽取计算不是瓶颈；i9"够用"主要是它单核更快+核更多掩盖了短板。

== 丢包定位与根因（*每通道核数需求受限*，不是网卡、不是 FIR 算力）

*观察（都为真，但只是现象）：*

- 8 通道同时发（100G 网卡 `eno1np0` 收 ≈1.25M 包/秒 ≈10.24 GB/s）→ `capture_pipeline_decim` 单进程绑定 4000 时 *丢包 3.5%→1.2%（20s 丢 ~36,833 包）*，`fir q` 稳在 0/1（FIR 没瓶颈），主循环 `queue cnt: 0`。
- 只留通道 0（src/dst MAC 置 0 禁用另 7 通道）→ 网卡 155k 包/秒 → *`0 pkts dropped`*。
- Soapy 驱动单流同样不丢包：`driver=syncdaq,ctrl_ip=192.168.5.174,port_id=0,shifts=15,firshift=8` → 底层 syncdaq *`0 pkts dropped`*；Python readStream 达到率 89%（buf=2048）→ 98%（buf=16384）是 Python 循环开销，不是数据丢失。

*根因（用分级实测钉死的）：*

- *NIC 侧不是瓶颈*：`mlx5_core`（ConnectX），*16 RX 队列 + RSS 已启用*，MSI-X 各队列中断已分散到不同 CPU。网卡本身能扛。
- *瓶颈是主机处理管线的"每通道核数需求"*：`capture_pipeline_decim`/soapy 里每通道是多线程流水线（收包 + resample2 + 抗混叠 FIR + 主循环），*每个 stage 独立线程、各自吃满一个核*。
- *分级实测（单通道 320 MS/s，管线钉到不同核数，`SYNCDAQ_NO_PIN=1`+`taskset`）*：

#table(
  columns: (auto, auto),
  fill: cellfill,
  [每通道核数], [丢包],
  [*1*], [*21%*（q≈1.3万，收包→resample2 积压，跟不动）],
  [*2*], [*≈0*（勉强跟上）],
  [*3*], [*0*（q=0，干净）],
  [*4*], [*0*（q=0，干净）],
)

#warn[*一句话*][
  单流不丢包；*多流 320 MS/s 丢包的根因是"主机每通道要 2–3 核、机器只有 8 物理核"*——8 通道需 ~16–24 核，远超 8 核，实际上限约 *3–4 通道 320 MS/s*。与网卡、FIR 算力无关，是可投入的物理核数量受限。
]

== 若确要 8 通道实时（后续方向，按代价低→高）

- *接受 3–4 通道（320 MS/s）*：把每通道的收包/resample2/FIR 分到不同的物理核（`SYNCDAQ_NO_PIN` + 每通道给 2–3 个独立核）→ 各自 0 丢包。（核数不够，这是 8 核机器上限。）
- *降数据率*：用 *100 MS/s 固件变体* → 每通道大幅变轻，8 通道在 8 核上大概率可行——*最省事的务实路径*。
- *换更多核的机器*（如 ≥16 物理核；i9-13900H 的 14 核对 8×320 仍偏紧）。
- *优化管线到"每通道 ≤1 核"*（`recvmmsg` 批量收降收包核负载、合并 stage、更高效 FIR）——*根本解法，但属于改系统/改代码*。

== 本会话新增工具/配置

- `syncdaq/aligned_send.sh`：半秒对齐发送 helper（直接调用预编译 `target/release/send_cmd`，同 `send_cmd.sh` 但对齐后免 cargo 重新编译）。
- `syncdaq/cmd/XGbeCfgSingle_one.yaml` / `_all8.yaml`：只留通道 0 / 恢复全部 8 通道的 100G 打包配置。
- `soapy_syncdaq/soapy_nodrop_test.py`：Soapy 单流不丢包自检（判据 = stderr 的 `pkts dropped` + 达到率随 buffer 提升）。
- 禁用某通道发包：写该通道 `dst_mac=[0,0,0,0,0,0]`（`src_mac` 一并置 0）。

#v(12pt)
#align(center)[
  #line(length: 40%, stroke: 0.5pt + rgb("#aaaaaa"))
  #v(4pt)
  #text(size: 9pt, fill: rgb("#888888"))[本笔记由 AI 助手根据 syncdaq 系统操作与验证过程整理 ｜ 由 Typst 排版生成 ｜ 2026-08-24]
]
