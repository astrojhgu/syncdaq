#![allow(static_mut_refs)]

use crossbeam::channel::Receiver;
use lockfree_object_pool::LinearOwnedReusable;
use num::Complex;

use crate::{
    ctrl_msg::{send_cmd, CtrlMsg, Health},
    default_cfg::DEFAULT_CTRL_PORT,
    device_discovery::get_device_info,
    payload::{Payload, n_pt_per_frame},
    sdr::Sdr16Decim,
};

use std::{
    net::{Ipv4Addr, SocketAddrV4},
    slice::{from_raw_parts, from_raw_parts_mut},
};

#[target_feature(enable = "avx2")]
unsafe fn convert_i16_to_f32_simd(src: *const i16, dst: *mut f32, n: usize) {
    use std::arch::x86_64::*;

    let mut i = 0;

    while i + 16 <= n {
        // load 16 x i16
        let v = unsafe { _mm256_loadu_si256(src.add(i) as *const __m256i) };

        // low 8
        let lo = _mm256_cvtepi16_epi32(_mm256_castsi256_si128(v));
        let lo_ps = _mm256_cvtepi32_ps(lo);

        // high 8
        let hi = _mm256_cvtepi16_epi32(_mm256_extracti128_si256(v, 1));
        let hi_ps = _mm256_cvtepi32_ps(hi);

        unsafe { _mm256_storeu_ps(dst.add(i), lo_ps) };
        unsafe { _mm256_storeu_ps(dst.add(i + 8), hi_ps) };

        i += 16;
    }

    // tail
    while i < n {
        unsafe { *dst.add(i) = *src.add(i) as f32 };
        i += 1;
    }
}
//use sdaa_ctrl::ctrl_msg::{CtrlMsg, bcast_cmd, send_cmd};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CComplex {
    pub re: i16,
    pub im: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CComplexF32 {
    pub re: f32,
    pub im: f32,
}

