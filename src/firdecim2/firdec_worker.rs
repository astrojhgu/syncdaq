use super::{F32s, I16s, I32s, LANES8, LANES16};
use std::simd::{Simd, StdFloat, num::SimdInt, simd_swizzle};

pub fn resample2_plain(
    input: &[i16],
    output: &mut [i16],
    coeffs: &[i16], // 假设这里传入的是半个周期的系数（包含中心点）
    state: &mut [i16],
    bit_shift: u32,
) {
    // 1. 准备完整系数
    // 半带滤波器特性：除了中心点，偶数项为 0。
    // 假设 coeffs 传入的是对称的一半，例如 [c0, 0, c2, 0, c_center]
    let fir_coeffs_full: Vec<i32> = coeffs
        .iter()
        .skip(1) // 跳过中心点，避免重复
        .rev()
        .chain(coeffs.iter())
        .map(|&x| x as i32)
        .collect();
    //println!("coeff: {:?}", fir_coeffs_full);
    let n_full_taps = fir_coeffs_full.len();
    let n_input = input.len() / 2; //in complex samples
    let n_old_state = n_full_taps - 1;

    // --- 断言校验 ---
    assert!(
        state.len() / 2 >= n_old_state + n_input, //in complex samples
        "状态空间不足以容纳历史数据和当前输入"
    );
    let n_output = n_input / 2;
    assert_eq!(output.len() / 2, n_output, "输出长度必须是输入长度的一半"); // in complex samples
    assert_eq!(n_input % 2, 0, "输入长度必须是2的倍数以进行下采样");

    // 2. 将输入加载到 state 缓冲区（紧随历史数据之后）
    state[n_old_state * 2..n_old_state * 2 + n_input * 2].copy_from_slice(input);

    // 3. 滤波与下采样
    // 这里的 i 是输入索引，由于是 1/2 下采样，我们每次跳 2 个样本
    for i in 0..n_output {
        let mut acc_re = 0i32;
        let mut acc_im = 0i32;

        // 滑动窗口起始点
        let window: &[i16] = &state[i * 4..i * 4 + n_full_taps * 2]; // 每个复数占 2 个 i16

        for (sample, &coeff) in window.chunks(2).zip(fir_coeffs_full.iter()) {
            acc_re += (sample[0] as i32) * (coeff as i32);
            acc_im += (sample[1] as i32) * (coeff as i32);
        }

        // 缩放并转回 i16
        //output[i] = Complex::new((acc_re >> bit_shift) as i16, (acc_im >> bit_shift) as i16);
        output[i * 2] = (acc_re >> bit_shift) as i16; // I
        output[i * 2 + 1] = (acc_im >> bit_shift) as i16; // Q
    }

    // 4. 更新状态：将本次输入的末尾部分移至 state 开头，供下次迭代使用
    state.copy_within(n_input * 2..n_input * 2 + n_old_state * 2, 0);
}

#[inline(always)]
pub fn resample2(
    input: &[i16],
    output: &mut [i16],
    coeffs: &[i16],
    state: &mut [i16],
    bit_shift: u32,
) {
    let coeffs_i32: Vec<std::simd::Simd<i32, 16>> =
        coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();

    let n_half_taps = coeffs_i32.len();
    let m_half = n_half_taps - 1;
    let n_input = input.len();
    let n_output = output.len();
    let n_old_state = m_half * 4;

    state[n_old_state..n_old_state + n_input].copy_from_slice(input);

    let shift_vec = I32s::splat(bit_shift as i32);

    for j in 0..(n_output / LANES16) {
        let out_idx = j * LANES16;
        let state_offset = 2 * out_idx + (m_half * 2);

        // --- 中心 Tap ---
        let mut acc0 = extract_even_iq(&state[state_offset..]) * coeffs_i32[0];
        let mut acc1 = I32s::splat(0); // 第二个累加器，打破流水线依赖

        // --- 展开循环 (每步处理 2 个 tap，即 4 个对称点) ---
        let mut k = 1;
        while k + 2 <= m_half {
            // 第一组
            let c_a = coeffs_i32[k];
            let p_a = extract_even_iq(&state[state_offset + k * 2..]);
            let n_a = extract_even_iq(&state[state_offset - k * 2..]);
            acc0 += (p_a + n_a) * c_a;

            // 第二组
            let c_b = coeffs_i32[k + 2];
            let p_b = extract_even_iq(&state[state_offset + (k + 2) * 2..]);
            let n_b = extract_even_iq(&state[state_offset - (k + 2) * 2..]);
            acc1 += (p_b + n_b) * c_b;

            k += 4; // 步进 4
        }

        // 处理剩余的 k (如果有)
        while k <= m_half {
            let c = coeffs_i32[k];
            let p = extract_even_iq(&state[state_offset + k * 2..]);
            let n = extract_even_iq(&state[state_offset - k * 2..]);
            acc0 += (p + n) * c;
            k += 2;
        }

        // 合并累加器
        let acc = acc0 + acc1;

        let shifted = acc >> shift_vec;
        let out_simd: Simd<i16, LANES16> = shifted.cast::<i16>();
        output[out_idx..out_idx + LANES16].copy_from_slice(out_simd.as_array());
    }

    state.copy_within(n_input..n_input + n_old_state, 0);
}


