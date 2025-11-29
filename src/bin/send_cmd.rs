use clap::Parser;
use syncdaq::ctrl_msg::{self, send_cmd, CtrlMsg};
use serde_yaml::from_reader;
use std::{fmt::Display, fs::File, time::Duration};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short = 'a', long = "addr", num_args(1..), value_name = "<ip:port> ...")]
    addr: Vec<String>,

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

#[derive(Debug)]

enum MsgError {
    NotAllReplied,
    HasInvalidReply,
    StatAbnormal,
}

impl Display for MsgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MsgError::NotAllReplied => write!(f, "not all replied"),
            MsgError::HasInvalidReply => write!(f, "has invalid reply"),
            MsgError::StatAbnormal => write!(f, "state abnormal"),
        }
    }
}


impl std::error::Error for MsgError {}

fn main()->Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let debug_level = args.debug_level;

    let cmds: Vec<CtrlMsg> = from_reader(File::open(&args.cmd).expect("file not open")).expect("failed to load cmd");
    for c in cmds {
        let summary = send_cmd(
            c,
            &args.addr,
            &args.local_addr,
            Some(Duration::from_secs(args.timeout)),
            debug_level,
        );

        for (_a,msg) in &summary.normal_reply{
            if let ctrl_msg::CtrlMsg::QueryReply { msg_id:_, fm_ver:_, tick_cnt1, tick_cnt2, trans_state:_, locked, health:_ }=msg.clone(){
                println!("{}", tick_cnt2-tick_cnt1);
                if tick_cnt2-tick_cnt1!=10_000_000 || locked&0x00_00_00_0f!=0x0f{
                    return Err(Box::new(MsgError::StatAbnormal))
                }
            }
        }
        

        if summary.no_reply.is_empty() {
            println!("all replied");
        } else {
            println!("not replied:");
            for (addr, msg_id) in &summary.no_reply {
                println!("{addr:?} {msg_id}");
            }
            return Err(Box::new(MsgError::NotAllReplied));
        }

        if !summary.invalid_reply.is_empty() {
            println!("Invalid reply:");
            for (a, r) in summary.invalid_reply {
                println!("{a} {r}");
            }
            return Err(Box::new(MsgError::HasInvalidReply));
        }
    }
    Ok(())
}
