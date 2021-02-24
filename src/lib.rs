//! # afxdp-rs
//!
//! A Rust library for using AF_XDP (XSK) Linux sockets.
//!
//! For information on AF_XDP, see the Linux kernel [docs](https://www.kernel.org/doc/html/latest/networking/af_xdp.html).
//!
//! This implementation is focused on routing, switching and other high packet rate use cases. This implies:
//! * It must be possible to receive packets on one NIC and write them out any another (shared umem or shared MMAP area)
//! * It is appropriate to dedicate threads or cores to packet processing tasks
//! * There is a focus on low latency
//!
//! Several example programs exist in the [examples/](https://github.com/aterlo/afxdp-rs/tree/master/examples) directory.

/// Buf is the trait that is used to represent a packet buffer.
pub mod buf;
/// BufMMap is the primary implementation of [Buf](crate::buf::Buf) for use with AF_XDP sockets.
pub mod buf_mmap;
/// BufPool is a trait which represents a pool of buffers.
pub mod buf_pool;
/// BufPoolVec is an implementation of the [BufPool](crate::buf_pool::BufPool) trait based on Vec.
pub mod buf_pool_vec;
/// BufVec is an implementation of the [Buf](crate::buf::Buf) trait that is easier to use in tests because it doesn't require the
/// all of the AF_XDP infrastructure to be configured.
pub mod buf_vec;
/// MMapArea is a region of memory that is shared with one or more NICs via one or more [UMem](crate::umem::Umem)s.
pub mod mmap_area;
/// The Socket module represents an AF_XDP (XSK) socket which is used to transmit or receive packets.
pub mod socket;
/// The Umem module represents a shared memory area shared with the NIC and one or more sockets (shared Umem).
pub mod umem;
/// Utilities
mod util;

/// PENDING_LEN is the size of the circular buffer used when reading from and writing to the socket.
pub const PENDING_LEN: usize = 4096;