#[inline(always)]
pub fn resample2_gen(
    input: &[i16],
    output: &mut [i16],
    coeffs: &[i16], // 现在传入完整的系数数组，不再假定偶数索引为0
    state: &mut [i16],
    bit_shift: u32,
) {
    let coeffs_i32: Vec<I32s> =
        coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();

    let n_half_taps = coeffs_i32.len();
    let m_half = n_half_taps - 1;
    let n_input = input.len();
    let n_output = output.len();
    
    // 保持原来的状态长度不变，以兼容你外层的 buffer 逻辑
    let n_old_state = m_half * 4; 

    state[n_old_state..n_old_state + n_input].copy_from_slice(input);

    let shift_vec = I32s::splat(bit_shift as i32);

    for j in 0..(n_output / LANES16) {
        let out_idx = j * LANES16;
        let state_offset = 2 * out_idx + (m_half * 2);

        // --- 中心 Tap (k = 0) ---
        // --- 中心 Tap ---
        let mut acc0 = extract_even_iq(&state[state_offset..]) * coeffs_i32[0];
        let mut acc1 = I32s::splat(0);
        let mut acc2 = I32s::splat(0);
        let mut acc3 = I32s::splat(0);

        // --- 展开循环 (每步处理 4 个 tap) ---
        let mut k = 1;
        while k + 3 <= m_half {
            // 第 1 组
            let c_a = coeffs_i32[k];
            let p_a = extract_even_iq(&state[state_offset + k * 2..]);
            let n_a = extract_even_iq(&state[state_offset - k * 2..]);
            acc0 += (p_a + n_a) * c_a;

            // 第 2 组
            let c_b = coeffs_i32[k + 1];
            let p_b = extract_even_iq(&state[state_offset + (k + 1) * 2..]);
            let n_b = extract_even_iq(&state[state_offset - (k + 1) * 2..]);
            acc1 += (p_b + n_b) * c_b;

            // 第 3 组
            let c_c = coeffs_i32[k + 2];
            let p_c = extract_even_iq(&state[state_offset + (k + 2) * 2..]);
            let n_c = extract_even_iq(&state[state_offset - (k + 2) * 2..]);
            acc2 += (p_c + n_c) * c_c;

            // 第 4 组
            let c_d = coeffs_i32[k + 3];
            let p_d = extract_even_iq(&state[state_offset + (k + 3) * 2..]);
            let n_d = extract_even_iq(&state[state_offset - (k + 3) * 2..]);
            acc3 += (p_d + n_d) * c_d;

            k += 4;
        }

        // --- 处理剩余的 k (0 到 3 个) ---
        while k <= m_half {
            let c = coeffs_i32[k];
            let p = extract_even_iq(&state[state_offset + k * 2..]);
            let n = extract_even_iq(&state[state_offset - k * 2..]);
            acc0 += (p + n) * c;
            k += 1;
        }

        // 合并所有累加器
        let acc = acc0 + acc1 + acc2 + acc3;
        
        let shifted = acc >> shift_vec;
        let out_simd: Simd<i16, LANES16> = shifted.cast::<i16>();
        output[out_idx..out_idx + LANES16].copy_from_slice(out_simd.as_array());
    }

    state.copy_within(n_input..n_input + n_old_state, 0);
}

