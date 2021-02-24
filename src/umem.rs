use std::sync::Arc;
use std::{cmp::min, u64};

use thiserror::Error;

use libbpf_sys::{
    _xsk_ring_cons__comp_addr, _xsk_ring_cons__peek, _xsk_ring_cons__release,
    _xsk_ring_prod__fill_addr, _xsk_ring_prod__needs_wakeup, _xsk_ring_prod__reserve,
    _xsk_ring_prod__submit, xsk_ring_cons, xsk_ring_prod, xsk_umem, xsk_umem__create,
    xsk_umem__delete, xsk_umem_config, XSK_UMEM__DEFAULT_FRAME_HEADROOM,
};

use crate::buf_mmap::BufMmap;
use crate::mmap_area::MmapArea;
use crate::util;
use crate::AF_XDP_RESERVED;

/// The Umem is a shared region of memory, backed by MMap, that is shared between userspace and the NIC(s).
/// Bufs (descriptors) represent packets stored in this area.
#[derive(Debug)]
pub struct Umem<'a, T: std::default::Default + std::marker::Copy> {
    pub(crate) area: Arc<MmapArea<'a, T>>,
    pub(crate) umem: Box<xsk_umem>,
}

/// The completion queue is used by the kernel to signal to the application using AF_XDP that the buffer has been
/// transmitted and is free to be used again.
#[derive(Debug)]
pub struct UmemCompletionQueue<'a, T: std::default::Default + std::marker::Copy> {
    umem: Arc<Umem<'a, T>>,
    cq: Box<xsk_ring_cons>,
}

/// The fill queue is used to provide the AF_XDP socket (kernel) with buffers where it can write incoming packets.
#[derive(Debug)]
pub struct UmemFillQueue<'a, T: std::default::Default + std::marker::Copy> {
    umem: Arc<Umem<'a, T>>,
    fq: Box<xsk_ring_prod>,
}

#[derive(Debug, Error)]
pub enum UmemNewError {
    #[error("umem create failed: {0}")]
    Create(std::io::Error),
    #[error("umem ring size not a power of two")]
    RingNotPowerOfTwo,
}

#[derive(Debug, Error)]
pub enum UmemError {
    #[error("failed")]
    Failed,
}

impl<'a, T: std::default::Default + std::marker::Copy> Umem<'a, T> {
    /// Create a new Umem using the passed memory mapped area.
    pub fn new(
        area: Arc<MmapArea<'a, T>>,
        completion_ring_size: u32,
        fill_ring_size: u32,
    ) -> Result<
        (
            Arc<Umem<'a, T>>,
            UmemCompletionQueue<'a, T>,
            UmemFillQueue<'a, T>,
        ),
        UmemNewError,
    > {
        // Verify that the passed ring sizes are a power of two.
        // https://www.kernel.org/doc/html/latest/networking/af_xdp.html
        if !util::is_pow_of_two(completion_ring_size) || !util::is_pow_of_two(fill_ring_size) {
            return Err(UmemNewError::RingNotPowerOfTwo);
        }

        let cfg = xsk_umem_config {
            fill_size: fill_ring_size,
            comp_size: completion_ring_size,
            frame_size: area.get_buf_len() as u32,
            frame_headroom: XSK_UMEM__DEFAULT_FRAME_HEADROOM,
            flags: 0,
        };

        /*
        println!(
            "umem config: fill_size={} comp_size={} frame_size={} frame_headroom={} flags={}",
            cfg.fill_size, cfg.comp_size, cfg.frame_size, cfg.frame_headroom, cfg.flags
        );
        */

        // Allocate the rings on the heap since we pass these pointers to xsk_umem_create()
        let mut cq: Box<xsk_ring_cons> = Default::default();
        let mut fq: Box<xsk_ring_prod> = Default::default();

        // Double indirection in C function
        let mut umem: *mut xsk_umem = std::ptr::null_mut();
        let umem_ptr: *mut *mut xsk_umem = &mut umem;

        let ret: std::os::raw::c_int;
        let size = (area.get_buf_num() * area.get_buf_len()) as u64;

        unsafe {
            ret = xsk_umem__create(
                umem_ptr,
                area.get_ptr(),
                size,
                fq.as_mut(),
                cq.as_mut(),
                &cfg,
            );
        }

        if ret != 0 {
            return Err(UmemNewError::Create(std::io::Error::from_raw_os_error(ret)));
        }

        let arc = Arc::new(Umem {
            area,
            umem: unsafe { Box::from_raw(*umem_ptr) },
        });

        let cq = UmemCompletionQueue {
            umem: arc.clone(),
            cq,
        };
        let fq = UmemFillQueue {
            umem: arc.clone(),
            fq,
        };

        Ok((arc, cq, fq))
    }

