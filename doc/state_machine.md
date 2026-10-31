# SyncDAQ 板卡状态转移图

- 矢量图：`syncdaq_state.svg` ｜ 位图：`syncdaq_state.png`
- 源：`gen_state_svg.py`（手写布局；改 `NODES` / `EDGES` 里的文本和坐标即可）
- 重绘：`python3 gen_state_svg.py`
- 读法：**边上的文字 = 触发这条转移的指令或事件**。橙色 = 开机自动流程；蓝色 = 网络；绿色 = 指令配置；紫色 = 数据流；红色 = 关机。

## 状态

| id | 状态 | 类别 | 含义 | 进入方式 |
|---|---|---|---|---|
| `off` | 关机 | power | 板卡断电，不在线 | 断电 |
| `boot` | 开机 / 启动中 | boot | FSBL → PL → PS，LMK/CMAC/RFDC 初始化 | 上电 |
| `mac` | MAC 已配置 | boot | 控制口 MAC 由 PL DNA 派生（失败用 fallback） | 自动 |
| `dhcp` | 等待 DHCP | net | `dhcp_start()`，无地址时 UDP 服务不建 socket | 自动 |
| `online` | 在线 | net | 拿到 IP，UDP :3000 可收指令、可 `Query`（336 B 回包） | 拿到 IP |
| `synced` | 已同步 | cfg | RFDC MTS 完成（前置 `SetClk` 选时钟/PPS 源） | `Sync.yaml` |
| `lo` | 本振已设 | cfg | 各通道 LO 频率/相位 | `SetMixer.yaml` |
| `xgbe` | 100G 已配置 | cfg | 8 个端口的源/目的 MAC、IP、UDP 端口 | `XGbeCfgSingle` ×8 |
| `ready` | 就绪 | cfg | 可开流；上电后必须显式 `SetDSA`（默认大衰减，`GetDSA` 假回读 0） | `SetDSA.yaml` |
| `tx` | 数据传输中 | stream | PL `transmitting=1`，8 路 UDP 上行 | `StreamStart.yaml`（PPS 对齐） |

## 转移

| from | → | to | 触发 | 说明 |
|---|---|---|---|---|
| `off` | → | `boot` | 上电 | 启动链开始 |
| `boot` | → | `mac` | 自动 | `port_mac()` |
| `mac` | → | `dhcp` | 自动 | `dhcp_start()` |
| `dhcp` | → | `online` | 拿到 IP | 地址非 0 → UDP 服务建 socket |
| `online` | → | `synced` | `Sync.yaml` | MTS；内部先 `stop_stream()` |
| `synced` | → | `lo` | `SetMixer.yaml` | `sync=1` 时等 PPS 边沿 |
| `lo` | → | `xgbe` | `XGbeCfgSingle` ×8 | 逐口装订 |
| `xgbe` | → | `ready` | `SetDSA.yaml` | 上电后必须做 |
| `ready` | → | `tx` | `StreamStart.yaml` | PPS 对齐，0~1 s 生效；帧计数清零 |
| `tx` | → | `ready` | `StreamStop.yaml` | PPS 对齐，等 `transmitting=0` |
| 任意在线状态 | → | `boot` | `Reboot.yaml` | 软 POR / 看门狗 |
| `ready` | → | `off` | 断电 | 直接断电 |

## 结构化数据

