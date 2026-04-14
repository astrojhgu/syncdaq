use std::io::Write;

use syncdaq::{sdr::Sdr16Decim, utils::as_u8_slice};

fn main() {
    //let (sdr, rx)= Sdr::new::<&str, Complex<i16>>("192.168.5.255:3000".parse().unwrap(), "0.0.0.0:3001".parse().unwrap(),"0.0.0.0:4000".parse().unwrap(), Some("/home/user/src/syncdaq/init_rfsoc.yaml"));

    let mut sdr = Sdr16Decim::new(
        "192.168.5.255:3000".parse().unwrap(),
        "0.0.0.0:3001".parse().unwrap(),
        "0.0.0.0:4000".parse().unwrap(),
        Some("/home/user/src/syncdaq/init_rfsoc.yaml"),
    );

    let mut outfile = std::fs::File::create("a.bin").unwrap();

    let rx = sdr.setup_stream(&[15], Some(8));

    sdr.ctrl.stream_start();

    for _i in 0..101 {
        if let Ok(payload) = rx.recv() {
            let d = as_u8_slice(&payload.data);
            outfile.write_all(d).unwrap();
        } else {
            break;
        }
    }

    println!("stream started");
    //std::thread::sleep(Duration::from_secs(1));
}
