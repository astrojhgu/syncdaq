#![allow(static_mut_refs)]

use crossbeam::channel::Receiver;
use lockfree_object_pool::LinearOwnedReusable;
use num::Complex;

use crate::{
    ctrl_msg::{CtrlMsg, bcast_cmd},
    payload::{Payload, n_pt_per_frame},
    sdr::{Sdr, Sdr16Decim},
};

use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    simd::{Simd, num::SimdInt},
    slice::{from_raw_parts, from_raw_parts_mut},
    time::Duration,
};

fn convert_simd(src: &[i16], dst: &mut [f32]) {
    assert!(src.len() == dst.len());
    const CHK_LEN: usize = 64;

    //type Vf32 = Simd<f32, CHK_LEN>;
    type Vi16 = Simd<i16, CHK_LEN>;

    let chunks = src.len() / CHK_LEN;

    for i in 0..chunks {
        let vi = Vi16::from_slice(&src[i * CHK_LEN..]);
        let vf = vi.cast::<f32>();
        vf.copy_to_slice(&mut dst[i * CHK_LEN..]);
    }

    // 处理尾部
    for i in (chunks * CHK_LEN)..src.len() {
        dst[i] = src[i] as f32;
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
    rx_payload: Receiver<LinearOwnedReusable<Payload<Complex<i16>>>>,
    buffer: Option<LinearOwnedReusable<Payload<Complex<i16>>>>,
    cursor: usize,
}


#[unsafe(no_mangle)]
pub unsafe extern "C" fn fetch_data_16(csdr: *mut CSdr16Decim, buf: *mut CComplex, npt: usize) {
    if csdr.is_null() {
        return;
    }

    let obj = unsafe { &mut *csdr };
    let buf = unsafe { std::slice::from_raw_parts_mut(buf as *mut Complex<i16>, npt) };
    if obj.buffer.is_none() {
        obj.buffer = Some(obj.rx_payload.recv().unwrap());
        if obj.rx_payload.len() >= 16 {
            println!("almost full");
        }
        obj.cursor = 0;
    }

    let mut written = 0;
    let total = npt;
    while written < total {
        let available = n_pt_per_frame::<i16>() - obj.cursor;
        if available == 0 {
            obj.buffer = Some(obj.rx_payload.recv().unwrap());
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
        let buf_ci16= &obj.buffer.as_ref().unwrap().data;
        buf[written..written + copy_len]
            .copy_from_slice(&buf_ci16[obj.cursor..obj.cursor + copy_len]);
        obj.cursor += copy_len;
        written += copy_len;
    }
}


/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub extern "C" fn get_mtu() -> usize {
    n_pt_per_frame::<i16>()
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn start_data_stream(csdr: *mut CSdr16Decim) {
    let obj = unsafe { &mut *csdr };
    obj.sdr_dev.ctrl.stream_start();
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn set_mixer_freq(csdr: *mut CSdr16Decim, freq_mega_hz: f64, sync: u32) {
    let obj = unsafe { &mut *csdr };
    obj.sdr_dev.ctrl.set_mixer_freq(freq_mega_hz, sync);
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stop_data_stream(csdr: *mut CSdr16Decim) {
    let obj = unsafe { &mut *csdr };
    obj.sdr_dev.ctrl.stream_stop();
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
pub extern "C" fn use_payload_ci16(_p: Payload<CComplex>) {}


#[unsafe(no_mangle)]
pub extern "C" fn make_sdr16_decim(
    ip: &[u8; 4],
    local_ctrl_port: u16,
    port_id: usize,
    decim_shifts: *const u32,
    ndecim_shifts: usize,
    anti_aliasing_shift: u32,
    cfg_file: *const std::ffi::c_char,
) -> *mut CSdr16Decim {
    let ip = Ipv4Addr::from(*ip);
    let decim_shifts = unsafe { from_raw_parts(decim_shifts, ndecim_shifts) };
    let c_str = if cfg_file.is_null() {
        None
    } else {
        Some(
            unsafe { std::ffi::CStr::from_ptr(cfg_file) }
                .to_string_lossy()
                .into_owned(),
        )
    };

    let (sdr_dev, rx_payload) = crate::device_discovery::make_sdr16_decim(
        ip,
        local_ctrl_port,
        port_id,
        decim_shifts,
        anti_aliasing_shift,
        c_str,
    )
    .unwrap();

    Box::into_raw(Box::new(CSdr16Decim {
        sdr_dev,
        rx_payload,
        buffer: None,
        cursor: 0,
    }))
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
        } = *obj;
        drop(rx_payload);
    }
}
