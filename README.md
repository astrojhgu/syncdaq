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

