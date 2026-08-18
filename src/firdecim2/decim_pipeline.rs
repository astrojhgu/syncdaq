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

pub fn start_decim_pipeline(
    recv: Receiver<LinearOwnedReusable<Payload<Complex<DTYPE>>>>,
    send: Sender<LinearOwnedReusable<Payload<Complex<DTYPE>>>>,
    fir_coeffs: &[DTYPE],
    bit_shift: u32,
) -> JoinHandle<()> {
    // Implementation of the decimation pipeline start logic
    let fir_coeffs = fir_coeffs.to_vec();
    let fir_coeffs_i32: Vec<I32s> = fir_coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();
    let patch_len = N_BYTE_PER_FRAME / std::mem::size_of::<Complex<DTYPE>>();

    std::thread::spawn(move || {
        if let Some(core) = claim_worker_core() {
            pin_to_core(core);
        }
        let mut state = StreamState::new();

        let pool: Arc<LinearObjectPool<Payload<Complex<DTYPE>>>> = Arc::new(LinearObjectPool::new(
            move || {
                //eprint!("o");
                Payload::<Complex<DTYPE>>::default()
            },
            |_v| {},
        ));

        // 批量 4 包输入 → 2 包输出，摊薄通道/线程唤醒开销
        loop {
            let mut out1 = pool.pull_owned();
            let mut out2 = pool.pull_owned();
            let out1_raw = unsafe {
                std::slice::from_raw_parts_mut(
                    out1.data.as_mut_ptr() as *mut DTYPE,
                    patch_len * 2,
                )
            };
            let out2_raw = unsafe {
                std::slice::from_raw_parts_mut(
                    out2.data.as_mut_ptr() as *mut DTYPE,
                    patch_len * 2,
                )
            };

            let input1 = match recv.recv() {
                Ok(p) => p,
                Err(_) => break,
            };
            let input2 = match recv.recv() {
                Ok(p) => p,
                Err(_) => break,
            };
            let input3 = match recv.recv() {
                Ok(p) => p,
                Err(_) => break,
            };
            let input4 = match recv.recv() {
                Ok(p) => p,
                Err(_) => break,
            };

            out1.copy_header(&input1);
            out1.pkt_cnt /= 2;
            let i1 = unsafe {
                std::slice::from_raw_parts(input1.data.as_ptr() as *const DTYPE, patch_len * 2)
            };
            let i2 = unsafe {
                std::slice::from_raw_parts(input2.data.as_ptr() as *const DTYPE, patch_len * 2)
            };
            resample2_streams(i1, &mut out1_raw[..patch_len], &fir_coeffs_i32, &mut state, bit_shift);
            resample2_streams(i2, &mut out1_raw[patch_len..], &fir_coeffs_i32, &mut state, bit_shift);

            out2.copy_header(&input3);
            out2.pkt_cnt /= 2;
            let i3 = unsafe {
                std::slice::from_raw_parts(input3.data.as_ptr() as *const DTYPE, patch_len * 2)
            };
            let i4 = unsafe {
                std::slice::from_raw_parts(input4.data.as_ptr() as *const DTYPE, patch_len * 2)
            };
            resample2_streams(i3, &mut out2_raw[..patch_len], &fir_coeffs_i32, &mut state, bit_shift);
            resample2_streams(i4, &mut out2_raw[patch_len..], &fir_coeffs_i32, &mut state, bit_shift);

            if send.send(out1).is_err() || send.send(out2).is_err() {
                break;
            }
        }
    })
}

pub fn start_decim_pipeline_chain(
    recv: Receiver<LinearOwnedReusable<Payload<Complex<DTYPE>>>>,
    fir_coeffs: &[DTYPE],
    bit_shifts: &[u32],
) -> (
    Vec<JoinHandle<()>>,
    Receiver<LinearOwnedReusable<Payload<Complex<DTYPE>>>>,
) {
    bit_shifts.iter().fold((Vec::with_capacity(bit_shifts.len()), recv), 
        |(mut handles, curr_recv), &shift| {
            let (send_next, recv_next) = crossbeam::channel::unbounded();
            
            // 启动当前阶段，传入 curr_recv，产生新的 handle
            handles.push(start_decim_pipeline(curr_recv, send_next, fir_coeffs, shift));
            
            // 返回更新后的元组，供下一轮使用
            (handles, recv_next)
        }
    )
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
