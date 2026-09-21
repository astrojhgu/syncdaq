# 编译
1. 安装`rust`编译环境
2. `git clone https://github.com/astrojhgu/syncdaq`
3. `cd syncdaq`
4. `cargo build --release`

# 配置100G网口参数
首先要确认100G网卡的网络端口名称。
步骤如下：
1. 使用 `ip link`命令，列出所有的网络端口名称
2. 对于可能是100G网络端口的名称，例如`xgbe1`，使用命令
```bash
ethtool xgbe1
```
列出网络端口的信息，注意其中的`Advertised link mode`部分，找到对应100G网速的模式，如果存在，那么该端口为100G以太网端口。

3. 配置该端口的ip地址和mtu。mtu设置为9000，并注意ip地址不要和其他端口冲突（可以存在多种“冲突”类型，在此不做展开）。
4. 几下该端口的mac地址和ip地址，进入下一步。


# 准备指令文件
0. 可以将cmd复制一份到cmd1，然后编辑
1. 编辑`cmd1/XGbeCfgSingle.yaml`(看了就知道怎么改，唯一需要注意的是如果想禁用某路的发送，就将`src/dst_mac`设置为全0)

# 准备一台运行dhcp服务的机器
配置一台运行 dhcp服务的服务器，监听某个端口，假定改端口的ip地址是`192.168.1.1`。

将T510采集板的`sfp28`口（已经编程为千兆以太网口）连接到服务器的对应端口。


## 使用`udhcpd`作为临时的dhcp服务器
如果不想配置全局服务，可以安装`udhcpd`，编写如下配置文件：
```bash
start 192.168.1.100
end 192.168.1.150
interface enp0s20f0u2 #注意这里的网卡名称要和所使用的端口匹配
opt lease 3600
lease_file /tmp/my-leases.leases
```
运行如下命令
```bash
sudo touch /tmp/my-leases.leases
sudo udhcpd -f ./udhcp.conf
```

注意防火墙要放行67和68号UDP端口。


# 发送控制指令
## 一般性控制指令发送命令
```bash
cargo run --bin send_cmd -- -a 192.168.1.255:3000 -c <指令内容文件名> -d 1 -t <超时秒数>
```


## 状态查询
`cargo run --bin send_cmd -- -a 192.168.1.255:3000 -c cmd1/Query.yaml -d 1 -t 3`

## 装订100G网口参数
`cargo run --bin send_cmd -- -a 192.168.1.255:3000 -c cmd1/XGbeCfgSingle.yaml -d 1 -t 3`

