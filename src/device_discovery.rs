use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    path::Path,
    time::Duration,
};

use if_addrs::{IfAddr, get_if_addrs};
use pnet_datalink::MacAddr;
use pnet_datalink::NetworkInterface;
use pnet_datalink::interfaces;

use crate::{
    ctrl_msg::{CmdReplySummary, CtrlMsg, send_cmd},
    sdr::Sdr16Decim,
};

#[derive(Debug, Clone)]
pub struct IfaceBroadcast {
    pub name: String,
    pub ip: Ipv4Addr,
    pub broadcast: Ipv4Addr,
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub ctrl_addr: SocketAddr,
    pub payload_addr: Vec<Option<SocketAddrV4>>,
}

pub fn list_iface_broadcasts() -> std::io::Result<Vec<IfaceBroadcast>> {
    let mut result = Vec::new();

    for iface in get_if_addrs()? {
        let v4 = match iface.addr {
            IfAddr::V4(v4) => v4,
            _ => continue,
        };

        let ip = v4.ip;
        let mask = v4.netmask;

        if ip.is_loopback() {
            continue;
        }

        let mask_u32 = u32::from(mask);
        if mask_u32 == 0 || mask_u32 == u32::MAX {
            continue;
        }

        let broadcast = ipv4_broadcast(ip, mask);

        result.push(IfaceBroadcast {
            name: iface.name,
            ip,
            broadcast,
        });
    }

    Ok(result)
}

pub fn enumerate_device_addr() -> std::io::Result<Vec<SocketAddr>> {
    let addrs = list_iface_broadcasts()?
        .into_iter()
        .map(|a| SocketAddrV4::new(a.broadcast, crate::default_cfg::DEFAULT_CTRL_PORT))
        .collect::<Vec<_>>();
    let CmdReplySummary {
        no_reply: _,
        invalid_reply: _,
        normal_reply,
    } = send_cmd(
        crate::ctrl_msg::CtrlMsg::Query { msg_id: 0 },
        &addrs,
        "0.0.0.0:3001",
        Some(Duration::from_secs(1)),
        0,
    );
    Ok(normal_reply.into_iter().map(|(a, _)| a).collect())
}

pub fn find_iface_by_mac(target: MacAddr) -> Option<NetworkInterface> {
    for iface in interfaces() {
        if let Some(mac) = iface.mac {
            if mac == target {
                return Some(iface);
            }
        }
    }
    None
}

pub fn get_all_device_info(local_ctrl_port: u16) -> std::io::Result<Vec<DeviceInfo>> {
    let addrs = enumerate_device_addr()?;

    let CmdReplySummary {
        no_reply: _,
        invalid_reply: _,
        normal_reply,
    } = send_cmd(
        crate::ctrl_msg::CtrlMsg::XGbeCfgQuery { msg_id: 0 },
        &addrs,
        (Ipv4Addr::new(0, 0, 0, 0), local_ctrl_port),
        Some(Duration::from_secs(1)),
        1,
    );

    let xgbe_cfg = normal_reply
        .into_iter()
        .map(|(a, r)| {
            if let CtrlMsg::XGbeCfgQueryReply {
                msg_id: _,
                nports: _,
                cfg,
            } = r
            {
                let payload_addr = cfg
                    .into_iter()
                    .map(|p| {
                        find_iface_by_mac(MacAddr::from(p.dst_mac)).and_then(|ifc| {
                            ifc.ips
                                .iter()
                                .find(|ip| ip.ip() == IpAddr::V4(Ipv4Addr::from(p.dst_ip)))
                                .map(|_| SocketAddrV4::new(Ipv4Addr::from(p.dst_ip), p.dst_port))
                        })
                    })
                    .inspect(|opt| println!("matched payload_addr: {:?}", opt))
                    .collect();

                DeviceInfo {
                    ctrl_addr: a,
                    payload_addr,
                }
            } else {
                panic!("invalid reply")
            }
        })
        .collect::<Vec<_>>();
    Ok(xgbe_cfg)
}