pub struct CSdr16Decim {
    sdr_dev: Sdr16Decim,
    rx_payload: Option<Receiver<LinearOwnedReusable<Payload<Complex<i16>>>>>,
    buffer: Option<LinearOwnedReusable<Payload<Complex<i16>>>>,
    cursor: usize,
    decim_shifts: Vec<u32>,
    fir_shift: Option<u32>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fetch_data_16(csdr: *mut CSdr16Decim, buf: *mut CComplex, npt: usize) {
    if csdr.is_null() {
        return;
    }

    let obj = unsafe { &mut *csdr };
    let buf = unsafe { std::slice::from_raw_parts_mut(buf as *mut Complex<i16>, npt) };
    if let Some(ref mut rx_payload) = obj.rx_payload {
        if obj.buffer.is_none() {
            obj.buffer = Some(rx_payload.recv().unwrap());
            if rx_payload.len() >= 16 {
                println!("almost full");
            }
            obj.cursor = 0;
        }

        let mut written = 0;
        let total = npt;
        while written < total {
            let available = n_pt_per_frame::<i16>() - obj.cursor;
            if available == 0 {
                obj.buffer = Some(rx_payload.recv().unwrap());
                obj.cursor = 0;
                continue;
            }
            let copy_len = (total - written).min(available);
            // let buf_ci16 = unsafe {
            //     from_raw_parts(
            //         obj.buffer.as_ref().unwrap().data.as_ptr() as *const Complex<i16>,
            //         n_pt_per_frame::<i16>(),
            //     )
            // };
            let buf_ci16 = &obj.buffer.as_ref().unwrap().data;
            buf[written..written + copy_len]
                .copy_from_slice(&buf_ci16[obj.cursor..obj.cursor + copy_len]);
            obj.cursor += copy_len;
            written += copy_len;
        }
    } else {
        panic!("recv thread must be started before hand");
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fetch_data_cf32(
    csdr: *mut CSdr16Decim,
    buf: *mut CComplexF32,
    npt: usize,
) {
    if csdr.is_null() {
        return;
    }

    let obj = unsafe { &mut *csdr };

    let buf = unsafe { std::slice::from_raw_parts_mut(buf, npt) };

    if let Some(ref mut rx_payload) = obj.rx_payload {
        if obj.buffer.is_none() {
            let x = match rx_payload.recv() {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("{}", e);
                    panic!("{}", e)
                }
            };
            obj.buffer = Some(x);
            obj.cursor = 0;
        }

        let mut written = 0;
        let total = npt;

        while written < total {
            let available = n_pt_per_frame::<i16>() - obj.cursor;

            if available == 0 {
                obj.buffer = Some(rx_payload.recv().unwrap());
                obj.cursor = 0;
                continue;
            }

            let copy_len = (total - written).min(available);

            let src = &obj.buffer.as_ref().unwrap().data;

            // ⚠️ reinterpret 为 i16 流
            let src_ptr = unsafe { src.as_ptr().add(obj.cursor) } as *const i16;
            let dst_ptr = unsafe { buf.as_mut_ptr().add(written) } as *mut f32;

            // 每个 Complex = 2 个 i16
            let n_scalar = copy_len * 2;

            unsafe { convert_i16_to_f32_simd(src_ptr, dst_ptr, n_scalar) };

            obj.cursor += copy_len;
            written += copy_len;
        }
    } else {
        panic!("recv thread must be started before hand");
    }
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub extern "C" fn get_mtu() -> usize {
    n_pt_per_frame::<i16>()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn setup_data_stream(
    csdr: *mut CSdr16Decim,
    decim_shifts: *const u32,
    ndecim_stages: usize,
    fir_shift: i32,
) {
    if csdr.is_null() {
        dbg!("null dev");
        return;
    }
    //let obj = unsafe { &mut *csdr };
    let obj = unsafe { &mut *csdr };

    obj.decim_shifts = unsafe { from_raw_parts(decim_shifts, ndecim_stages) }.to_vec();
    obj.fir_shift = if fir_shift >= 0 {
        Some(fir_shift as u32)
    } else {
        None
    };

    //let decim_shifts = [12];
    //let fir_shift = 5;
    obj.rx_payload = Some(obj.sdr_dev.setup_stream(&obj.decim_shifts, obj.fir_shift));
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn start_data_stream(csdr: *mut CSdr16Decim) {
    if csdr.is_null() {
        dbg!("null dev");
        return;
    }
    let obj = unsafe { &mut *csdr };

    if obj.rx_payload.is_none() {
        obj.rx_payload = Some(obj.sdr_dev.setup_stream(&obj.decim_shifts, obj.fir_shift));
    }
    assert!(obj.rx_payload.is_some());
    //let decim_shifts = unsafe { from_raw_parts(decim_shifts, ndecim_stages) };
    obj.sdr_dev.ctrl.stream_start();
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn set_mixer_freq(csdr: *mut CSdr16Decim, freq_mega_hz: f64, sync: u32) {
    if csdr.is_null() {
        dbg!("null dev");
        return;
    }
    let obj = unsafe { &mut *csdr };
    obj.sdr_dev.ctrl.set_mixer_freq(freq_mega_hz, sync);
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stop_data_stream(csdr: *mut CSdr16Decim) {
    if csdr.is_null() {
        dbg!("null dev");
        return;
    }
    let obj = unsafe { &mut *csdr };
    obj.sdr_dev.destroy_recv_thread();
    obj.sdr_dev.ctrl.stream_stop();
    obj.rx_payload = None;
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn find_all_devices(
    result: *mut u32,
    max_n: usize,
    local_port: u16,
) -> usize {
    let devices = crate::device_discovery::get_all_device_info(local_port).unwrap_or_default();
    let n = devices.len().min(max_n);

    let result_slice = unsafe { from_raw_parts_mut(result, n) };

    for (i, device) in devices.into_iter().take(n).enumerate() {
        let ip = match device.ctrl_addr.ip() {
            std::net::IpAddr::V4(ipv4) => ipv4,
            _ => continue,
        };
        result_slice[i] = u32::from(ip);
    }
    n
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn device_is_alive(ip: u32) -> bool {
    let ip = Ipv4Addr::from(ip);
    get_device_info(Ipv4Addr::from(ip)).is_some()
}

/// Query the device's ADC sample rate in MSps (e.g. 320 or 100).
/// Returns 0 if the device did not reply.
#[unsafe(no_mangle)]
pub extern "C" fn get_device_smp_rate(ip_u32: u32, local_ctrl_port: u16) -> u32 {
    let ip = Ipv4Addr::from(ip_u32);
    let remote = SocketAddrV4::new(ip, DEFAULT_CTRL_PORT);
    let summary = send_cmd(
        CtrlMsg::Query { msg_id: 0 },
        &[remote],
        SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, local_ctrl_port),
        Some(std::time::Duration::from_secs(2)),
        0,
    );
    for (_addr, msg) in &summary.normal_reply {
        if let CtrlMsg::QueryReply {
            msg_id: _,
            fm_ver: _,
            tick_cnt1: _,
            tick_cnt2: _,
            trans_state: _,
            locked: _,
            health: Health::T510Health { smp_rate, .. },
        } = msg
        {
            return *smp_rate;
        }
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn use_payload_ci16(_p: Payload<CComplex>) {}

#[unsafe(no_mangle)]
pub extern "C" fn make_sdr16_decim_u32(
    ip_u32: u32,
    local_ctrl_port: u16,
    port_id: usize,
    cfg_file: *const std::ffi::c_char,
) -> *mut CSdr16Decim {
    let ip = Ipv4Addr::from(ip_u32);
    let c_str = if cfg_file.is_null() {
        None
    } else {
        Some(
            unsafe { std::ffi::CStr::from_ptr(cfg_file) }
                .to_string_lossy()
                .into_owned(),
        )
    };

    crate::device_discovery::make_sdr16_decim(ip, local_ctrl_port, port_id, c_str)
        .map(|sdr_dev| CSdr16Decim {
            sdr_dev,
            rx_payload: None,
            buffer: None,
            cursor: 0,
            fir_shift: None,
            decim_shifts: Vec::default(),
        })
        .map(Box::new)
        .map(Box::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub extern "C" fn make_sdr16_decim(
    ip: &[u8; 4],
    local_ctrl_port: u16,
    port_id: usize,
    cfg_file: *const std::ffi::c_char,
) -> *mut CSdr16Decim {
    let ip = Ipv4Addr::from(*ip);
    let c_str = if cfg_file.is_null() {
        None
    } else {
        Some(
            unsafe { std::ffi::CStr::from_ptr(cfg_file) }
                .to_string_lossy()
                .into_owned(),
        )
    };

    crate::device_discovery::make_sdr16_decim(ip, local_ctrl_port, port_id, c_str)
        .map(|sdr_dev| CSdr16Decim {
            sdr_dev,
            rx_payload: None,
            buffer: None,
            cursor: 0,
            fir_shift: None,
            decim_shifts: Vec::default(),
        })
        .map(Box::new)
        .map(Box::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_sdr_device(csdr: *mut CSdr16Decim) {
    if !csdr.is_null() {
        let obj = unsafe { Box::from_raw(csdr) };
        let CSdr16Decim {
            sdr_dev: _,
            rx_payload,
            buffer: _,
            cursor: _,
            decim_shifts: _,
            fir_shift: _,
        } = *obj;
        drop(rx_payload);
    }
}
