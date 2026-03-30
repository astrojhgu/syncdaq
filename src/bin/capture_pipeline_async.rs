#![allow(incomplete_features)]
#![feature(generic_const_exprs)]
use futures_util::{StreamExt, pin_mut};
use std::{
    net::SocketAddrV4,
};
use tokio::{
    fs::File,
    io::{AsyncWriteExt, BufWriter},
    net::UdpSocket,
};

use clap::Parser;

use syncdaq::{
    async_pipeline::recv_pkt,
    utils::as_u8_slice,
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

    #[clap(short = 'b', value_name = "buffer size in MB")]
    buffer_size_mega_byte: Option<usize>,
}

#[tokio::main]
async fn main() {
    //let (tx,rx)=bounded(256);
    let args = Args::parse();

    let addr = args.local_addr.parse::<SocketAddrV4>().unwrap();
    let buffer_size_mega_byte = args.buffer_size_mega_byte.unwrap_or(8);

    let socket = UdpSocket::bind(&addr).await.unwrap().into();

    //let pool1 = Arc::clone(&pool);
    let s = recv_pkt::<u8>(socket);

    pin_mut!(s);

    let mut npkt_to_dump = 0;
    let mut dump_file = None;

    let mut old_cnt = None;
    let mut full_dump_cnt = 0;
    let mut full_dump_file = None;

    if let Some(ref fname) = args.full_dump_name {
        full_dump_file = Some(BufWriter::with_capacity(
            buffer_size_mega_byte * 1024 * 1024,
            File::create(format!("{}{}.dat", fname, full_dump_cnt))
                .await
                .expect("Failed to create file"),
        ));
    }
    let mut npkts_full_dump = 0;
    let mut total_npkts_received = 0;

    while let Some(payload) = s.next().await {
        if payload.pkt_cnt % 100000 == 0 {
            println!("cnt: {}", payload.pkt_cnt);
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
            dump_file = Some(
                File::create(outname)
                    .await
                    .expect("failed to create dump file"),
            );
            npkt_to_dump = args.npkt_per_dump;
            println!("dump file created");
        }

        if let Some(ref mut f) = dump_file {
            let data = as_u8_slice(&payload.data);
            f.write_all(data).await.expect("failed to write");
            npkt_to_dump -= 1;
            if npkt_to_dump == 0 {
                dump_file = None;
                println!("dump file saved");

                println!("pkt_cnt: {}, port_id: {}", payload.pkt_cnt, payload.port_id);
            }
        }

        if let Some(ref mut f) = full_dump_file {
            let data = as_u8_slice(&payload.data);
            f.write_all(data).await.expect("failed to write");
            npkts_full_dump += 1;

            if npkts_full_dump == args.npkt_per_full_dump {
                full_dump_cnt += 1;

                if let Some(ref fname) = args.full_dump_name {
                    full_dump_file = Some(BufWriter::with_capacity(
                        buffer_size_mega_byte * 1024 * 1024,
                        File::create(format!("{fname}{full_dump_cnt}.dat"))
                            .await
                            .expect("failed to create"),
                    ));
                }
            };
            npkts_full_dump = 0;
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

            if let Some(ref fname) = args.full_dump_name {
                full_dump_file = Some(BufWriter::with_capacity(
                    buffer_size_mega_byte * 1024 * 1024,
                    File::create(format!("{fname}{full_dump_cnt}.dat"))
                        .await
                        .expect("failed to create file"),
                ));
            }
        }
    }
}
