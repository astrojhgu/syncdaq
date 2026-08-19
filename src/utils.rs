use std::{
    net::UdpSocket,
    os::fd::AsRawFd,
    slice::{from_raw_parts, from_raw_parts_mut},
};

use futures_core::Stream;
use futures_util::StreamExt;
use num::Complex;
use serde::de::{self, SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserializer, Serializer};
use std::fmt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use libc::{SO_RCVBUF, SOL_SOCKET, setsockopt, socklen_t};

pub fn as_complex_t<'a, 'b, T: Sized>(input: &'a [u8]) -> &'b [Complex<T>]
where
    'b: 'a,
{
    let npt = input.len() / std::mem::size_of::<T>() / 2;
    unsafe { from_raw_parts(input.as_ptr() as *const Complex<T>, npt) }
}

pub fn as_u8_slice<'a, 'b, T: Sized>(x: &'a T) -> &'b [u8]
where
    'b: 'a,
{
    unsafe { from_raw_parts((x as *const T) as *const u8, std::mem::size_of::<T>()) }
}

pub fn as_mut_u8_slice<'a, 'b, T: Sized>(x: &'a mut T) -> &'b mut [u8]
where
    'b: 'a,
{
    unsafe { from_raw_parts_mut((x as *mut T) as *mut u8, std::mem::size_of::<T>()) }
}

pub fn slice_as_u8<T: Sized>(x: &[T]) -> &[u8] {
    unsafe { from_raw_parts(x.as_ptr() as *const u8, std::mem::size_of_val(x)) }
}

pub fn set_recv_buffer_size(socket: &UdpSocket, size: usize) -> std::io::Result<()> {
    let fd = socket.as_raw_fd();
    let size = size as libc::c_int;

    let ret = unsafe {
        setsockopt(
            fd,
            SOL_SOCKET,
            SO_RCVBUF,
            &size as *const _ as *const libc::c_void,
            std::mem::size_of_val(&size) as socklen_t,
        )
    };

    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub mod u8_hex_array {
    use super::*;

    pub fn serialize<S>(data: &[u8; 6], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(6))?;
        for byte in data.iter() {
            seq.serialize_element(byte)?; // 序列化为整数，不是字符串
        }
        seq.end()
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 6], D::Error>
    where
        D: Deserializer<'de>,
    {
        struct U8HexVisitor;

        impl<'de> Visitor<'de> for U8HexVisitor {
            type Value = [u8; 6];

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a list of 6 u8 values, decimal or 0x-prefixed hex")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<[u8; 6], A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut result = [0u8; 6];
                for i in 0..6 {
                    let value: serde_yaml::Value = seq
                        .next_element()?
                        .ok_or_else(|| de::Error::invalid_length(i, &self))?;

                    let parsed = match &value {
                        // YAML会将裸数字解析成Number，可以是十进制或十六进制
                        serde_yaml::Value::Number(n) => n
                            .as_u64()
                            .ok_or_else(|| de::Error::custom("invalid number"))?,
                        // 或者写成 "0x??" 的字符串，也接受
                        serde_yaml::Value::String(s) => {
                            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))
                            {
                                u8::from_str_radix(hex, 16)
                                    .map_err(|_| de::Error::custom("invalid hex string"))?
                                    as u64
                            } else {
                                s.parse::<u64>()
                                    .map_err(|_| de::Error::custom("invalid decimal string"))?
                            }
                        }
                        _ => return Err(de::Error::custom("expected number or string")),
                    };

                    if value > (u8::MAX as u64).into() {
                        return Err(de::Error::custom("value out of range for u8"));
                    }

                    result[i] = parsed as u8;
                }
                Ok(result)
            }
        }

        deserializer.deserialize_seq(U8HexVisitor)
    }
}

pub fn async_buffer<S>(mut input_stream: S, buffer_size: usize) -> impl Stream<Item = S::Item>
where
    S: Stream + Send + Unpin + 'static,
    S::Item: Send + Unpin + 'static,
{
    // 1. 创建异步通道作为缓冲区
    let (tx, rx) = mpsc::channel(buffer_size);

    // 2. 启动独立任务进行“推”操作
    tokio::spawn(async move {
        while let Some(item) = input_stream.next().await {
            // 如果 tx.send 失败，说明下游接收端（ReceiverStream）已关闭
            if tx.send(item).await.is_err() {
                break;
            }
        }
    });

    // 3. 将 Receiver 包装回 Stream 返回
    ReceiverStream::new(rx)
}

use core_affinity;

