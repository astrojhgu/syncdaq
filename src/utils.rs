use std::{
    net::UdpSocket,
    os::fd::AsRawFd,
    slice::{from_raw_parts, from_raw_parts_mut},
};

use libc::{SO_RCVBUF, SOL_SOCKET, setsockopt, socklen_t};

pub fn as_u8_slice<'a, 'b, T: Sized>(x: &'a T) -> &'b [u8]
where
    'b: 'a,
{
    unsafe { from_raw_parts((x as *const T) as *const u8, std::mem::size_of::<T>()) }
}

pub fn as_mut_u8_slice<'a, 'b, T: Sized>(x: &'a mut T) -> &'b mut [u8]
where
    'b: 'a,
{
    unsafe { from_raw_parts_mut((x as *mut T) as *mut u8, std::mem::size_of::<T>()) }
}

pub fn slice_as_u8<T: Sized>(x: &[T]) -> &[u8] {
    unsafe { from_raw_parts(x.as_ptr() as *const u8, std::mem::size_of_val(x)) }
}

pub fn set_recv_buffer_size(socket: &UdpSocket, size: usize) -> std::io::Result<()> {
    let fd = socket.as_raw_fd();
    let size = size as libc::c_int;

    let ret = unsafe {
        setsockopt(
            fd,
            SOL_SOCKET,
            SO_RCVBUF,
            &size as *const _ as *const libc::c_void,
            std::mem::size_of_val(&size) as socklen_t,
        )
    };

    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
