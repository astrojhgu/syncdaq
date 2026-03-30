use async_stream::stream;
use futures_core::Stream;
use futures_util::StreamExt;
use lockfree_object_pool::{LinearObjectPool, LinearOwnedReusable};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use std::{
    ops::DerefMut,
    pin::Pin,
    sync::{Arc, Mutex},
};

use num::{Complex, Zero};

use crate::firdecim2::firdec_worker::fir_symmetric_full_rate;

use super::{
    super::payload::{N_BYTE_PER_FRAME, Payload},
    I32s,
    firdec_worker::resample2,
};

pub fn with_buffer<S>(upstream: S, cap: usize) -> impl Stream<Item = S::Item>
where
    S: Stream + Send + 'static, // 去掉了 Unpin 约束
    S::Item: Send + 'static,
{
    let (tx, rx) = mpsc::channel(cap);

    // 在这里进行 Pin
    let mut pinned_upstream = Box::pin(upstream);

    tokio::spawn(async move {
        // 现在可以使用 pinned_upstream，因为 Pin<Box<S>> 实现了 Unpin
        while let Some(item) = pinned_upstream.next().await {
            //println!("pkt cnt: {}", item.pkt_cnt);
            if tx.send(item).await.is_err() {
                break;
            }
        }
    });

    ReceiverStream::new(rx)
}

pub fn decim2(
    input: impl Stream<Item = LinearOwnedReusable<Payload<Complex<i16>>>> + Send + 'static,
    fir_coeffs: &[i16],
    bit_shift: u32,
) -> impl Stream<Item = LinearOwnedReusable<Payload<Complex<i16>>>> + Send + 'static {
    let fir_coeffs = fir_coeffs.to_vec();
    let _fir_coeffs_i32: Vec<std::simd::Simd<i32, 16>> =
        fir_coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();
    let patch_len = N_BYTE_PER_FRAME / std::mem::size_of::<Complex<i16>>();

    //let (tx, rx) = mpsc::channel(buffer_size);
    //tokio::spawn(async move {

    let pool: Arc<LinearObjectPool<Payload<Complex<i16>>>> = Arc::new(LinearObjectPool::new(
        move || {
            //eprint!("o");
            Payload::<Complex<i16>>::default()
        },
        |v| {
            v.pkt_cnt = 0;
        },
    ));

    let ntaps = fir_coeffs.len();
    let state_len = ntaps * 2 - 2 + patch_len; // 2:1 decimation, so input is 2x output
    let state = Arc::new(Mutex::new(vec![i16::zero(); state_len * 2]));
    // let state_raw = unsafe {
    //     std::slice::from_raw_parts_mut(state.as_mut_ptr() as *mut i16, state_len * 2)
    // };

    stream! {
        futures_util::pin_mut!(input);

        let mut batched_input = input.chunks(2);
        loop{
            let mut output = pool.pull_owned();
            assert!(output.data.len() == patch_len);
            let (output1, output2) = output.data.split_at_mut(patch_len/2);

            assert!(output1.len() == patch_len/2);
            assert!(output2.len() == patch_len/2);
            let mut output_raw1 = unsafe {
                std::slice::from_raw_parts_mut(output1.as_mut_ptr() as *mut i16, patch_len)
            };

            let mut output_raw2 = unsafe {
                std::slice::from_raw_parts_mut(output2.as_mut_ptr() as *mut i16, patch_len)
            };

            if let Some(batch)=batched_input.next().await{
                if batch.len()!=2{
                    break;
                }
                let input1=&batch[0];
                let input2=&batch[1];

                output.copy_header(input1);
                output.pkt_cnt /=2; // because of 2:1 decimation
                //println!("pkt CNT:{} {}", input1.pkt_cnt, input2.pkt_cnt);
                let input_raw = unsafe {
                    std::slice::from_raw_parts(input1.data.as_ptr() as *const i16, patch_len * 2)
                };

                let fir1=fir_coeffs.clone();

                resample2(
                    input_raw,
                    &mut output_raw1,
                    &fir1,
                    state.lock().unwrap().deref_mut(),
                    bit_shift,
                );

                let input_raw = unsafe {
                        std::slice::from_raw_parts(input2.data.as_ptr() as *const i16, patch_len * 2)
                    };

                resample2(
                    input_raw,
                    &mut output_raw2,
                    &fir1,
                    state.lock().unwrap().deref_mut(),
                    bit_shift,
                );
            }

            yield output;
            // if tx.send(output).await.is_err() {
            //     // 如果 ReceiverStream 被 drop 了，这里会退出
            //     //println!("err");
            //     break;
            // }else{
            //     //println!("a");
            // }
        }
    }
    //});

    //ReceiverStream::new(rx)
}

pub fn decim2_chained(
    input: impl Stream<Item = LinearOwnedReusable<Payload<Complex<i16>>>> + 'static + Send,
    fir_coeffs: &[i16],
    bit_shifts: &[u32],
    buffer_size: usize,
) -> Pin<Box<dyn Stream<Item = LinearOwnedReusable<Payload<Complex<i16>>>> + 'static + Send>> {
    let mut output: Pin<
        Box<dyn Stream<Item = LinearOwnedReusable<Payload<Complex<i16>>>> + Send + 'static>,
    > = Box::pin(input);
    for &bs in bit_shifts {
        let s = decim2(output, fir_coeffs, bs);
        let s = with_buffer(s, buffer_size);
        output = Box::pin(s);
    }
    output
}

pub fn fir_pipeline(
    input: Pin<Box<dyn Stream<Item = LinearOwnedReusable<Payload<Complex<i16>>>> + 'static + Send>>,
    fir_coeffs: &[i16],
    bit_shift: u32,
) -> Pin<Box<dyn Stream<Item = LinearOwnedReusable<Payload<Complex<i16>>>> + 'static + Send>> {
    let fir_coeffs = fir_coeffs.to_vec();
    let _fir_coeffs_i32: Vec<std::simd::Simd<i32, 16>> =
        fir_coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();
    let patch_len = N_BYTE_PER_FRAME / std::mem::size_of::<Complex<i16>>();

    //let (tx, rx) = mpsc::channel(buffer_size);
    //tokio::spawn(async move {

    let ntaps = fir_coeffs.len();
    let state_len = ntaps * 2 - 1 + patch_len; // 2:1 decimation, so input is 2x output

    let pool: Arc<LinearObjectPool<Payload<Complex<i16>>>> = Arc::new(LinearObjectPool::new(
        move || {
            //eprint!("o");
            Payload::<Complex<i16>>::default()
        },
        |_v| {},
    ));

    let s = stream! {
        futures_util::pin_mut!(input);
        let mut state = vec![Complex::<i16>::zero(); state_len];
        let state_raw =
            unsafe { std::slice::from_raw_parts_mut(state.as_mut_ptr() as *mut i16, state_len * 2) };
        loop{
            if let Some(input) = input.next().await {
                assert_eq!(input.data.len(), patch_len);
                let input_raw = unsafe {
                    std::slice::from_raw_parts(input.data.as_ptr() as *const i16, patch_len * 2)
                };

                let mut output = pool.pull_owned();
                output.copy_header(&input);
                let mut output_raw = unsafe {
                std::slice::from_raw_parts_mut(
                    output.data.as_mut_ptr() as *mut i16,
                    patch_len * 2,
                )
                };

                fir_symmetric_full_rate(
                    input_raw,
                    &mut output_raw,
                    &fir_coeffs,
                    state_raw,
                    bit_shift,
                );

                yield output;
            }else{
                break;
            }
        }
    };

    Box::pin(s)

    //ReceiverStream::new(rx)
}