pub fn pin_current_thread() {
    let cpu = unsafe { libc::sched_getcpu() };

    let cores = core_affinity::get_core_ids().unwrap();
    let core = cores.into_iter().find(|c| c.id == cpu as usize).unwrap();

    core_affinity::set_for_current(core);
}

pub fn pin_to_core(core_id: usize) {
    if let Some(core) = core_affinity::get_core_ids()
        .into_iter()
        .flatten()
        .find(|c| c.id == core_id)
    {
        core_affinity::set_for_current(core);
    }
}

/// 探测 CPU 的基础频率（Linux cpufreq），作为 P/E 核判断的回退信号。
fn cpu_base_freq(cpu: usize) -> Option<u32> {
    std::fs::read_to_string(format!(
        "/sys/devices/system/cpu/cpu{cpu}/cpufreq/base_frequency"
    ))
    .ok()?
    .trim()
    .parse()
    .ok()
}

/// 探测 CPU 的容量权重（Linux sched asymmetric capacity）。
/// 混合架构下 P 核容量约为 E 核的两倍（如 854 vs 427）；未启用时恒为 1024 或缺失。
fn cpu_capacity(cpu: usize) -> Option<u32> {
    std::fs::read_to_string(format!(
        "/sys/devices/system/cpu/cpu{cpu}/cpu_capacity"
    ))
    .ok()?
    .trim()
    .parse()
    .ok()
}

/// 返回某逻辑 CPU 所属物理核的代表 id（thread_siblings_list 中的最小值）。
fn physical_core_repr(cpu: usize) -> usize {
    if let Ok(s) = std::fs::read_to_string(format!(
        "/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list"
    )) {
        if let Some(first) = s.split(['-', ',']).next() {
            if let Ok(id) = first.trim().parse::<usize>() {
                return id;
            }
        }
    }
    cpu
}

/// 优选 worker 核列表：每个物理核一个代表 CPU，P 核在前。
///
/// 排序信号自动选择（取第一个能真正区分核的信号）：
/// 1. `cpu_capacity`（内核混合架构容量，P≈2×E）；
/// 2. `base_frequency`（cpufreq 基础频率，P 通常显著更高）；
/// 3. 两者都不区分（非混合架构）→ 按物理核 id 升序。
pub fn preferred_worker_cores() -> Vec<usize> {
    let n = core_affinity::get_core_ids()
        .map(|v| v.len())
        .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1));

    // 每个物理核: (代表CPU, 该核最大capacity, 该核最大base_freq)
    let mut phys: std::collections::BTreeMap<usize, (usize, u32, u32)> =
        std::collections::BTreeMap::new();
    for cpu in 0..n {
        let phys_id = physical_core_repr(cpu);
        let cap = cpu_capacity(cpu).unwrap_or(0);
        let freq = cpu_base_freq(cpu).unwrap_or(0);
        let e = phys.entry(phys_id).or_insert((cpu, cap, freq));
        // 代表 CPU 取该物理核内频率/容量最高者；相同时保留 id 较小者
        if freq > e.2 || (freq == e.2 && cap > e.1) {
            *e = (cpu, cap, freq);
        }
    }

    let distinct = |vals: Vec<u32>| -> bool {
        let mut s = vals;
        s.retain(|&v| v > 0);
        s.sort_unstable();
        s.dedup();
        s.len() > 1
    };
    let use_cap = distinct(phys.values().map(|v| v.1).collect());
    let use_freq = distinct(phys.values().map(|v| v.2).collect());

    let mut cores: Vec<(usize, u32, usize)> = phys
        .into_iter()
        .map(|(pid, (cpu, cap, freq))| {
            let key = if use_cap {
                cap
            } else if use_freq {
                freq
            } else {
                0
            };
            (cpu, key, pid)
        })
        .collect();
    // P 核（key 高）优先，同 key 按物理核 id 升序
    cores.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    cores.into_iter().map(|(c, _, _)| c).collect()
}

use std::sync::Mutex;