// 保持这个高效的 swizzle 不变，但确保它内联
#[inline(always)]
fn extract_even_iq(src: &[i16]) -> Simd<i32, LANES16> {
    // 强制使用对齐加载（如果可能）或者直接 from_slice
    let s = Simd::<i16, 32>::from_slice(&src[0..32]);
    let picked = simd_swizzle!(
        s,
        [0, 1, 4, 5, 8, 9, 12, 13, 16, 17, 20, 21, 24, 25, 28, 29]
    );
    picked.cast::<i32>()
}


#[inline(always)]
pub fn fir_symmetric_full_rate_plain(
    input: &[i16],
    output: &mut [i16],
    coeffs: &[i16], // 从中心向外
    state: &mut [i16],
    bit_shift: u32,
) {
    assert_eq!(input.len(), output.len());

    let m_half = coeffs.len();          // M
    let n_full_taps = m_half * 2;       // 2M
    let n_input = input.len() / 2;      // complex samples
    let n_output = output.len() / 2;

    assert_eq!(n_input, n_output);
    assert!(m_half > 0);

    let n_old_state = n_full_taps - 1;

    // 拼接 state
    state[n_old_state * 2..n_old_state * 2 + input.len()]
        .copy_from_slice(input);

    assert_eq!(
        state.len(),
        n_old_state * 2 + input.len(),
        "状态空间不足"
    );

    for n in 0..n_output {

        let center = n + m_half - 1; // 对齐到 FIR 中心左侧

        let mut acc_re: i32 = 0;
        let mut acc_im: i32 = 0;

        for k in 0..m_half {
            // 对称点：x[n-k] 和 x[n+k+1]
            let left_idx  = (center - k) * 2;
            let right_idx = (center + k + 1) * 2;

            let re = state[left_idx] as i32 + state[right_idx] as i32;
            let im = state[left_idx + 1] as i32 + state[right_idx + 1] as i32;

            let c = coeffs[k] as i32;

            acc_re += re * c;
            acc_im += im * c;
        }

        output[2 * n]     = (acc_re >> bit_shift) as i16;
        output[2 * n + 1] = (acc_im >> bit_shift) as i16;
    }

    // 更新 state
    state.copy_within(input.len()..input.len() + n_old_state * 2, 0);
}

#[inline(always)]
fn extract_i(src: &[i16]) -> Simd<i32, LANES16> {
    let s = Simd::<i16, 32>::from_slice(&src[0..32]);
    let picked = simd_swizzle!(
        s,
        [0, 2, 4, 6, 8, 10, 12, 14,
         16, 18, 20, 22, 24, 26, 28, 30]
    );
    picked.cast::<i32>()
}

#[inline(always)]
fn extract_q(src: &[i16]) -> Simd<i32, LANES16> {
    let s = Simd::<i16, 32>::from_slice(&src[0..32]);
    let picked = simd_swizzle!(
        s,
        [1, 3, 5, 7, 9, 11, 13, 15,
         17, 19, 21, 23, 25, 27, 29, 31]
    );
    picked.cast::<i32>()
}



#[inline(always)]
pub fn fir_symmetric_full_rate(
    input: &[i16],
    output: &mut [i16],
    coeffs: &[i16], // 从中心向外
    state: &mut [i16],
    bit_shift: u32,
) {
    const LANES: usize = 16;
    type I32s = Simd<i32, LANES>;
    type I16s = Simd<i16, LANES>;

    assert_eq!(input.len(), output.len());

    let m_half = coeffs.len();
    let n_full_taps = m_half * 2;

    let n_input = input.len() / 2;
    let n_output = output.len() / 2;

    assert_eq!(n_input, n_output);
    assert!(m_half > 0);

    let n_old_state = n_full_taps - 1;

    assert_eq!(
        state.len(),
        n_old_state * 2 + input.len(),
        "状态空间不足"
    );

    // --- 拼接 state ---
    state[n_old_state * 2..n_old_state * 2 + input.len()]
        .copy_from_slice(input);

    

    // --- SIMD 系数 ---
    let coeffs_i32: Vec<I32s> =
        coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();

    let shift_vec = I32s::splat(bit_shift as i32);

    // --- 主循环：每次处理 16 个复数 ---
    for j in 0..(n_output / LANES) {

        let base_n = j * LANES;

        let mut acc_i = I32s::splat(0);
        let mut acc_q = I32s::splat(0);

        // FIR 累加
        for k in 0..m_half {

            // 对齐中心（和 plain 版本完全一致）
            let center = base_n + m_half - 1;

            let left  = (center - k) * 2;
            let right = (center + k + 1) * 2;

            // SIMD load
            let li = extract_i(&state[left..]);
            let lq = extract_q(&state[left..]);

            let ri = extract_i(&state[right..]);
            let rq = extract_q(&state[right..]);

            let c = coeffs_i32[k];

            acc_i += (li + ri) * c;
            acc_q += (lq + rq) * c;
        }

        // --- shift ---
        let out_i: I16s = (acc_i >> shift_vec).cast();
        let out_q: I16s = (acc_q >> shift_vec).cast();

        // --- 写回（interleave）---
        for lane in 0..LANES {
            let idx = 2 * (base_n + lane);
            output[idx]     = out_i[lane];
            output[idx + 1] = out_q[lane];
        }
    }

    // --- 更新 state ---
    state.copy_within(input.len()..input.len() + n_old_state * 2, 0);
}