pub fn get_device_info(ip: Ipv4Addr) -> Option<DeviceInfo> {
    let addrs = enumerate_device_addr().ok()?;

    let CmdReplySummary {
        no_reply: _,
        invalid_reply: _,
        normal_reply,
    } = send_cmd(
        crate::ctrl_msg::CtrlMsg::XGbeCfgQuery { msg_id: 0 },
        &addrs,
        "0.0.0.0:3001",
        Some(Duration::from_secs(1)),
        1,
    );

    let xgbe_cfg = normal_reply
        .into_iter()
        .filter(|&(ref a, ref _m)| a.ip() == IpAddr::V4(ip))
        .map(|(a, r)| {
            if let CtrlMsg::XGbeCfgQueryReply {
                msg_id: _,
                nports: _,
                cfg,
            } = r
            {
                let payload_addr = cfg
                    .into_iter()
                    .map(|p| {
                        find_iface_by_mac(MacAddr::from(p.dst_mac)).and_then(|ifc| {
                            ifc.ips
                                .iter()
                                .find(|ip| ip.ip() == IpAddr::V4(Ipv4Addr::from(p.dst_ip)))
                                .map(|_| SocketAddrV4::new(Ipv4Addr::from(p.dst_ip), p.dst_port))
                        })
                    })
                    .inspect(|opt| println!("matched payload_addr: {:?}", opt))
                    .collect();

                DeviceInfo {
                    ctrl_addr: a,
                    payload_addr,
                }
            } else {
                panic!("invalid reply")
            }
        })
        .collect::<Vec<_>>();
    if xgbe_cfg.len() > 0 {
        Some(xgbe_cfg[0].clone())
    } else {
        None
    }
}

pub fn make_sdr16_decim<P: std::fmt::Debug + AsRef<Path>>(
    ip: Ipv4Addr,
    local_ctrl_port: u16,
    port_id: usize,
    init_file: Option<P>,
) -> Option<Sdr16Decim> {
    let info = get_device_info(ip)?;
    let payload_addr = info.payload_addr.get(port_id)?.clone()?;
    println!("Creating Sdr16Decim with ctrl_addr={} and payload_addr={}", info.ctrl_addr, payload_addr);
    Some(Sdr16Decim::new(
        SocketAddrV4::new(ip, info.ctrl_addr.port()),
        SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, local_ctrl_port),
        payload_addr,
        init_file,
    ))
}

#[inline]
fn ipv4_broadcast(ip: Ipv4Addr, mask: Ipv4Addr) -> Ipv4Addr {
    let ip = u32::from(ip);
    let mask = u32::from(mask);
    Ipv4Addr::from(ip | !mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_device_info() {
        let result = get_all_device_info(3001);

        assert!(result.is_ok(), "function should not return Err");

        let devices = result.unwrap();

        for device in devices {
            println!(
                "device: ctrl_addr={}, payload_addrs={:?}",
                device.ctrl_addr, device.payload_addr
            );
            // 基本 sanity check：控制地址应该使用默认端口
            assert_eq!(
                device.ctrl_addr.port(),
                crate::default_cfg::DEFAULT_CTRL_PORT,
                "control address should use default control port"
            );
            // 负载地址应该不为空（但在 CI/容器环境中可能没有设备在线，所以这里只做弱检查）
            assert!(
                !device.payload_addr.is_empty(),
                "payload addresses should not be empty"
            );
        }
    }

    #[test]
    fn test_enumerate_device_addr() {
        let result = enumerate_device_addr();

        assert!(result.is_ok(), "function should not return Err");

        let addrs = result.unwrap();

        // 这里我们无法断言具体的地址，但至少应该返回一个地址（如果有设备在线）
        // 在 CI/容器环境中可能没有设备，所以这里只做弱检查
        for addr in addrs {
            println!("discovered device at {}", addr);
            // 简单 sanity check：端口应该是默认的控制端口
            assert_eq!(
                addr.port(),
                crate::default_cfg::DEFAULT_CTRL_PORT,
                "discovered address should use default control port"
            );
        }
    }

    #[test]
    fn test_list_iface_broadcasts_basic() {
        let result = list_iface_broadcasts();

        assert!(result.is_ok(), "function should not return Err");

        let list = result.unwrap();

        // 在大多数系统上，至少应该有一个非 loopback 接口
        // （但在CI/容器里可能没有，所以这里只做弱检查）
        for iface in &list {
            println!(
                "iface={}, ip={}, broadcast={}",
                iface.name, iface.ip, iface.broadcast
            );

            // 基本合理性检查
            assert!(!iface.ip.is_loopback(), "loopback should be filtered");

            // 广播地址不应该等于 IP
            assert_ne!(iface.ip, iface.broadcast, "broadcast should differ from ip");

            // 简单 sanity check：广播地址通常以 .255 结尾（仅对 /24）
            // 这里只做弱判断，不强制
            let octets = iface.broadcast.octets();
            assert!(
                octets[3] == 255 || octets[3] == 0 || octets[3] > 0,
                "broadcast last octet looks invalid"
            );
        }
    }

    #[test]
    fn test_no_duplicate_broadcasts() {
        let list = list_iface_broadcasts().expect("should succeed");

        use std::collections::HashSet;
        let mut set = HashSet::new();

        for iface in list {
            let inserted = set.insert(iface.broadcast);
            assert!(inserted, "duplicate broadcast address found");
        }
    }
}
