#![allow(incomplete_features)]
#![feature(portable_simd)]

// Behavior-consistency test for the master branch.
// Feed fixed inputs to resample2 (half-band 2:1 decim) and
// fir_symmetric_full_rate (full-rate FIR), print deterministic results.
// Compare the output of this program against the _aiopt twin on the
// ai_opt branch to verify behavioral consistency between branches.

use syncdaq::{
    firdecim2::{
        fir_coeffs::{fir_anti_aliasing_coeffs, fir_half_band_coeffs},
        firdec_worker::{
            fir_symmetric_full_rate, fir_symmetric_full_rate_plain, resample2, resample2_plain,
        },
    },
};

// ---------- deterministic test vector generator ----------
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u16 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 32) as u16
    }
}

// full-range pseudo-random i16 (fixed seed -> deterministic across runs/branches)
fn gen_input(n: usize, seed: u64) -> Vec<i16> {
    let mut rng = Rng(seed);
    (0..n).map(|_| rng.next() as i16).collect()
}

// ---------- output helpers ----------
fn checksum(v: &[i16]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &x in v {
        h ^= x as u16 as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn print_vec(tag: &str, v: &[i16]) {
    println!("{} len={} crc={:016x}", tag, v.len(), checksum(v));
    let mut line = String::new();
    for (i, &x) in v.iter().enumerate() {
        if i % 16 == 0 {
            if i > 0 {
                println!("{}", line);
                line.clear();
            }
        }
        if !line.is_empty() {
            line.push(',');
        }
        line.push_str(&x.to_string());
    }
    if !line.is_empty() {
        println!("{}", line);
    }
}

fn main() {
    // ---- Scenario 1: resample2 (half-band 2:1 decimation), streamed in blocks ----
    let hb = fir_half_band_coeffs();
    let bs_vals = [0u32, 13u32];

    const BLOCK_IN: usize = 128; // i16 = 64 complex input
    const N_BLOCKS: usize = 6;
    let input = gen_input(BLOCK_IN * N_BLOCKS, 0x1234_5678);

    // resample2 (SIMD) — master state buffer
    for &bs in &bs_vals {
        let mut out_acc: Vec<i16> = Vec::new();
        let state_len = (hb.len() - 1) * 4 + BLOCK_IN;
        let mut state = vec![0i16; state_len];
        for b in 0..N_BLOCKS {
            let inp = &input[b * BLOCK_IN..(b + 1) * BLOCK_IN];
            let mut out = vec![0i16; BLOCK_IN / 2];
            resample2(inp, &mut out, &hb, &mut state, bs);
            out_acc.extend_from_slice(&out);
        }
        print_vec(&format!("resample2 bs={}", bs), &out_acc);
    }

    // resample2_plain reference (identical code on both branches)
    for &bs in &bs_vals {
        let mut out_acc: Vec<i16> = Vec::new();
        let state_len = (hb.len() - 1) * 4 + BLOCK_IN;
        let mut state = vec![0i16; state_len];
        for b in 0..N_BLOCKS {
            let inp = &input[b * BLOCK_IN..(b + 1) * BLOCK_IN];
            let mut out = vec![0i16; BLOCK_IN / 2];
            resample2_plain(inp, &mut out, &hb, &mut state, bs);
            out_acc.extend_from_slice(&out);
        }
        print_vec(&format!("resample2_plain bs={}", bs), &out_acc);
    }

    // ---- Scenario 2: fir_symmetric_full_rate (full-rate anti-alias FIR) ----
    let aa = fir_anti_aliasing_coeffs();

    const FIR_BLOCK: usize = 128; // i16 = 64 complex
    const FIR_BLOCKS: usize = 6;
    let input2 = gen_input(FIR_BLOCK * FIR_BLOCKS, 0xDEAD_BEEF);

    for &bs in &bs_vals {
        let mut out_acc: Vec<i16> = Vec::new();
        let state_len = (aa.len() * 2 - 1) * 2 + FIR_BLOCK;
        let mut state = vec![0i16; state_len];
        for b in 0..FIR_BLOCKS {
            let inp = &input2[b * FIR_BLOCK..(b + 1) * FIR_BLOCK];
            let mut out = vec![0i16; FIR_BLOCK];
            fir_symmetric_full_rate(inp, &mut out, &aa, &mut state, bs);
            out_acc.extend_from_slice(&out);
        }
        print_vec(&format!("fir bs={}", bs), &out_acc);
    }

    for &bs in &bs_vals {
        let mut out_acc: Vec<i16> = Vec::new();
        let state_len = (aa.len() * 2 - 1) * 2 + FIR_BLOCK;
        let mut state = vec![0i16; state_len];
        for b in 0..FIR_BLOCKS {
            let inp = &input2[b * FIR_BLOCK..(b + 1) * FIR_BLOCK];
            let mut out = vec![0i16; FIR_BLOCK];
            fir_symmetric_full_rate_plain(inp, &mut out, &aa, &mut state, bs);
            out_acc.extend_from_slice(&out);
        }
        print_vec(&format!("fir_plain bs={}", bs), &out_acc);
    }
}
