#![allow(static_mut_refs)]

use std::{net::{Ipv4Addr, SocketAddr, SocketAddrV4}, slice::from_raw_parts_mut, time::Duration};


use crossbeam::channel::{Receiver, Sender};
use lockfree_object_pool::LinearOwnedReusable;
use num::Complex;

use crate::{
    ctrl_msg::{CtrlMsg, bcast_cmd, send_cmd}, payload::{N_PT_PER_FRAME, Payload}, pipeline::RecvCmd, sdr::Sdr
};

//use sdaa_ctrl::ctrl_msg::{CtrlMsg, bcast_cmd, send_cmd};


pub struct CSdr {
    sdr_dev: Sdr,
    rx_payload: Receiver<LinearOwnedReusable<Payload>>,
    tx_cmd: Sender<RecvCmd>,
    buffer: Option<LinearOwnedReusable<Payload>>,
    cursor: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CComplex {
    pub re: i16,
    pub im: i16,
}


#[unsafe(no_mangle)]
pub extern "C" fn new_sdr_device(
    remote_ctrl_ip: u32,
    local_ctrl_port: u16,
    local_payload_ip: u32,
    local_payload_port: u16,
    cfg_file: *const std::ffi::c_char
) -> *mut CSdr {
    let c_str= unsafe { std::ffi::CStr::from_ptr(cfg_file) };
    let remote_ctrl_addr = SocketAddrV4::new(Ipv4Addr::from(remote_ctrl_ip), 3000);
    let local_ctrl_addr = SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), local_ctrl_port);
    let local_payload_addr =
        SocketAddrV4::new(Ipv4Addr::from(local_payload_ip), local_payload_port);

    let (sdr_dev, rx_payload, tx_cmd) =
        Sdr::new(remote_ctrl_addr, local_ctrl_addr, local_payload_addr, c_str.to_str().unwrap());

    

    Box::into_raw(Box::new(CSdr {
        sdr_dev,
        rx_payload,
        tx_cmd,
        buffer: None,
        cursor: 0,
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_sdr_device(csdr: *mut CSdr) {
    if !csdr.is_null() {
        let obj = unsafe { Box::from_raw(csdr) };
        let CSdr {
            sdr_dev: _,
            rx_payload,
            tx_cmd,
            buffer: _,
            cursor: _,
        } = *obj;
        tx_cmd.send(RecvCmd::Destroy).unwrap();
        drop(tx_cmd);
        drop(rx_payload);
    }
}


#[unsafe(no_mangle)]
pub unsafe extern "C" fn set_lo_freq(csdr: *mut CSdr, f_lo_mega_hz: f64) {
    if csdr.is_null() {
        return;
    }

    let obj = unsafe { &mut *csdr };
    //obj.tx_cmd.send(DdcCmd::LoCh(lo_ch as isize)).unwrap();
    let cmd=CtrlMsg::MixerSet { msg_id: 0, freq: f_lo_mega_hz, phase: 0.0, sync: 0 };
    let _reply=obj.sdr_dev.ctrl.send_cmd(cmd);
}


#[unsafe(no_mangle)]
pub unsafe extern "C" fn fetch_data(csdr: *mut CSdr, buf: *mut CComplex, npt: usize) {
    if csdr.is_null() {
        return;
    }

    let obj = unsafe { &mut *csdr };
    let buf = unsafe { std::slice::from_raw_parts_mut(buf as *mut Complex<i16>, npt) };
    if obj.buffer.is_none() {
        obj.buffer = Some(obj.rx_payload.recv().unwrap());
        obj.cursor = 0;
    }

    let mut written = 0;
    let total = npt;
    while written < total {
        let available = N_PT_PER_FRAME - obj.cursor;
        if available == 0 {
            obj.buffer = Some(obj.rx_payload.recv().unwrap());
            obj.cursor = 0;
            continue;
        }
        let copy_len = (total - written).min(available);
        buf[written..written + copy_len]
            .copy_from_slice(&obj.buffer.as_ref().unwrap().data[obj.cursor..obj.cursor + copy_len]);
        obj.cursor += copy_len;
        written += copy_len;
    }
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub extern "C" fn get_mtu() -> usize {
    N_PT_PER_FRAME
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn start_data_stream(csdr: *mut CSdr) {
    let obj = unsafe { &mut *csdr };
    obj.sdr_dev.ctrl.stream_start();
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn set_mixer_freq(csdr: *mut CSdr, freq_mega_hz: f64, sync: u32) {
    let obj = unsafe { &mut *csdr };
    obj.sdr_dev.ctrl.set_mixer_freq(freq_mega_hz, sync);
}


/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stop_data_stream(csdr: *mut CSdr) {
    let obj = unsafe { &mut *csdr };
    obj.sdr_dev.ctrl.stream_stop();
}




/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn find_device(
    addr: u32,
    result: *mut u32,
    max_n: usize,
    local_port: u16,
) -> usize {
    let result = unsafe{from_raw_parts_mut(result, max_n)};
    let ip = Ipv4Addr::from(addr);

    let addr = SocketAddrV4::new(ip, 3000);

    let query = CtrlMsg::Query { msg_id: 0 };

    let summary = bcast_cmd(
        query,
        addr,
        format!("0.0.0.0:{local_port}"),
        Some(Duration::from_secs(1)),
        1,
    );

    let mut nresult = 0;
    for (a, _r) in summary.normal_reply {
        if let SocketAddr::V4(x) = a {
            let ip = x.ip();
            let mut r: u32 = 0;
            for (i, &o) in ip.octets().iter().enumerate() {
                //(i as u32)
                r += (o as u32) << (8 * (3 - i));
            }

            nresult += 1;
            if nresult >= max_n {
                break;
            }
            result[nresult - 1] = r;
        }
    }
    nresult
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn make_device(ip: u32, local_port: u16) -> bool {
    let ip = Ipv4Addr::from(ip);
    let addr = SocketAddrV4::new(ip, 3000);

    let local_addr = format!("0.0.0.0:{local_port}");

    let cmd = CtrlMsg::Init {
        msg_id: 0,
        reserved_zeros: 0,
    };
    let summary = send_cmd(cmd, &[addr], &local_addr, Some(Duration::from_secs(5)), 1);

    println!("{summary:?}");

    // if summary.normal_reply.len() != 1 {
    //     return false;
    // }
    let cmd = CtrlMsg::Sync { msg_id: 0 };
    let _summary = send_cmd(cmd, &[addr], local_addr, Some(Duration::from_secs(5)), 1);

    // if summary.normal_reply.len() != 1 {
    //     return false;
    // }

    true
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn unmake_device(ip: u32, local_port: u16) -> bool {
    let ip = Ipv4Addr::from(ip);
    let addr = SocketAddrV4::new(ip, 3000);

    let local_addr = format!("0.0.0.0:{local_port}");

    let cmd = CtrlMsg::StreamStop { msg_id: 0 };

    let summary = send_cmd(cmd, &[addr], &local_addr, Some(Duration::from_secs(5)), 1);
    if summary.normal_reply.len() != 1 {
        return false;
    }

    true
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn start_stream(ip: u32, local_port: u16) -> bool {
    let ip = Ipv4Addr::from(ip);
    let addr = SocketAddrV4::new(ip, 3000);

    let local_addr = format!("0.0.0.0:{local_port}");

    let cmd = CtrlMsg::StreamStart { msg_id: 0 };

    let summary = send_cmd(cmd, &[addr], &local_addr, Some(Duration::from_secs(5)), 1);
    if summary.normal_reply.len() != 1 {
        return false;
    }

    true
}
