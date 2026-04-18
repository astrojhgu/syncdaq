use clap::Parser;
use syncdaq::device_discovery::get_device_info;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short = 'a', long = "addr", value_name = "ip:port")]
    addr: String,
}


fn main(){
    let args=Args::parse();
    let device_info=get_device_info(args.addr.parse().expect("invalid address"));
    println!("{:?}", device_info);
}