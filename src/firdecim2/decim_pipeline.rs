use std::{sync::Arc, thread::JoinHandle};

use crossbeam::channel::{Receiver, Sender};
use lockfree_object_pool::{LinearObjectPool, LinearOwnedReusable};
use num::{Complex, Zero};

use super::{
    I32s,
    firdec_worker::resample2,
    super::payload::{Payload, N_BYTE_PER_FRAME},
};

//use core_affinity;

// fn pin_current_thread() {
//     let cpu = unsafe { libc::sched_getcpu() };

//     let cores = core_affinity::get_core_ids().unwrap();
//     let core = cores.into_iter().find(|c| c.id == cpu as usize).unwrap();

//     core_affinity::set_for_current(core);
// }

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
    let patch_len= N_BYTE_PER_FRAME/ std::mem::size_of::<Complex<DTYPE>>();

    std::thread::spawn(move || {
        //pin_current_thread();
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
                std::slice::from_raw_parts_mut(output.data.as_mut_ptr() as *mut DTYPE, patch_len * 2)
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
    let n_cascades = bit_shifts.len();
    let mut result = Vec::with_capacity(n_cascades);

    if n_cascades == 0 {
        (result, recv)
    } else {
        let (send1, mut recv1) = crossbeam::channel::bounded::<
            lockfree_object_pool::LinearOwnedReusable<Payload<Complex<DTYPE>>>,
        >(32);
        result.push(start_decim_pipeline(
            recv,
            send1,
            fir_coeffs,
            bit_shifts[0],
        ));

        for i in 1..n_cascades {
            let (send1, recv2) = crossbeam::channel::bounded::<
                lockfree_object_pool::LinearOwnedReusable<Payload<Complex<DTYPE>>>,
            >(4);
            let recv = std::mem::replace(&mut recv1, recv2);
            result.push(start_decim_pipeline(
                recv,
                send1,
                fir_coeffs,
                bit_shifts[i],
            ));
        }
        (result, recv1)
    }
}

