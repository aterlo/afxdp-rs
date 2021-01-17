pub mod buf;
pub mod buf_mmap;
pub mod buf_vec;
pub mod buf_pool;
pub mod buf_pool_vec;
pub mod mmap_area;
pub mod umem;
pub mod socket;

pub const PENDING_LEN: usize = 4096; // TODO: Experiment with size