/*
#[inline(always)]
pub fn fir_symmetric_full_rate(
    input: &[i16],
    output: &mut [i16],
    coeffs: &[i16], 
    state: &mut [i16],
    bit_shift: u32,
) {
    let m_half = coeffs.len();
    let n_full_taps = m_half * 2;
    let n_old_state_elems = (n_full_taps - 1) * 2;
    let n_input_elems = input.len();

    // --- 严格长度校验 ---
    assert_eq!(state.len(), n_old_state_elems + n_input_elems);
    state[n_old_state_elems..].copy_from_slice(input);

    let coeffs_i32: Vec<I32s> = coeffs.iter().map(|&c| I32s::splat(c as i32)).collect();
    let shift_vec = I32s::splat(bit_shift as i32);

    // 每次处理 16 个 i16 (即 8 个 IQ 对)
    // 使用 step_by(16)
    for base_idx in (0..n_input_elems).step_by(LANES16) {
        let mut acc0 = I32s::splat(0);
        let mut acc1 = I32s::splat(0);

        for k in 0..m_half {
            let c = coeffs_i32[k];
            
            // 对应 plain 逻辑：left = (i+k)*2, right = (i + n_full - 1 - k)*2
            // 这里我们直接按 i16 索引操作
            let left_idx = base_idx + k * 2;
            let right_idx = base_idx + (n_full_taps - 1 - k) * 2;

            // --- 安全加载策略 ---
            // 只有当索引 + 16 会超过 state.len() 时才需要特殊处理
            let l_vec = if left_idx + LANES16 <= state.len() {
                I16s::from_slice(&state[left_idx..left_idx + LANES16])
            } else {
                // 这种边界情况在精确长度下只会发生在 right_idx，
                // 但为了严谨，我们处理剩余部分
                load_partial(&state, left_idx)
            };

            let r_vec = if right_idx + LANES16 <= state.len() {
                I16s::from_slice(&state[right_idx..right_idx + LANES16])
            } else {
                load_partial(&state, right_idx)
            };

            let sum = l_vec.cast::<i32>() + r_vec.cast::<i32>();
            
            if k % 2 == 0 { acc0 += sum * c; } else { acc1 += sum * c; }
        }

        let res = (acc0 + acc1) >> shift_vec;
        output[base_idx..base_idx + LANES16].copy_from_slice(res.cast::<i16>().as_array());
    }

    state.copy_within(n_input_elems..n_input_elems + n_old_state_elems, 0);
}

// 辅助函数：处理末尾不满 16 个元素的加载，防止越界
#[inline(always)]
fn load_partial(slice: &[i16], start: usize) -> I16s {
    let mut tmp = [0i16; LANES16];
    let len = slice.len() - start;
    tmp[..len].copy_from_slice(&slice[start..]);
    I16s::from_array(tmp)
}
*/

#[cfg(test)]
mod tests {
    use super::resample2_plain;
    //use crate::fir;
    use super::super::fir_coeffs::fir_half_band_coeffs;
    //use num::Complex;
    //use num::traits::FloatConst;
    //use num::traits::Zero;
    //use std::fs::File;
    //use std::io::Write;
    const N_BATCH: usize = 512;