/// 本进程 worker 线程已认领的核（stage 线程启动时认领一次，进程存活期间不释放）。
static CLAIMED_CORES: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// 为 worker 线程认领一个未被占用的核。
///
/// 规则：
/// - 设置了 `SYNCDAQ_NO_PIN` 时返回 `None`（不绑定）。
/// - 设置了 `SYNCDAQ_CORES`（如 `"0,2,4,6"`）时按该列表轮转，优先未占用的核。
/// - 否则自动认领：每个物理 P 核一个代表、P 核优先，避免落到 E 核 / HT 兄弟核争抢。
pub fn claim_worker_core() -> Option<usize> {
    if std::env::var("SYNCDAQ_NO_PIN").is_ok() {
        return None;
    }
    let mut claimed = CLAIMED_CORES.lock().unwrap();
    if let Some(list) = std::env::var("SYNCDAQ_CORES")
        .ok()
        .map(|s| s.split([',', ' ']).filter_map(|x| x.trim().parse::<usize>().ok()).collect::<Vec<_>>())
        .filter(|v: &Vec<usize>| !v.is_empty())
    {
        for &c in &list {
            if !claimed.contains(&c) {
                claimed.push(c);
                return Some(c);
            }
        }
        // 全部占用：轮转分配
        let n = claimed.iter().filter(|c| list.contains(c)).count();
        return Some(list[n % list.len()]);
    }
    for c in preferred_worker_cores() {
        if !claimed.contains(&c) {
            claimed.push(c);
            return Some(c);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn valid_cores() -> HashSet<usize> {
        core_affinity::get_core_ids()
            .map(|ids| ids.into_iter().map(|c| c.id).collect())
            .unwrap_or_default()
    }

    #[test]
    fn preferred_cores_each_physical_once_and_valid() {
        let cores = preferred_worker_cores();
        assert!(!cores.is_empty(), "应返回至少一个核");
        let valid = valid_cores();
        let mut phys_seen = HashSet::new();
        for &c in &cores {
            assert!(valid.contains(&c), "返回了无效核 {}", c);
            assert!(
                phys_seen.insert(physical_core_repr(c)),
                "重复物理核代表 {} (cpu {})",
                physical_core_repr(c),
                c
            );
        }
    }

    #[test]
    fn preferred_cores_p_first_when_signal_differentiates() {
        let cores = preferred_worker_cores();
        // 若机器上任一 P/E 信号可区分，则返回顺序必须非增
        let caps: Vec<u32> = cores.iter().map(|&c| cpu_capacity(c).unwrap_or(0)).collect();
        let freqs: Vec<u32> = cores.iter().map(|&c| cpu_base_freq(c).unwrap_or(0)).collect();
        let distinct_cap = {
            let mut s = caps.iter().copied().filter(|&v| v > 0).collect::<Vec<_>>();
            s.sort_unstable();
            s.dedup();
            s.len() > 1
        };
        let distinct_freq = {
            let mut s = freqs.iter().copied().filter(|&v| v > 0).collect::<Vec<_>>();
            s.sort_unstable();
            s.dedup();
            s.len() > 1
        };
        // 输出检测到的顺序（--nocapture 可见），便于本机人工核对
        eprintln!("preferred_worker_cores = {:?}", cores);
        eprintln!("  caps  = {:?}", caps);
        eprintln!("  freqs = {:?}", freqs);
        if distinct_cap || distinct_freq {
            let key = |i: usize| -> u32 {
                if distinct_cap {
                    caps[i]
                } else if distinct_freq {
                    freqs[i]
                } else {
                    0
                }
            };
            for w in cores.windows(2) {
                let a = key(cores.iter().position(|&c| c == w[0]).unwrap());
                let b = key(cores.iter().position(|&c| c == w[1]).unwrap());
                assert!(a >= b, "排序未按强度降序: {:?} {:?}", w[0], w[1]);
            }
        }
    }

    #[test]
    fn claim_worker_cores_are_unique() {
        // 环境变量会改变 claim 行为（SYNCDAQ_NO_PIN/CORES），此时跳过
        if std::env::var("SYNCDAQ_NO_PIN").is_ok() || std::env::var("SYNCDAQ_CORES").is_ok() {
            eprintln!("SYNCDAQ_NO_PIN/SYNCDAQ_CORES 已设置，跳过该测试");
            return;
        }
        let mut seen = HashSet::new();
        let pref = preferred_worker_cores();
        let mut claimed_any = false;
        for _ in 0..pref.len() {
            if let Some(c) = claim_worker_core() {
                claimed_any = true;
                assert!(seen.insert(c), "重复认领核心 {}", c);
            }
        }
        assert!(claimed_any, "应能至少认领一个核");
    }

    #[test]
    fn pin_worker_lands_on_claimed_core() {
        if std::env::var("SYNCDAQ_NO_PIN").is_ok() {
            eprintln!("SYNCDAQ_NO_PIN 已设置，跳过");
            return;
        }
        let Some(core) = claim_worker_core() else {
            eprintln!("无可用核，跳过");
            return;
        };
        pin_to_core(core);
        let actual = unsafe { libc::sched_getcpu() } as usize;
        assert_eq!(
            actual, core,
            "绑定到核 {} 后应运行在核 {} 上",
            core, actual
        );
    }
}
