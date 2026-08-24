use std::{sync::Arc, thread::JoinHandle, time::{Duration, Instant}};

use chrono::Local;
use crossbeam::channel::{Receiver, Sender};
use lockfree_object_pool::{LinearObjectPool, LinearOwnedReusable};
use num::Complex;

use crate::{
    firdecim2::firdec_worker::fir_symmetric_full_rate_streams_inplace,
    utils::{claim_worker_core, pin_to_core},
};

use super::{
    super::payload::{N_BYTE_PER_FRAME, Payload},
    I32s,
    firdec_worker::{FirStreamState, StreamState, resample2_streams},
};


type DTYPE = i16;

/// 将 N 级 1/2 下抽样按相对负载（1/2^k）贪心分组。
///
/// 第 k 级输入采样率 = R/2^k，CPU 负载正比于输入率 → 相对负载 = 1/2^k。
/// 一组内累计相对负载 ≤ budget（默认 1.0 ≈ 一个 P 核的负载）即并入同一 worker
/// 线程，避免级联后级低采样率阶段各自独占一个核。
/// 相对负载和为 Σ1/2^k → 默认 budget=1.0 时第一级独占、其余各级合并为一个 worker。
/// budget 可用环境变量 `SYNCDAQ_DECIM_BUDGET` 覆盖（越小越保守、worker 越多）。
fn group_decim_stages(n_stages: usize, budget: f64) -> Vec<(usize, usize)> {
    let mut groups = Vec::new();
    let mut start = 0usize;
    let mut load = 0.0f64;
    for k in 0..n_stages {
        let rel = 1.0 / (1u64 << k) as f64;
        if load > 0.0 && load + rel > budget {
            groups.push((start, k));
            start = k;
            load = 0.0;
        }
        load += rel;
    }
    if start < n_stages {
        groups.push((start, n_stages));
    }
    groups
}

fn decim_budget() -> f64 {
    std::env::var("SYNCDAQ_DECIM_BUDGET")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(1.0)
}

/// 一个 worker 线程依次执行一组 1/2 下抽样阶段。
///
/// 每次读取 2^n 个输入包（n = 组内阶段数），逐级两两合并（每级包数减半、
/// pkt_cnt 除以 2），最终输出 1 个包。各级使用各自独立的 `StreamState`。
fn start_decim_group(
    recv: Receiver<LinearOwnedReusable<Payload<Complex<DTYPE>>>>,
    send: Sender<LinearOwnedReusable<Payload<Complex<DTYPE>>>>,
    fir_coeffs_i32: Vec<I32s>,
    bit_shifts: Vec<u32>,
) -> JoinHandle<()> {
    let patch_len = N_BYTE_PER_FRAME / std::mem::size_of::<Complex<DTYPE>>();
    let n_stages = bit_shifts.len();
    let batch = 1usize << n_stages;

    std::thread::spawn(move || {
        if let Some(core) = claim_worker_core() {
            pin_to_core(core);
        }
        // 每级各自的流状态（各级是不同的流，历史不能共用）
        let mut states: Vec<StreamState> = (0..n_stages).map(|_| StreamState::new()).collect();

        let pool: Arc<LinearObjectPool<Payload<Complex<DTYPE>>>> = Arc::new(LinearObjectPool::new(
            move || {
                //eprint!("o");
                Payload::<Complex<DTYPE>>::default()
            },
            |_v| {},
        ));

        loop {
            // 1. 读取 batch = 2^n 个输入包
            let mut cur: Vec<LinearOwnedReusable<Payload<Complex<DTYPE>>>> =
                Vec::with_capacity(batch);
            for _ in 0..batch {
                match recv.recv() {
                    Ok(p) => cur.push(p),
                    Err(_) => return,
                }
            }

            // 2. 逐级处理：每级把 cur 两两合并减半
            for (si, &shift) in bit_shifts.iter().enumerate() {
                let half = cur.len() / 2;
                let mut next: Vec<LinearOwnedReusable<Payload<Complex<DTYPE>>>> =
                    Vec::with_capacity(half);
                for i in 0..half {
                    let mut out = pool.pull_owned();
                    let out_raw = unsafe {
                        std::slice::from_raw_parts_mut(
                            out.data.as_mut_ptr() as *mut DTYPE,
                            patch_len * 2,
                        )
                    };
                    let i1 = unsafe {
                        std::slice::from_raw_parts(
                            cur[2 * i].data.as_ptr() as *const DTYPE,
                            patch_len * 2,
                        )
                    };
                    let i2 = unsafe {
                        std::slice::from_raw_parts(
                            cur[2 * i + 1].data.as_ptr() as *const DTYPE,
                            patch_len * 2,
                        )
                    };
                    resample2_streams(
                        i1,
                        &mut out_raw[..patch_len],
                        &fir_coeffs_i32,
                        &mut states[si],
                        shift,
                    );
                    resample2_streams(
                        i2,
                        &mut out_raw[patch_len..],
                        &fir_coeffs_i32,
                        &mut states[si],
                        shift,
                    );
                    out.copy_header(&cur[2 * i]);
                    out.pkt_cnt /= 2;
                    next.push(out);
                }
                cur = next;
            }

            // 3. 逐级处理完后 cur 恰好 1 个包
            let out = cur.pop().expect("分组 worker 输出不应为空");
            if send.send(out).is_err() {
                return;
            }
        }
    })
}

