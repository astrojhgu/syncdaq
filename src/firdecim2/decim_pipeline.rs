use std::{sync::Arc, thread::JoinHandle, time::{Duration, Instant}};

use chrono::Local;
use crossbeam::channel::{Receiver, Sender};
use lockfree_object_pool::{LinearObjectPool, LinearOwnedReusable};
use num::{Complex, Zero};

use crate::{firdecim2::firdec_worker::fir_symmetric_full_rate, utils::pin_current_thread};

use super::{
    super::payload::{N_BYTE_PER_FRAME, Payload},
    I32s,
    firdec_worker::resample2,
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
    let _fir_coeffs_i32: Vec<std::simd::Simd<i32, 16>> =
        fir_coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();
    let patch_len = N_BYTE_PER_FRAME / std::mem::size_of::<Complex<DTYPE>>();

    std::thread::spawn(move || {
        pin_current_thread();
        let ntaps = fir_coeffs.len();
        let state_len = ntaps * 2 - 2 + patch_len; // 2:1 decimation, so input is 2x output
        let mut state = vec![Complex::<DTYPE>::zero(); state_len];
        let state_raw = unsafe {
            std::slice::from_raw_parts_mut(state.as_mut_ptr() as *mut DTYPE, state_len * 2)
        };

        let pool: Arc<LinearObjectPool<Payload<Complex<DTYPE>>>> = Arc::new(LinearObjectPool::new(
            move || {
                //eprint!("o");
                Payload::<Complex<DTYPE>>::default()
            },
            |_v| {},
        ));

        loop {
            let mut output = pool.pull_owned();
            let output_raw = unsafe {
                std::slice::from_raw_parts_mut(
                    output.data.as_mut_ptr() as *mut DTYPE,
                    patch_len * 2,
                )
            };
            if let Ok(input) = recv.recv() {
                let input_raw = unsafe {
                    std::slice::from_raw_parts(input.data.as_ptr() as *const DTYPE, patch_len * 2)
                };

                output.copy_header(&input);
                output.pkt_cnt /= 2; // because of 2:1 decimation
                resample2(
                    input_raw,
                    &mut output_raw[..patch_len],
                    &fir_coeffs,
                    state_raw,
                    bit_shift,
                );
            } else {
                break;
            }

            if let Ok(input) = recv.recv() {
                let input_raw = unsafe {
                    std::slice::from_raw_parts(input.data.as_ptr() as *const DTYPE, patch_len * 2)
                };

                resample2(
                    input_raw,
                    &mut output_raw[patch_len..],
                    &fir_coeffs,
                    state_raw,
                    bit_shift,
                );
            } else {
                break;
            }

            if send.send(output).is_err() {
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
    // Implementation of the decimation pipeline start logic
    let fir_coeffs = fir_coeffs.to_vec();
    let _fir_coeffs_i32: Vec<std::simd::Simd<i32, 16>> =
        fir_coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();
    let patch_len = N_BYTE_PER_FRAME / std::mem::size_of::<Complex<DTYPE>>();

    std::thread::spawn(move || {
        pin_current_thread();
        let mut last_print_time = Instant::now();
        let print_interval = Duration::from_secs(2);
        let ntaps = fir_coeffs.len();
        let state_len = ntaps * 2 - 1 + patch_len; // 2:1 decimation, so input is 2x output
        let mut state = vec![Complex::<DTYPE>::zero(); state_len];
        let state_raw = unsafe {
            std::slice::from_raw_parts_mut(state.as_mut_ptr() as *mut DTYPE, state_len * 2)
        };

        let pool: Arc<LinearObjectPool<Payload<Complex<DTYPE>>>> = Arc::new(LinearObjectPool::new(
            move || {
                //eprint!("o");
                Payload::<Complex<DTYPE>>::default()
            },
            |_v| {},
        ));

        loop {
            let mut output = pool.pull_owned();
            let mut output_raw = unsafe {
                std::slice::from_raw_parts_mut(
                    output.data.as_mut_ptr() as *mut DTYPE,
                    patch_len * 2,
                )
            };
            if let Ok(input) = recv.recv() {
                let input_raw = unsafe {
                    std::slice::from_raw_parts(input.data.as_ptr() as *const DTYPE, patch_len * 2)
                };

                output.copy_header(&input);
                fir_symmetric_full_rate(
                    input_raw,
                    &mut output_raw,
                    &fir_coeffs,
                    state_raw,
                    bit_shift,
                );
            } else {
                break;
            }

            // if send.send(output).is_err() {
            //     break;
            // }
            match send.try_send(output) {
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
