use std::{fs::File, io::Write, net::UdpSocket};

use clap::Parser;
use syncdaq::{
    payload::Payload,
    utils::{as_mut_u8_slice, as_u8_slice},
};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short = 'a', long = "addr", value_name = "ip:port")]
    local_addr: String,

    #[clap(short = 'o', long = "out", value_name = "out name")]
    outname: Option<String>,

    #[clap(short = 'n', value_name = "npkts_per_dump")]
    npkt_per_dump: usize,

    #[clap(short = 'm', value_name = "dumps per npkt", default_value("100000"))]
    dump_per_npkt: usize,
}

fn main() {
    //let (tx,rx)=bounded(256);
    let args = Args::parse();

    let socket = UdpSocket::bind(&args.local_addr).unwrap();
    let mut payload = Payload::default();

    let x = as_mut_u8_slice(&mut payload);

    let mut npkt_to_dump = 0;
    let mut dump_file = None;

    let mut next_cnt = None;
    loop {
        let (s, _a) = socket.recv_from(x).unwrap();
        if s != std::mem::size_of::<Payload>() {
            continue;
        }

        if next_cnt.is_none() {
            next_cnt = Some(payload.pkt_cnt);
        }

        while let Some(ref mut c) = next_cnt {
            //let current_cnt = c + 1;

            if *c as usize % args.dump_per_npkt == 0
                && args.npkt_per_dump > 0
                && let Some(ref outname) = args.outname
            {
                dump_file = Some(File::create(outname).unwrap());
                npkt_to_dump = args.npkt_per_dump;
                println!("dump file created");
            }

            if let Some(ref mut f) = dump_file {
                let data = as_u8_slice(&payload.data);
                f.write_all(data).unwrap();
                npkt_to_dump -= 1;
                if npkt_to_dump == 0 {
                    dump_file = None;
                    println!("dump file saved");
                }
            }

            if *c >= payload.pkt_cnt {
                *c = payload.pkt_cnt + 1;
                break;
            }
            print!(".");

            *c += 1;
        }

        if payload.pkt_cnt % 10000 == 0 {
            println!("{}", payload.pkt_cnt);
        }
    }
}
