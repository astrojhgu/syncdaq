use lockfree_object_pool::LinearOwnedReusable;
use std::{fs::File, io::Write, net::UdpSocket};

use clap::Parser;
use crossbeam::channel::unbounded;
use syncdaq::{
    payload::Payload,
    pipeline::recv_pkt,
    utils::{as_u8_slice, set_recv_buffer_size},
};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short = 'a', long = "addr", value_name = "ip:port")]
    local_addr: String,

    #[clap(short = 'o', long = "out", value_name = "out name")]
    outname: Option<String>,

    #[clap(short = 'F', value_name = "out prefix for full dump file")]
    full_dump_name: Option<String>,

    #[clap(
        short = 'k',
        value_name = "number of pkts per full dump file",
        default_value = "1000000"
    )]
    npkt_per_full_dump: usize,

    #[clap(short = 'n', value_name = "npkts_per_dump", default_value = "100")]
    npkt_per_dump: usize,

    #[clap(short = 'm', value_name = "dumps per npkt", default_value = "100000")]
    dump_per_npkt: usize,

    #[clap(short = 'p', value_name = "npkts to dump")]
    npkts_to_recv: Option<usize>,
}

fn main() {
    //let (tx,rx)=bounded(256);
    let args = Args::parse();

    let socket = UdpSocket::bind(&args.local_addr).expect("failed to bind local addr");
    set_recv_buffer_size(&socket, 10 * 1024 * 1024 * 1024).unwrap();
    //let (tx, rx) = bounded::<LinearOwnedReusable<Payload>>(65536);
    let (tx, rx) = unbounded::<LinearOwnedReusable<Payload>>();
    let (_tx_cmd, rx_cmd) = unbounded();
    //let pool1 = Arc::clone(&pool);
    std::thread::spawn(|| recv_pkt(socket.into(), tx, rx_cmd));

    let mut npkt_to_dump = 0;
    let mut dump_file = None;

    let mut old_cnt = None;
    let mut full_dump_cnt = 0;
    let mut full_dump_file = args.full_dump_name.as_ref().map(|n| {
        File::create(format!("{}{}.dat", &n, full_dump_cnt)).expect("failed to create file")
    });
    let mut npkts_full_dump = 0;
    let mut total_npkts_received = 0;

    loop {
        let payload = rx.recv().expect("failed to recv payload");

        if payload.pkt_cnt % 100000 == 0 {
            println!("cnt: {} queue cnt: {}", payload.pkt_cnt, rx.len());
        }

        if let Some(c) = old_cnt
            && payload.pkt_cnt != 0
            && c + 1 != payload.pkt_cnt
        {
            eprintln!("dropped {}", payload.pkt_cnt - c - 1);
        }

        old_cnt = Some(payload.pkt_cnt);

        if payload.pkt_cnt as usize % args.dump_per_npkt == 0
            && args.npkt_per_dump > 0
            && let Some(ref outname) = args.outname
        {
            dump_file = Some(File::create(outname).expect("failed to create dump file"));
            npkt_to_dump = args.npkt_per_dump;
            println!("dump file created");
        }

        if let Some(ref mut f) = dump_file {
            let data = as_u8_slice(&payload.data);
            f.write_all(data).expect("failed to write");
            npkt_to_dump -= 1;
            if npkt_to_dump == 0 {
                dump_file = None;
                println!("dump file saved");

                println!(
                    "pkt_cnt: {}, port_id: {}",
                    payload.pkt_cnt, payload.port_id
                );
            }
        }

        if let Some(ref mut f) = full_dump_file {
            let data = as_u8_slice(&payload.data);
            f.write_all(data).expect("failed to write");
            npkts_full_dump += 1;

            if npkts_full_dump == args.npkt_per_full_dump {
                full_dump_cnt += 1;
                full_dump_file = args.full_dump_name.as_ref().map(|n| {
                    File::create(format!("{n}{full_dump_cnt}.dat")).expect("failed to create")
                });
                npkts_full_dump = 0;
            }
        }

        total_npkts_received += 1;
        if let Some(n) = args.npkts_to_recv
            && total_npkts_received == n
        {
            break;
        }

        if payload.pkt_cnt == 0 {
            full_dump_cnt = 0;
            npkts_full_dump = 0;
            total_npkts_received = 0;
            full_dump_file = args.full_dump_name.as_ref().map(|n| {
                File::create(format!("{n}{full_dump_cnt}.dat")).expect("failed to create file")
            });
        }
    }
}
