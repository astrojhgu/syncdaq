use clap::Parser;
use syncdaq::ctrl_msg::{bcast_cmd, CtrlMsg};
use serde_yaml::from_reader;
use std::{fs::File, time::Duration};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short = 'a', long = "addr", value_name = "<bcast_addr:port>")]
    addr: String,

    #[clap(
        short = 'L',
        long = "local addr",
        value_name = "local addr and port, default: [::]:3001",
        default_value("[::]:3001")
    )]
    local_addr: String,

    #[clap(short = 'c', long = "cmd", value_name = "cmd.yaml")]
    cmd: String,

    #[clap(short = 't', value_name = "timeout in sec", default_value = "1")]
    timeout: u64,

    #[clap(
        short = 'd',
        long = "debug",
        value_name = "debug level",
        default_value("0")
    )]
    debug_level: u32,
}

fn main() {
    let args = Args::parse();
    let debug_level = args.debug_level;

    let cmds: Vec<CtrlMsg> = from_reader(File::open(&args.cmd).expect("file not open")).expect("failed to load cmd");
    for c in cmds {
        let summary = bcast_cmd(
            c,
            &args.addr,
            &args.local_addr,
            Some(Duration::from_secs(args.timeout)),
            debug_level,
        );

        println!("replied:");

        for (a, r) in &summary.normal_reply {
            println!("{a} \n{r}");
        }

        if !summary.invalid_reply.is_empty() {
            println!("Invalid reply:");
            for (a, r) in summary.invalid_reply {
                println!("{a} \n{r}");
            }
        }
    }
}
