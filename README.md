# 编译
1. 安装`rust`编译环境
2. `git clone https://github.com/astrojhgu/syncdaq`
3. `cd syncdaq`
4. `cargo build --release`

# 准备指令文件
0. 可以将cmd复制一份到cmd1，然后编辑
1. 编辑`cmd1/XGbeCfgSingle.yaml`(看了就知道怎么改，位于需要注意的是如果想禁用某路的发送，就将`src/dst_mac`设置为全0)

# 准备一台运行dhcp服务的机器，用来做控制上位机
（略）

# 使用
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

# 设置本振
编辑`cmd1/MixerSet.yaml`，修改其中的本振频率（以MHz为单位，浮点数）、本振相位（可不改）。`sync`字段代表是否执行同步，若执行同步，则会等待下一个pps秒脉冲，使用 sysref作为事件触发，否则就不执行同步，而是使用tile 作为事件触发。

`cargo run --bin send_cmd -- -a 192.168.1.255:3000 -c cmd1/MixerSet.yaml -d 1 -t 3;`

# 开启数据传输
`cargo run --bin send_cmd -- -a 192.168.1.255:3000 -c cmd1/StreamStart.yaml -d 1 -t 3;`

# 停止数据传输
`cargo run --bin send_cmd -- -a 192.168.1.255:3000 -c cmd1/StreamStop.yaml -d 1 -t 3;`