```yaml
# 与 gen_state_svg.py 中的 NODES / EDGES 一一对应（改图时两边同步）
meta:
  name: syncdaq_board_state
  diagram: syncdaq_state.svg
  source: gen_state_svg.py
  view: 主机指令 → 板卡状态

states:
  - {id: "off",    label: 关机,          kind: power,  desc: "板卡断电，不在线"}
  - {id: boot,   label: 开机 / 启动中,  kind: boot,   desc: "FSBL → PL → PS，LMK/CMAC/RFDC 初始化"}
  - {id: mac,    label: MAC 已配置,     kind: boot,   desc: "控制口 MAC 由 PL DNA 派生"}
  - {id: dhcp,   label: 等待 DHCP,      kind: net,    desc: "无地址时 UDP 服务不建 socket"}
  - {id: online, label: 在线,           kind: net,    desc: "UDP :3000，Query 收 336 B"}
  - {id: synced, label: 已同步,         kind: cfg,    desc: "RFDC MTS"}
  - {id: lo,     label: 本振已设,       kind: cfg,    desc: "LO 频率/相位"}
  - {id: xgbe,   label: 100G 已配置,    kind: cfg,    desc: "8 端口 MAC/IP/UDP 端口"}
  - {id: ready,  label: 就绪,           kind: cfg,    desc: "可开流；SetDSA 已设"}
  - {id: tx,     label: 数据传输中,     kind: stream, desc: "PL transmitting=1"}

transitions:
  - {from: "off",    to: boot,   trigger: 上电,              effect: "启动链开始"}
  - {from: boot,   to: mac,    trigger: 自动,              effect: "port_mac()"}
  - {from: mac,    to: dhcp,   trigger: 自动,              effect: "dhcp_start()"}
  - {from: dhcp,   to: online, trigger: 拿到 IP,           effect: "UDP 服务建 socket"}
  - {from: online, to: synced, trigger: Sync.yaml,         effect: "MTS；内部先 stop_stream()"}
  - {from: synced, to: lo,     trigger: SetMixer.yaml,     effect: "sync=1 时等 PPS 边沿"}
  - {from: lo,     to: xgbe,   trigger: XGbeCfgSingle x8,  effect: "逐口装订"}
  - {from: xgbe,   to: ready,  trigger: SetDSA.yaml,       effect: "上电后必须做"}
  - {from: ready,  to: tx,     trigger: StreamStart.yaml,  effect: "PPS 对齐，0~1 s；帧计数清零"}
  - {from: tx,     to: ready,  trigger: StreamStop.yaml,   effect: "PPS 对齐，等 transmitting=0"}
  - {from: "*",    to: boot,   trigger: Reboot.yaml,       effect: "软 POR / 看门狗；任意在线状态均可发"}
  - {from: ready,  to: "off",    trigger: 断电,              effect: "直接断电"}

constraints:
  - "Start/Stop 是 PPS 对齐的：0~1 s 才生效；Stop 后至少等 2 s 再数包"
  - "Sync / SetClk 内部先 stop_stream()：传输中发会强制停流"
  - "上电后必须显式 SetDSA：默认大衰减，GetDSA 假回读 0"
  - "快速反复 Stop/Start 会把数据路径冻结，恢复需重新 init_rfsoc → Sync"
```

## 四条硬约束

1. **Start/Stop 是 PPS 对齐的**：命令发出后 0~1 s 才真正生效；`StreamStop` 后至少等 2 s 再数包。
2. **`Sync` 和 `SetClk` 内部先 `stop_stream()`**：传输中发这两个会强制停流。
3. **上电后必须显式 `SetDSA`**：片上衰减器停在上电默认态，`GetDSA` 却回读 0（固件已知 bug）。
4. **快速反复 Stop/Start 会把数据路径冻结**（Stop 失效、`pkt_cnt` 一直涨）；恢复 = 重新 `init_rfsoc` → `Sync`。

## 两个"MAC"不要混

- 控制口（1G SFP）：MAC 由 **PL DNA 自动派生**，上电即完成，不需要指令。
- 100G 数据口：MAC/IP/端口由 **`XGbeCfgSingle`** 逐口装订，属于"100G 已配置"一步。

## 待确认

- `init_rfsoc.yaml` 的实际顺序是 `SetClk → Sync → XGbeCfgSingle×8 → SetMixer`；图中按"同步 → 本振 → 100G"排列。
- `SetDSA` 在图中挂在"100G 已配置 → 就绪"边上；它其实在上电后任意时刻可做。

## 事实来源

- `~/fpga/sync_daq_320MSps_iq/sw/daq_sys/src/main.c`：启动链、`port_mac`、DHCP
- `~/fpga/sync_daq_320MSps_iq/sw/daq_sys/src/udp_server.c`：各指令处理器、`stream_state()`
- `~/fpga/sync_daq_320MSps_iq/bsv/AXISGate.bsv`：PL 侧 arm/transmitting 与 PPS 锁存
- `~/src/syncdaq/cmd/*.yaml`、`README.md`、`repeat_trig.sh`、`init_rfsoc.yaml`
- `~/fpga/doc/DAQ_固件与软件测试流程.md`、`单次开机反复触发绝对延迟测试规程.md`
