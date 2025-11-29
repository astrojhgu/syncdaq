use lockfree_object_pool::LinearOwnedReusable;
use std::{
    fs::File,
    io::{BufWriter, Write},
    net::UdpSocket,
};

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

    #[clap(short = 'p', value_name = "npkts to dump")]
    npkts_to_recv: Option<usize>,

    #[clap(short = 's', value_name = "npkts per file")]
    npkts_per_file: Option<usize>,

    #[clap(short = 'b', value_name = "buffer size in MB")]
    buffer_size_mega_byte: Option<usize>,
}

fn main() {
    //let (tx,rx)=bounded(256);
    let args = Args::parse();
    let buffer_size_mega_byte = args.buffer_size_mega_byte.unwrap_or(8);

    let socket = UdpSocket::bind(&args.local_addr).expect("failed to bind local addr");
    set_recv_buffer_size(&socket, 10 * 1024 * 1024 * 1024).unwrap();
    //let (tx, rx) = bounded::<LinearOwnedReusable<Payload>>(65536);
    let (tx, rx) = unbounded::<LinearOwnedReusable<Payload>>();
    let (_tx_cmd, rx_cmd) = unbounded();
    //let pool1 = Arc::clone(&pool);
    std::thread::spawn(|| recv_pkt(socket.into(), tx, rx_cmd));

    let mut npkts_received = 0;
    let mut current_file_no = 0;
    let mut current_file_pkts = 0;

    let mut dump_file = if let Some(ref fname) = args.outname {
        Some(BufWriter::with_capacity(
            buffer_size_mega_byte * 1024 * 1024,
            if args.npkts_per_file.is_some() {
                File::create(format!("{fname}{current_file_no}.bin"))
                    .expect("failed to create output file")
            } else {
                File::create(fname).expect("failed to create output file")
            },
        ))
    } else {
        None
    };

    loop {
        let payload = rx.recv().expect("failed to recv payload");

        if payload.pkt_cnt % 100000 == 0 {
            println!("cnt: {} queue cnt: {}", payload.pkt_cnt, rx.len());
        }

        // dump_file.as_mut().map(|f| {
        //     f.write_all(as_u8_slice(&payload.data)).expect("failed to write to dump file");
        // });

        if let Some(f) = dump_file.as_mut() {
            f.write_all(as_u8_slice(&payload.data))
                .expect("failed to write to dump file");
        }

        npkts_received += 1;
        current_file_pkts += 1;

        if let Some(n) = args.npkts_to_recv
            && npkts_received >= n
        {
            break;
        }

        if let Some(npkts_per_file) = args.npkts_per_file
            && let Some(ref fname) = args.outname
            && current_file_pkts >= npkts_per_file
            && npkts_per_file > 0
        {
            current_file_no += 1;
            current_file_pkts = 0;
            dump_file = Some(BufWriter::with_capacity(
                buffer_size_mega_byte * 1024 * 1024,
                File::create(format!("{fname}{current_file_no}.bin"))
                    .expect("failed to create output file"),
            ));
            println!("new file segment created")
        }
    }
}
