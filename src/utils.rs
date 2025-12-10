use std::{
    net::UdpSocket,
    os::fd::AsRawFd,
    slice::{from_raw_parts, from_raw_parts_mut},
};

use num::Complex;
use serde::de::{self, SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserializer,Serializer};
use std::fmt;


use libc::{SO_RCVBUF, SOL_SOCKET, setsockopt, socklen_t};

pub fn as_complex_t<'a, 'b, T:Sized>(input: &'a[u8])->&'b[Complex<T>]
where 
    'b: 'a
{
    let npt=input.len()/std::mem::size_of::<T>()/2;
    unsafe{from_raw_parts(input.as_ptr() as *const Complex<T>, npt)}
}


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


pub mod u8_hex_array {
    use super::*;

    pub fn serialize<S>(data: &[u8; 6], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(6))?;
        for byte in data.iter() {
            seq.serialize_element(byte)?; // 序列化为整数，不是字符串
        }
        seq.end()
    }

    
    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 6], D::Error>
    where
        D: Deserializer<'de>,
    {
        struct U8HexVisitor;

        impl<'de> Visitor<'de> for U8HexVisitor {
            type Value = [u8; 6];

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a list of 6 u8 values, decimal or 0x-prefixed hex")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<[u8; 6], A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut result = [0u8; 6];
                for i in 0..6 {
                    let value: serde_yaml::Value = seq
                        .next_element()?
                        .ok_or_else(|| de::Error::invalid_length(i, &self))?;

                    let parsed = match &value {
                        // YAML会将裸数字解析成Number，可以是十进制或十六进制
                        serde_yaml::Value::Number(n) => n
                            .as_u64()
                            .ok_or_else(|| de::Error::custom("invalid number"))?,
                        // 或者写成 "0x??" 的字符串，也接受
                        serde_yaml::Value::String(s) => {
                            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))
                            {
                                u8::from_str_radix(hex, 16)
                                    .map_err(|_| de::Error::custom("invalid hex string"))?
                                    as u64
                            } else {
                                s.parse::<u64>()
                                    .map_err(|_| de::Error::custom("invalid decimal string"))?
                            }
                        }
                        _ => return Err(de::Error::custom("expected number or string")),
                    };

                    if value > (u8::MAX as u64).into() {
                        return Err(de::Error::custom("value out of range for u8"));
                    }

                    result[i] = parsed as u8;
                }
                Ok(result)
            }
        }

        deserializer.deserialize_seq(U8HexVisitor)
    }
}
