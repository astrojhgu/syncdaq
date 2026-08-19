use async_stream::stream;
use futures_core::Stream;
use futures_util::StreamExt;
use lockfree_object_pool::{LinearObjectPool, LinearOwnedReusable};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use std::{pin::Pin, sync::Arc};

use num::Complex;

use crate::firdecim2::firdec_worker::fir_symmetric_full_rate_streams_inplace;

use super::{
    super::payload::{N_BYTE_PER_FRAME, Payload},
    I32s,
    firdec_worker::{FirStreamState, StreamState, resample2_streams},
};

pub fn with_buffer<S>(upstream: S) -> impl Stream<Item = S::Item>
where
    S: Stream + Send + 'static, // 去掉了 Unpin 约束
    S::Item: Send + 'static,
{
    let (tx, rx) = mpsc::unbounded_channel();

    // 在这里进行 Pin
    let mut pinned_upstream = Box::pin(upstream);
    
    tokio::spawn(async move {
        // 现在可以使用 pinned_upstream，因为 Pin<Box<S>> 实现了 Unpin
        while let Some(item) = pinned_upstream.next().await {
            //println!("pkt cnt: {}", item.pkt_cnt);
            if tx.send(item).is_err() {
                break;
            }
        }
    });

    UnboundedReceiverStream::new(rx)
}

pub fn decim2(
    input: impl Stream<Item = LinearOwnedReusable<Payload<Complex<i16>>>> + Send + 'static,
    fir_coeffs: &[i16],
    bit_shift: u32,
) -> impl Stream<Item = LinearOwnedReusable<Payload<Complex<i16>>>> + Send + 'static {
    let fir_coeffs = fir_coeffs.to_vec();
    let fir_coeffs_i32: Vec<I32s> = fir_coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();
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

    let state = StreamState::new();

    stream! {
        futures_util::pin_mut!(input);

        let mut batched_input = input.chunks(2);
        let mut state = state;
        loop{
            let mut output = pool.pull_owned();


            if let Some(batch)=batched_input.next().await{
                if batch.len()!=2{
                    break;
                }
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

                let input1=&batch[0];
                let input2=&batch[1];

                output.copy_header(input1);
                output.pkt_cnt /=2; // because of 2:1 decimation
                //println!("pkt CNT:{} {}", input1.pkt_cnt, input2.pkt_cnt);
                let input_raw = unsafe {
                    std::slice::from_raw_parts(input1.data.as_ptr() as *const i16, patch_len * 2)
                };

                resample2_streams(
                    input_raw,
                    &mut output_raw1,
                    &fir_coeffs_i32,
                    &mut state,
                    bit_shift,
                );

                let input_raw = unsafe {
                        std::slice::from_raw_parts(input2.data.as_ptr() as *const i16, patch_len * 2)
                    };

                resample2_streams(
                    input_raw,
                    &mut output_raw2,
                    &fir_coeffs_i32,
                    &mut state,
                    bit_shift,
                );
            }

            yield output;
        }
    }
    //});

    //ReceiverStream::new(rx)
}

pub fn decim2_chained(
    input: impl Stream<Item = LinearOwnedReusable<Payload<Complex<i16>>>> + 'static + Send,
    fir_coeffs: &[i16],
    bit_shifts: &[u32],
) -> Pin<Box<dyn Stream<Item = LinearOwnedReusable<Payload<Complex<i16>>>> + 'static + Send>> {
    let mut output: Pin<
        Box<dyn Stream<Item = LinearOwnedReusable<Payload<Complex<i16>>>> + Send + 'static>,
    > = Box::pin(input);
    for &bs in bit_shifts {
        let s = decim2(output, fir_coeffs, bs);
        let s = with_buffer(s);
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
    let fir_coeffs_i32: Vec<std::simd::Simd<i32, 16>> =
        fir_coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();
    let patch_len = N_BYTE_PER_FRAME / std::mem::size_of::<Complex<i16>>();

    let s = stream! {
        futures_util::pin_mut!(input);
        let mut state = FirStreamState::new();
        loop{
            if let Some(mut input) = input.next().await {
                assert_eq!(input.data.len(), patch_len);
                let input_raw = unsafe {
                    std::slice::from_raw_parts_mut(input.data.as_mut_ptr() as *mut i16, patch_len * 2)
                };

                fir_symmetric_full_rate_streams_inplace(input_raw, &fir_coeffs_i32, &mut state, bit_shift);

                yield input;
            }else{
                break;
            }
        }
    };

    Box::pin(s)

    //ReceiverStream::new(rx)
}
