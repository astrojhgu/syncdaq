pub mod decim_pipeline;
pub mod decim_async_pipeline;
pub mod firdec_worker;
pub mod fir_coeffs;

pub type I32s = std::simd::Simd<i32, LANES16>;
pub const LANES16: usize = 16;

// 注意：在 AVX2 环境下，f32x8 (256-bit) 通常比 f32x16 (512-bit) 兼容性更好且稳定
pub const LANES8: usize = 8; 
type F32s = std::simd::Simd<f32, LANES8>;