去往 [设置内部gps为时钟和pps源](#设置内部gps为时钟和pps源)或者[设置外部10MHz和pps](#设置外部10MHz和pps)

## 设置内部gps为时钟和pps源
`cargo run --bin send_cmd -- -a 192.168.1.255:3000 -c cmd1/gps.yaml -d 1 -t 3;`

## 设置外部10MHz和pps
`cargo run --bin send_cmd -- -a 192.168.1.255:3000 -c cmd1/ext_clk.yaml -d 1 -t 3;`

## 执行`mts同步`
`cargo run --bin send_cmd -- -a 192.168.1.255:3000 -c cmd1/Sync.yaml -d 1 -t 3;`

## 设置本振
编辑`cmd1/MixerSet.yaml`，修改其中的本振频率（以MHz为单位，浮点数）、本振相位（可不改）。`sync`字段代表是否执行同步，若执行同步，则会等待下一个pps秒脉冲，使用 sysref作为事件触发，否则就不执行同步，而是使用tile 作为事件触发。

`cargo run --bin send_cmd -- -a 192.168.1.255:3000 -c cmd1/MixerSet.yaml -d 1 -t 3;`

## 开启数据传输
`cargo run --bin send_cmd -- -a 192.168.1.255:3000 -c cmd1/StreamStart.yaml -d 1 -t 3;`

## 停止数据传输
`cargo run --bin send_cmd -- -a 192.168.1.255:3000 -c cmd1/StreamStop.yaml -d 1 -t 3;`

# 升级板卡固件（把手工 7 步打包成一条命令）
原来手工要做的 7 步是：拷 `BOOT.bin` 到 `~/tftp` → 建 `BOOT.bin` 软链 → 算 md5 →
把 md5 抄进 `SwitchFW*.yaml` → 起 tftp 服务（非特权 1069）→ 发 `FetchFW.yaml`（异步）→
发 `SwitchFW.yaml`（同步，`-t 40` 等 `succeeded`）。现在一条命令搞定：

```bash
./upgrade_fw --addr 192.168.5.174 --fw ~/tftp/BOOT320.bin --storage emmc
```

| 旗标 | 说明 |
|---|---|
| `--addr` | 板卡控制地址，`IP` 或 `IP:PORT`（省略端口默认 3000）；**也支持广播地址**（如 `192.168.5.255`，不必知道板卡 DHCP 到哪个 IP），见下方注意事项 |
| `--fw` | 要升级的固件文件（**就地伺服**，不拷贝、不建软链、不动 `~/tftp`） |
| `--storage` | `emmc`（协议 storage=0）或 `sd`（=1） |
| `--srv-ip` | TFTP 服务端 IP，默认按到板卡的路由自动探测 |
| `--tftp-port` | 默认 1069；被占用时自动改用空闲端口，并把该端口写进 FetchFW |
| `--switch-timeout` | SwitchFW 的同步等待时间，默认 40s |
| `--idle-timeout` | 传输无进展多久判失败，默认 15s |
| `--dry-run` | 只算 md5、打印两份报文，一个包都不发 |
| `--fetch-only` | 只跑 1-6 步：传固件但不切换（板卡上留下 `<storage>:/BOOT.new`） |
| `--switch-only` | 只发 SwitchFW：md5 就地重算，不重传（适合切换失败后补一刀） |

要点：

* **绝不自动重启**（只封装 1-7 步）。成功后可自行重启：
  `cargo run --bin send_cmd -- -a 192.168.5.174:3000 -c cmd/Reboot.yaml -d 1 -t 3`
* 传输结束的判据是**最后一块 DATA 被 ACK**（服务端亲历）——`FetchFW` 的立即应答只是"收到指令"，
  不代表传完；板卡全程不发完成通知。
* 板卡固定请求 `BOOT.bin`，本地文件名任意；实测板卡**不协商任何 TFTP 选项**，
  即 512B/块、35MB 约 68582 块（**块号会回绕**，已在真实场景验证）、耗时约 48s。
* 退出码：`0` 成功 · `1` 参数/文件错 · `2` 板卡无应答 · `3` 传输失败/超时/重复会话 ·
  `4` 切换失败或超时 · `5` TFTP 端口不可用。
* **失败时的现场规矩**：`FetchFW` 一旦发出却没跑完，板卡会处于"抓取进程卡死"态——
  它**照样响应 Query 等指令**（别把这当健康证明），唯一恢复手段是在 fpga 工程里做 JTAG
  重编程：`cd ~/fpga/sync_daq_100MSps_iq/scripts && ./jtag_run.sh`（普通 `Reboot` 无效）。
  工具在失败时会把这行命令打印成醒目横幅，并提示去看板卡控制台 `/dev/ttyUSB1`。
* **传输途中绝不要杀进程**（实测教训）：`timeout`/`kill` 打断一次传输，板卡就会进卡死态。
  工具对 SIGINT/SIGTERM 都装了处理器（会打横幅后以 130 退出），但**先杀了再想恢复就已经晚了**；
  `SIGKILL` 更是无法拦截。给 `timeout` 留足余量（35MB 约需 50s）。
* **广播地址**：`--addr 192.168.5.255` 可用（发 `FetchFW`/`SwitchFW` 前会自动设
  `SO_BROADCAST`）。但广播会让**子网内每块板卡**都执行指令，而本工具只服务**一个**
  TFTP 会话——多块板卡同时来抓时，后来的会被拒（`second TFTP session refused`）并可能因此
  卡死。所以：**确认子网上只有一块板卡**，否则请用具名单播 IP。探活/切换的回复会带上
  "回复来自哪块板卡"（如 `board online: 192.168.5.174:3000`），便于确认是谁在应答。

### 多板卡同批升级（广播模式）

`--addr` 给**广播地址**时进入多板卡模式；给单播 IP 时只碰那一块板卡、绝不牵连其它板卡。

```bash
./upgrade_fw --addr 192.168.5.255 --fw ~/tftp/BOOT320.bin --storage emmc
```

时序：广播 `Query` **枚举所有在线板卡**（3 轮、抗回复碰撞）→ 广播 `FetchFW` → 内置 TFTP
服务端**并发服务**（`--max-clients`，默认 16；每会话独立 TID、共享同一份内存镜像，保证各板卡
拿到的字节与 md5 完全一致）→ 对**每一块枚举到的在线板卡**逐块单播 `SwitchFW`，然后统一收回复
直到全部到齐或 `--switch-timeout` 超时 → 逐块打印"抓取/切换"两维结果表。

* 抓取阶段单行汇总进度（总字节/总目标/总速率/完成数），每块板卡抓完立刻单独打一行结果。
* 失败语义：**继续完成能完成的**。退出码 **3 > 4 > 0**：
  `3` = 有板卡抓取未完成，或"探活应答了却始终不发 RRQ"（后者正是卡死态的典型特征，会打横幅）；
  `4` = 抓取都正常但有板卡 `succeeded=0`（无回复也算）；`0` = 全部成功。
* 相关开关：`--max-clients`（并发上限）、`--quiet-window`（所有会话完成后静默多久收工，默认 3s）、
  `--overall-timeout`（抓取阶段全程上限，默认 600s，0 = 不限）。
* `tftp_serve --multi --max-clients N` 是同一套并发服务端的独立可执行版。

回环自测（不需要板卡，覆盖块号回绕/选项协商/停滞判定/重复 RRQ/异 IP 拒绝/**3 客户端并发**）：

```bash
cargo test --release --lib tftp_server      # 单元测试
utils/test_tftp_server.sh [固件路径]          # 用 curl/自写客户端打本地服务端
```

另外 `cargo run --release --bin tftp_serve -- --path <文件> --port 1069` 是同一个内置
服务端的独立可执行版，可直接顶替原来的 `~/tftp/tftp.py`。

# 抓取基带数据
```bash
$> cargo run --bin capture_pipeline --release -- -h
Usage: capture_pipeline [OPTIONS] --addr <ip:port>

Options:
  -a, --addr <ip:port>                    
  -o, --out <out name>                    
  -F <out prefix for full dump file>      
  -k <number of pkts per full dump file>  [default: 1000000]
  -n <npkts_per_dump>                     [default: 100]
  -m <dumps per npkt>                     [default: 100000]
  -p <npkts to dump>                      
  -h, --help                              Print help
  -V, --version                           Print version
```

# 故障排除
## 控制端口无法从dhcp处获得ip地址
可能原因：
1. dhcp服务器没有启动
2. dhcp虽然启动了，但是在配置文件中没有绑定在正确的端口上
3. dhcp虽然启动了，但是防火墙没有放行相应的端口
4. 板卡要使用SFP口，不要使用板卡上的RJ-45口

## 指令能发出，从console看，板卡确实收到了指令，但是用户端没有看到指令的回复
可能是用来发送指令的千兆端口的ip地址和本机上其他端口的处于同一个网段，这样就触发了linux的反向过滤策略。解决方案：

```bash
sudo nano /etc/sysctl.d/99-rpfilter.conf
```
加入如下两行：
```bash
net.ipv4.conf.all.rp_filter = 0
net.ipv4.conf.default.rp_filter = 0
```
然后用如下命令使能该配置
```bash
sudo sysctl -p /etc/sysctl.d/99-rpfilter.conf
```

## 能响应指令，但是没有无法捕捉到数据
1. 100G端口参数没有正确配置，特别是mtu，要设置为9000
2. 100G端口的参数没有正确装订，即装订的100G网口参数和用来收数据的100G端口不一致
3. 防火墙没有正确配置为放行特定的端口
4. 可能还是反向过滤的原因，参见上面关于千兆网口反向过滤的解决方案

