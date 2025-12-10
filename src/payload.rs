//use num::Complex;

pub const N_BYTE_PER_FRAME: usize = 8192;

pub const fn n_pt_per_frame<T: Sized>()->usize{
    N_BYTE_PER_FRAME/std::mem::size_of::<T>()/2
}

#[repr(C)]
pub struct Payload{
    pub head_magic: u32,
    pub version: u32,
    pub port_id:u32,
    pub data_type: u32,
    pub pkt_cnt: u64,
    pub tail_magic: u64,
    pub data: [u8; N_BYTE_PER_FRAME],
}

impl Default for Payload{
    fn default() -> Self {
        Self { head_magic: 0, version: 0, port_id: 0, data_type: 0, pkt_cnt: 0, tail_magic: 0, data: [0_u8; N_BYTE_PER_FRAME] }
    }
}

impl Payload {
    pub fn copy_header(&mut self, rhs: &Self) {
        self.head_magic = rhs.head_magic;
        self.version = rhs.version;
        self.port_id=rhs.port_id;
        self.data_type=rhs.data_type;
        self.pkt_cnt = rhs.pkt_cnt;
        self.tail_magic = rhs.tail_magic;
    }
}
