# 编译
1. 安装`rust`编译环境
2. `git clone https://github.com/astrojhgu/syncdaq`
3. `cd syncdaq`
4. `cargo build --release`

# 准备指令文件
0. 可以将cmd复制一份到cmd1，然后编辑
1. 编辑`cmd1/XGbeCfgSingle.yaml`(看了就知道怎么改，唯一需要注意的是如果想禁用某路的发送，就将`src/dst_mac`设置为全0)

# 准备一台运行dhcp服务的机器
配置一台运行 dhcp服务的服务器，监听某个端口，假定改端口的ip地址是`192.168.1.1`。
将T510采集板的`sfp28`口（已经编程为千兆以太网口）连接到服务器的对应端口。

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
