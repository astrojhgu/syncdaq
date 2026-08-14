#![allow(incomplete_features)]
#![feature(portable_simd)]

use std::time::Instant;

use clap::Parser;
use num::Complex;
use syncdaq::{
    firdecim2::{
        fir_coeffs::{fir_anti_aliasing_coeffs, fir_half_band_coeffs},
        firdec_worker::{
            fir_symmetric_full_rate, fir_symmetric_full_rate_plain, resample2, resample2_plain,
        },
    },
    payload::N_BYTE_PER_FRAME,
};

#[derive(Parser, Debug)]
struct Args {
    #[clap(short = 'n', default_value = "1000000")]
    npkts: usize,
}

fn bench_resample2(npkts: usize) -> (f64, f64) {
    let coeffs = fir_half_band_coeffs();
    let patch_len = N_BYTE_PER_FRAME / std::mem::size_of::<Complex<i16>>();
    let state_len = coeffs.len() * 2 - 2 + patch_len;
    let bit_shift = 13;

    let mut rng = (0x1234u64..).map(|i| (i.wrapping_mul(1103515245).wrapping_add(12345)) >> 16);
    let input: Vec<i16> = (0..patch_len * 2).map(|_| (rng.next().unwrap() & 0xffff) as i16).collect();
    let mut output = vec![0i16; patch_len];
    let mut sink: i32 = 0;

    let mut t_simd = 0.0;
    let mut t_plain = 0.0;

    for _ in 0..3 {
        let mut state = vec![0i16; state_len * 2];
        let start = Instant::now();
        for _ in 0..npkts {
            resample2(&input, &mut output, &coeffs, &mut state, bit_shift);
            sink = sink.wrapping_add(output[0] as i32);
        }
        t_simd = start.elapsed().as_secs_f64();

        let mut state = vec![0i16; state_len * 2];
        let start = Instant::now();
        for _ in 0..npkts {
            resample2_plain(&input, &mut output, &coeffs, &mut state, bit_shift);
            sink = sink.wrapping_add(output[0] as i32);
        }
        t_plain = start.elapsed().as_secs_f64();
    }

    eprintln!("sink = {}", sink);
    (t_simd, t_plain)
}

fn bench_fir_full_rate(npkts: usize) -> (f64, f64) {
    let coeffs = fir_anti_aliasing_coeffs();
    let patch_len = N_BYTE_PER_FRAME / std::mem::size_of::<Complex<i16>>();
    let state_len = coeffs.len() * 2 - 1 + patch_len;
    let bit_shift = 13;

    let input = vec![0i16; patch_len * 2];
    let mut output = vec![0i16; patch_len * 2];

    let mut t_simd = 0.0;
    let mut t_plain = 0.0;

    for _ in 0..3 {
        let mut state = vec![0i16; state_len * 2];
        let start = Instant::now();
        for _ in 0..npkts {
            fir_symmetric_full_rate(&input, &mut output, &coeffs, &mut state, bit_shift);
        }
        t_simd = start.elapsed().as_secs_f64();

        let mut state = vec![0i16; state_len * 2];
        let start = Instant::now();
        for _ in 0..npkts {
            fir_symmetric_full_rate_plain(&input, &mut output, &coeffs, &mut state, bit_shift);
        }
        t_plain = start.elapsed().as_secs_f64();
    }

    (t_simd, t_plain)
}

fn main() {
    let args = Args::parse();
    let npkts = args.npkts;

    let patch_len = N_BYTE_PER_FRAME / std::mem::size_of::<Complex<i16>>();

    let (t_rs_simd, t_rs_plain) = bench_resample2(npkts);
    println!("resample2 (half-band 1/2 decim):");
    println!(
        "  simd : {:.2} ns/pkt ({:.2} Msamp/s), {:.2} GB/s",
        t_rs_simd / npkts as f64 * 1e9,
        npkts as f64 / t_rs_simd * (patch_len as f64 / 2.0 / 1e6),
        npkts as f64 / t_rs_simd * (patch_len as f64 * 2.0) / 1e9
    );
    println!(
        "  plain: {:.2} ns/pkt ({:.2} Msamp/s), {:.2} GB/s",
        t_rs_plain / npkts as f64 * 1e9,
        npkts as f64 / t_rs_plain * (patch_len as f64 / 2.0 / 1e6),
        npkts as f64 / t_rs_plain * (patch_len as f64 * 2.0) / 1e9
    );

    let (t_fir_simd, t_fir_plain) = bench_fir_full_rate(npkts);
    println!("fir_symmetric_full_rate (full rate FIR):");
    println!(
        "  simd : {:.2} ns/pkt ({:.2} Msamp/s), {:.2} GB/s",
        t_fir_simd / npkts as f64 * 1e9,
        npkts as f64 / t_fir_simd * (patch_len as f64 / 1e6),
        npkts as f64 / t_fir_simd * (patch_len as f64 * 4.0) / 1e9
    );
    println!(
        "  plain: {:.2} ns/pkt ({:.2} Msamp/s), {:.2} GB/s",
        t_fir_plain / npkts as f64 * 1e9,
        npkts as f64 / t_fir_plain * (patch_len as f64 / 1e6),
        npkts as f64 / t_fir_plain * (patch_len as f64 * 4.0) / 1e9
    );
}
