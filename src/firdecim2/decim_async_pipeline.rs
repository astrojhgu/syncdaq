use async_stream::stream;
use chrono::Local;
use futures_core::Stream;
use futures_util::StreamExt;
use lockfree_object_pool::{LinearObjectPool, LinearOwnedReusable};
use tokio::task::spawn_blocking;

use std::{
    net::{Ipv4Addr, SocketAddrV4},
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use num::{Complex, Zero};

use super::{
    super::payload::{N_BYTE_PER_FRAME, Payload},
    I32s,
    firdec_worker::resample2,
};

pub fn decim2(
    input: impl Stream<Item = LinearOwnedReusable<Payload<Complex<i16>>>>,
    fir_coeffs: &[i16],
    bit_shift: u32,
) -> impl Stream<Item = LinearOwnedReusable<Payload<Complex<i16>>>>
{
    let fir_coeffs = fir_coeffs.to_vec();
    let _fir_coeffs_i32: Vec<std::simd::Simd<i32, 16>> =
        fir_coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();
    let patch_len = N_BYTE_PER_FRAME / std::mem::size_of::<Complex<i16>>();

    stream! {
        let ntaps = fir_coeffs.len();
        let state_len = ntaps * 2 - 2 + patch_len; // 2:1 decimation, so input is 2x output
        let state = Arc::new(Mutex::new(vec![i16::zero(); state_len*2]));
        // let state_raw = unsafe {
        //     std::slice::from_raw_parts_mut(state.as_mut_ptr() as *mut i16, state_len * 2)
        // };
        let pool: Arc<LinearObjectPool<Payload<Complex<i16>>>> = Arc::new(LinearObjectPool::new(
            move || {
                //eprint!("o");
                Payload::<Complex<i16>>::default()
            },
            |_v| {},
        ));

        futures_util::pin_mut!(input);

        loop{
            let mut output = pool.pull_owned();
            

            let (output1, output2) = output.data.split_at_mut(patch_len);
            let mut output_raw1 = unsafe {
                std::slice::from_raw_parts_mut(output1.as_mut_ptr() as *mut i16, patch_len)
            };

            let mut output_raw2 = unsafe {
                std::slice::from_raw_parts_mut(output2.as_mut_ptr() as *mut i16, patch_len)
            };

            if let Some(input) = input.next().await {
                let input = input;

                output.copy_header(&input);
                output.pkt_cnt /= 2; // because of 2:1 decimation

                let input_raw = unsafe {
                    std::slice::from_raw_parts(input.data.as_ptr() as *const i16, patch_len * 2)
                };
                let d=state.clone();
                let fir1=fir_coeffs.clone();
                spawn_blocking(move ||{
                    resample2(
                    input_raw,
                    &mut output_raw1,
                    &fir1,
                    d.lock().unwrap().deref_mut(),
                    bit_shift,
                );
                }).await.expect("task failed");

            } else {
                break;
            }

            if let Some(input) = input.next().await {
                let input = input;

                let input_raw = unsafe {
                    std::slice::from_raw_parts(input.data.as_ptr() as *const i16, patch_len * 2)
                };
                let d=state.clone();
                let fir1=fir_coeffs.clone();
                spawn_blocking(move ||{
                    resample2(
                    input_raw,
                    &mut output_raw2,
                    &fir1,
                    d.lock().unwrap().deref_mut(),
                    bit_shift,
                );
                }).await.expect("task failed");

            } else {
                break;
            }

            yield output;
        }
    }
}
