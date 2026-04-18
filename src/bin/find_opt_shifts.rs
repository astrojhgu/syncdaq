use std::{fs::File, io::Write};

use clap::Parser;
use itertools::Itertools;
use syncdaq::sdr::Sdr16Decim;
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(
        short = 'L',
        long = "local-addr",
        value_name = "ip:port",
        default_value = "0.0.0.0:3001",
        help = "local ctrl addr"
    )]
    local_addr: String,

    #[clap(
        short = 'a',
        long = "addr",
        value_name = "ip:port",
        help = "remote ctrl addr"
    )]
    ctrl_addr: String,

    #[clap(
        short = 'p',
        long = "pp",
        value_name = "ip:port",
        help = "local payload addr"
    )]
    local_payload_addr: String,

    #[clap(short = 'd', long = "ndecim", value_name = "num of decim stages")]
    ndecim: usize,

    #[clap(short = 'r', long = "fir", help = "use or not use fir filter")]
    use_fir: bool,

    #[clap(
        short = 'z',
        long = "nsafebits",
        value_name = "number of safe bits",
        default_value = "2"
    )]
    n_safe_bits: u32,

    #[clap(
        short = 'n',
        long = "nbuf",
        value_name = "num of buf",
        help = "number of buf to receive"
    )]
    nbuf: usize,

    #[clap(short = 'f', long = "freq-MHz", value_name = "freq in MHz")]
    freq: f64,
}

fn main() {
    let args = Args::parse();

    let mut sdr = Sdr16Decim::new(
        args.ctrl_addr.parse().unwrap(),
        args.local_addr.parse().unwrap(),
        args.local_payload_addr.parse().unwrap(),
        Some("/home/user/src/syncdaq/init_rfsoc.yaml"),
    );
    sdr.ctrl.set_mixer_freq(args.freq, 0);
    sdr.ctrl.stream_start();
    let mut shifts = vec![0; args.ndecim];

    for i in 0..args.ndecim {
        for d in 0..=24 {
            shifts[i] = d;
            let rx = sdr.setup_stream(&shifts[..=i], None);
            let mut good = true;
            for _j in 0..args.nbuf {
                let payload = rx.recv().unwrap();
                if payload.data.iter().any(|x| {
                    x.re.abs() >= (i16::MAX >> args.n_safe_bits)
                        || x.im.abs() > (i16::MAX >> args.n_safe_bits)
                }) {
                    good = false;
                }
            }
            if good {
                println!("shifts={}", shifts.iter().format(":"));
                break;
            }
        }
    }
    let mut shift_file = File::create("opt_shift.txt").unwrap();

    writeln!(&mut shift_file, "shifts={}", shifts.iter().format(":")).unwrap();
    if args.use_fir {
        let mut fir_shift = 0;
        for d in 0..=24 {
            fir_shift = d;
            let rx = sdr.setup_stream(&shifts, Some(fir_shift));
            let mut good = true;
            for _j in 0..args.nbuf {
                let payload = rx.recv().unwrap();
                if payload.data.iter().any(|x| {
                    x.re.abs() >= (i16::MAX >> args.n_safe_bits)
                        || x.im.abs() > (i16::MAX >> args.n_safe_bits)
                }) {
                    good = false;
                }
            }

            if good {
                println!("firshift={}", fir_shift);
                break;
            }
        }
        //println!("firshift={}", fir_shift);
        writeln!(&mut shift_file, "firshift={}", fir_shift).unwrap();
    }
    sdr.destroy_recv_thread();
}
