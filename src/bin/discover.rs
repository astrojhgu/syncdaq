use syncdaq::device_discovery::get_all_device_info;



fn main(){
    get_all_device_info(3001).unwrap().iter().for_each(|device| {
        println!("device: ctrl_addr={}, payload_addrs={:?}", device.ctrl_addr, device.payload_addr);
    });
}