    pub fn get_ptr(&self) -> *const xsk_umem {
        self.umem.as_ref() as *const xsk_umem
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for Umem<'a, T> {
    fn drop(&mut self) {
        // TODO Does this need to test for null too?
        unsafe {
            xsk_umem__delete(self.umem.as_mut());
        }
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> UmemCompletionQueue<'a, T> {
    /// After packets have been transmitted, the buffer is returned via the completion queue. The service
    /// method processes the completion queue.
    #[inline]
    pub fn service(
        &mut self,
        bufs: &mut Vec<BufMmap<T>>,
        batch_size: usize,
    ) -> Result<usize, UmemError> {
        let mut idx: u32 = 0;
        let ready: usize;
        let batch_size = min(bufs.capacity() - bufs.len(), batch_size);

        unsafe {
            ready = _xsk_ring_cons__peek(self.cq.as_mut(), batch_size as u64, &mut idx) as usize;
        }
        if ready == 0 {
            return Ok(0);
        }

        let buf_len_available = self.umem.area.get_buf_len() - AF_XDP_RESERVED as usize;

        for _ in 0..ready {
            let buf: BufMmap<T>;

            unsafe {
                let addr = _xsk_ring_cons__comp_addr(self.cq.as_mut(), idx);
                let ptr = self.umem.area.get_ptr().offset(*addr as isize);
                idx += 1;

                buf = BufMmap {
                    addr: *addr + AF_XDP_RESERVED,
                    len: 0,
                    data: std::slice::from_raw_parts_mut(ptr as *mut u8, buf_len_available),
                    user: Default::default(),
                };
            }

            bufs.push(buf);
        }

        unsafe {
            _xsk_ring_cons__release(self.cq.as_mut(), ready as u64);
        }

        Ok(ready)
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for UmemCompletionQueue<'a, T> {
    fn drop(&mut self) {}
}

impl<'a, T: std::default::Default + std::marker::Copy> UmemFillQueue<'a, T> {
    /// In order to receive packets, the link needs buffers to write the packets to. These buffers are sent from
    /// userspace to the kernel via the fill queue.
    #[inline]
    pub fn fill(
        &mut self,
        bufs: &mut Vec<BufMmap<T>>,
        mut batch_size: usize,
    ) -> Result<usize, UmemError> {
        let mut idx: u32 = 0;
        let ready: usize;

        batch_size = min(bufs.len(), batch_size);

        unsafe {
            ready = _xsk_ring_prod__reserve(self.fq.as_mut(), batch_size as u64, &mut idx) as usize;
        }

        if ready > 0 {
            for _ in 0..ready {
                let b = bufs.pop();
                match b {
                    Some(b) => unsafe {
                        let ptr = _xsk_ring_prod__fill_addr(self.fq.as_mut(), idx);
                        idx += 1;

                        *ptr = (b.addr as u64).into();
                    },
                    None => {
                        todo!("tried to get buffer from empty pool");
                    }
                }
            }
        }

        unsafe {
            _xsk_ring_prod__submit(self.fq.as_mut(), ready as u64);
        }

        Ok(ready)
    }

    #[inline]
    pub fn needs_wakeup(&mut self) -> bool {
        unsafe {
            if _xsk_ring_prod__needs_wakeup(self.fq.as_mut()) != 0 {
                return true;
            }

            false
        }
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for UmemFillQueue<'a, T> {
    fn drop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::buf_mmap::BufMmap;
    use crate::mmap_area::{MmapArea, MmapAreaOptions, MmapError};
    use crate::umem::{Umem, UmemNewError};

    #[derive(Default, Copy, Clone, Debug)]
    struct BufCustom {}

    // Test with neither ring size as a power of two
    #[test]
    fn ring_size1() {
        const BUF_NUM: usize = 1024;
        const BUF_LEN: usize = 2048;

        let options = MmapAreaOptions { huge_tlb: false };
        let r: Result<(Arc<MmapArea<BufCustom>>, Vec<BufMmap<BufCustom>>), MmapError> =
            MmapArea::new(BUF_NUM, BUF_LEN, options);

        let (area, _) = match r {
            Ok((area, buf_pool)) => (area, buf_pool),
            Err(err) => panic!("{:?}", err),
        };

        let r = Umem::new(area, 97, 997);
        match r {
            Err(UmemNewError::RingNotPowerOfTwo) => {
                // Expected
            }
            Err(err) => {
                panic!("wrong error: {}", err);
            }
            Ok(_) => {
                panic!("no error");
            }
        }
    }

    // Test with the first ring size not power of two and second as power of two
    #[test]
    fn ring_size2() {
        const BUF_NUM: usize = 1024;
        const BUF_LEN: usize = 2048;

        let options = MmapAreaOptions { huge_tlb: false };
        let r: Result<(Arc<MmapArea<BufCustom>>, Vec<BufMmap<BufCustom>>), MmapError> =
            MmapArea::new(BUF_NUM, BUF_LEN, options);

        let (area, _) = match r {
            Ok((area, buf_pool)) => (area, buf_pool),
            Err(err) => panic!("{:?}", err),
        };

        let r = Umem::new(area, 100, 1024);
        match r {
            Err(UmemNewError::RingNotPowerOfTwo) => {
                // Expected
            }
            Err(err) => {
                panic!("wrong error: {}", err);
            }
            Ok(_) => {
                panic!("no error");
            }
        }
    }

    // Test with the first ring size as a power of two and the second not
    #[test]
    fn ring_size3() {
        const BUF_NUM: usize = 1024;
        const BUF_LEN: usize = 2048;

        let options = MmapAreaOptions { huge_tlb: false };
        let r: Result<(Arc<MmapArea<BufCustom>>, Vec<BufMmap<BufCustom>>), MmapError> =
            MmapArea::new(BUF_NUM, BUF_LEN, options);

        let (area, _) = match r {
            Ok((area, buf_pool)) => (area, buf_pool),
            Err(err) => panic!("{:?}", err),
        };

        let r = Umem::new(area, 1024, 100);
        match r {
            Err(UmemNewError::RingNotPowerOfTwo) => {
                // Expected
            }
            Err(err) => {
                panic!("wrong error: {}", err);
            }
            Ok(_) => {
                panic!("no error");
            }
        }
    }

    // Test with both ring sizes as a power of two
    // Note that Umem and AF_XDP in general required the locked memory to be increased to work
    #[test]
    fn ring_size4() {
        use rlimit::{setrlimit, Resource, Rlim};
        use std::io;
        use std::io::Write;

        let r = setrlimit(Resource::MEMLOCK, Rlim::INFINITY, Rlim::INFINITY);
        match r {
            Err(_) => {
                writeln!(
                    &mut io::stdout(),
                    "Test skipped as it needs to be run as root"
                )
                .unwrap();
                return;
            }
            Ok(_) => {
                // Expected
            }
        }

        const BUF_NUM: usize = 1024;
        const BUF_LEN: usize = 2048;

        let options = MmapAreaOptions { huge_tlb: false };
        let r: Result<(Arc<MmapArea<BufCustom>>, Vec<BufMmap<BufCustom>>), MmapError> =
            MmapArea::new(BUF_NUM, BUF_LEN, options);

        let (area, _) = match r {
            Ok((area, buf_pool)) => (area, buf_pool),
            Err(err) => panic!("{:?}", err),
        };

        let r = Umem::new(area, 1024, 1024);
        match r {
            Err(err) => {
                panic!("error: {}", err);
            }
            Ok(_) => {
                // Expected
            }
        }
    }
}