    #[test]
    fn unit_pulse_complex() {
        let fir_coeffs = fir_half_band_coeffs(); // 假设这是半带滤波器的前一半系数（含中心点）

        // 生成完整的滤波器系数用于比对
        // 注意：半带滤波器的偶数项（除了中心点）通常为 0
        let fir_coeffs_full: Vec<i32> = fir_coeffs
            .iter()
            .rev()
            .chain(fir_coeffs.iter().skip(1))
            .map(|&x| x as i32)
            .collect();

        let n_tap_half = fir_coeffs.len();
        let m_half = n_tap_half - 1;

        // input 长度 512 个 i16，代表 256 个 Complex
        let mut input = vec![0i16; N_BATCH];

        // 设置第一个复数为 1 + 1i
        input[0] = 1; // I0
        input[1] = 1; // Q0

        // 状态空间：(ntaps - 1) * 2 是历史复数点占用的 i16 数量
        let n_old_state = m_half * 2 * 2;
        let mut state = vec![0i16; n_old_state + N_BATCH];

        // 输出长度减半
        let mut output = vec![0i16; N_BATCH / 2];

        // 调用优化后的函数

        resample2_plain(&input, &mut output, &fir_coeffs, &mut state, 0);

        // --- 验证逻辑 ---
        // 对于单位脉冲 [1, 1, 0, 0, ...]，输出应该是滤波器的系数
        // 但因为是 2:1 降采样，输出只会保留偶数项的响应
        // 预期输出序列应该是：[h[0], h[0], h[2], h[2], h[4], h[4] ...] (如果脉冲在位置0)
        // 注意：h[k] 对应 fir_coeffs_full 中的值

        fir_coeffs_full
            .iter()
            .step_by(2) // 降采样 2 对应的系数步进
            .enumerate()
            .for_each(|(idx, &expected_val)| {
                let out_re = output[idx * 2]; // 输出的 I
                let out_im = output[idx * 2 + 1]; // 输出的 Q

                println!(
                    "TapIdx {}: Expected {}, Got I={}, Q={}",
                    idx, expected_val, out_re, out_im
                );

                assert_eq!(out_re, expected_val as i16, "实部不匹配 @ index {}", idx);
                assert_eq!(out_im, expected_val as i16, "虚部不匹配 @ index {}", idx);
            });
    }

    #[test]
    fn test_segmented_consistency() {
        // 1. 准备参数
        let coeff = fir_half_band_coeffs();
        let n_half_taps = coeff.len();
        let m_half = n_half_taps - 1;
        let n_old_state = 2 * m_half;
        let bit_shift = 2; // 示例位移

        // 构造一段足够长的随机输入数据 (必须是 LANES*2 的倍数)
        let total_input_len = 512;
        let input: Vec<i16> = (0..total_input_len as i16).collect();

        // --- 实验组 A: 一次性处理 ---
        let mut state_a = vec![0i16; n_old_state * 2 + total_input_len];
        let mut output_a = vec![0i16; total_input_len / 2];
        resample2_plain(&input, &mut output_a, &coeff, &mut state_a, bit_shift);
        println!("output a: {:?}", output_a);

        // --- 实验组 B: 分两段处理 ---
        let mut state_b = vec![0i16; n_old_state * 2 + total_input_len / 2]; // 状态空间需足够容纳单次输入
        let mut output_b = vec![0i16; total_input_len / 2];

        let mid_input = total_input_len / 2; // 从中间切分
        let mid_output = mid_input / 2;

        // 第一段：处理前一半
        // 注意：state 的长度在 resample2 中有断言检查，传入的 state 切片长度必须符合约定
        resample2_plain(
            &input[..mid_input],
            &mut output_b[..mid_output],
            &coeff,
            &mut state_b,
            bit_shift,
        );

        // 第二段：处理后一半 (此时 state_b 内部已经自动完成了 copy_within)
        resample2_plain(
            &input[mid_input..],
            &mut output_b[mid_output..],
            &coeff,
            &mut state_b,
            bit_shift,
        );

        println!("output b: {:?}", output_b);

        // --- 验证结果 ---
        // 检查 output_a 和 output_b 是否逐元素相等
        assert_eq!(output_a.len(), output_b.len(), "输出长度不一致");
        for i in 0..output_a.len() {
            assert_eq!(
                output_a[i], output_b[i],
                "分段处理在索引 {} 处不一致！A: {}, B: {}",
                i, output_a[i], output_b[i]
            );
        }
        println!("分段等效性测试通过！");
    }
}