pub fn start_decim_pipeline(
    recv: Receiver<LinearOwnedReusable<Payload<Complex<DTYPE>>>>,
    send: Sender<LinearOwnedReusable<Payload<Complex<DTYPE>>>>,
    fir_coeffs: &[DTYPE],
    bit_shift: u32,
) -> JoinHandle<()> {
    let fir_coeffs_i32: Vec<I32s> = fir_coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();
    start_decim_group(recv, send, fir_coeffs_i32, vec![bit_shift])
}

pub fn start_decim_pipeline_chain(
    recv: Receiver<LinearOwnedReusable<Payload<Complex<DTYPE>>>>,
    fir_coeffs: &[DTYPE],
    bit_shifts: &[u32],
) -> (
    Vec<JoinHandle<()>>,
    Receiver<LinearOwnedReusable<Payload<Complex<DTYPE>>>>,
) {
    let fir_coeffs_i32: Vec<I32s> = fir_coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();
    let budget = decim_budget();
    let groups = group_decim_stages(bit_shifts.len(), budget);
    if groups.len() > 1 {
        eprintln!(
            "decim: {} 级分组为 {} 个 worker (budget={})",
            bit_shifts.len(),
            groups.len(),
            budget
        );
    }

    let mut handles = Vec::with_capacity(groups.len());
    let mut curr_recv = recv;
    for (s, e) in groups {
        let (send_next, recv_next) = crossbeam::channel::unbounded();
        let shifts = bit_shifts[s..e].to_vec();
        handles.push(start_decim_group(curr_recv, send_next, fir_coeffs_i32.clone(), shifts));
        curr_recv = recv_next;
    }
    (handles, curr_recv)
}


pub fn start_fir_pipeline(
    recv: Receiver<LinearOwnedReusable<Payload<Complex<DTYPE>>>>,
    send: Sender<LinearOwnedReusable<Payload<Complex<DTYPE>>>>,
    fir_coeffs: &[DTYPE],
    bit_shift: u32,
) -> JoinHandle<()> {
    let fir_coeffs = fir_coeffs.to_vec();
    let fir_coeffs_i32: Vec<I32s> = fir_coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();
    let patch_len = N_BYTE_PER_FRAME / std::mem::size_of::<Complex<DTYPE>>();

    std::thread::spawn(move || {
        if let Some(core) = claim_worker_core() {
            pin_to_core(core);
        }
        let mut last_print_time = Instant::now();
        let print_interval = Duration::from_secs(2);
        let mut state = FirStreamState::new();

        loop {
            // 全速率 FIR 原地处理：读入 state 后才写回，直接复用输入 payload，
            // 省去输出对象池的 pull/push 与头拷贝。
            let mut input = match recv.recv() {
                Ok(p) => p,
                Err(_) => break,
            };
            let input_raw = unsafe {
                std::slice::from_raw_parts_mut(input.data.as_mut_ptr() as *mut DTYPE, patch_len * 2)
            };

            fir_symmetric_full_rate_streams_inplace(input_raw, &fir_coeffs_i32, &mut state, bit_shift);

            // if send.send(output).is_err() {
            //     break;
            // }
            match send.try_send(input) {
                Ok(()) => {}
                Err(e) => {
                    match e {
                        crossbeam::channel::TrySendError::Full(_) => {
                            //dbg!("O");
                            //eprintln!("q: {}", send.len());
                            //if the channel is full, we just drop the output and continue. This is to avoid blocking the pipeline.
                            //this is ok, because this stage is always used in the last step of the pipeline, so dropping some output will not cause any problem.
                        }
                        crossbeam::channel::TrySendError::Disconnected(_) => {
                            break;
                        }
                    }
                }
            }

            let now = Instant::now();
            if now.duration_since(last_print_time) >= print_interval{
                let local_time = Local::now().format("%Y-%m-%d %H:%M:%S");
                eprintln!("{} fir q: {}", local_time, send.len());
                last_print_time = now;
            }

        }
    })
}

#[cfg(test)]
mod tests {
    use super::group_decim_stages;

    #[test]
    fn grouping_packs_low_rate_stages() {
        // budget 1.0：第一级独立，其余各级合并进第二个 worker
        assert_eq!(group_decim_stages(1, 1.0), vec![(0, 1)]);
        assert_eq!(group_decim_stages(3, 1.0), vec![(0, 1), (1, 3)]);
        assert_eq!(group_decim_stages(6, 1.0), vec![(0, 1), (1, 6)]);
        // 预算收紧 → 分组更多
        assert_eq!(group_decim_stages(4, 0.75), vec![(0, 1), (1, 3), (3, 4)]);
        // 预算放宽 → 两级可以合并
        assert_eq!(group_decim_stages(2, 1.5), vec![(0, 2)]);
        // 空链
        assert_eq!(group_decim_stages(0, 1.0), Vec::<(usize, usize)>::new());
    }
}
