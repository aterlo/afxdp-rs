use std::{cmp::min, u64};

use libbpf_sys::{
    _xsk_ring_cons__comp_addr, _xsk_ring_cons__peek, _xsk_ring_cons__release,
    _xsk_ring_prod__fill_addr, _xsk_ring_prod__needs_wakeup, _xsk_ring_prod__reserve,
    _xsk_ring_prod__submit, xsk_ring_cons, xsk_ring_prod, xsk_umem, xsk_umem__create,
    xsk_umem__delete, xsk_umem_config, XSK_UMEM__DEFAULT_FRAME_HEADROOM,
};
use std::sync::{Arc, Mutex};

use crate::buf_mmap::BufMmap;
use crate::mmap_area::MmapArea;

/// AF_XDP Umem
#[derive(Debug)]
pub struct Umem<'a, T: std::default::Default + std::marker::Copy> {
    pub(crate) area: Arc<MmapArea<'a, T>>,
    pub(crate) umem: Mutex<Box<xsk_umem>>, // TODO - Rethink need for Mutex here
}
// TODO - Document why this is safe
unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for Umem<'a, T> {}

/// Completion queue per Umem
#[derive(Debug)]
pub struct UmemCompletionQueue<'a, T: std::default::Default + std::marker::Copy> {
    umem: Arc<Umem<'a, T>>,
    cq: Box<xsk_ring_cons>,
}
unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for UmemCompletionQueue<'a, T> {}

/// Fill queue per Umem
#[derive(Debug)]
pub struct UmemFillQueue<'a, T: std::default::Default + std::marker::Copy> {
    umem: Arc<Umem<'a, T>>,
    fq: Box<xsk_ring_prod>,
}
unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for UmemFillQueue<'a, T> {}

#[derive(Debug)]
pub enum UmemError {
    Failed,
}

impl<'a, T: std::default::Default + std::marker::Copy> Umem<'a, T> {
    /// Create a new Umem using the passed memory mapped area
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
        UmemError,
    > {
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
        unsafe {
            let size = (area.get_buf_num() * area.get_buf_len()) as u64;
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
            return Err(UmemError::Failed);
        }

        let arc = Arc::new(Umem {
            area,
            umem: Mutex::new(unsafe { Box::from_raw(*umem_ptr) }),
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
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for Umem<'a, T> {
    fn drop(&mut self) {
        // TODO Does this need to test for null too?
        unsafe {
            xsk_umem__delete(self.umem.lock().unwrap().as_mut());
        }
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> UmemCompletionQueue<'a, T> {
    /// After packets have been transmitted, the buffer is returned via the completion queue. The service
    /// method processed the completion queue.
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

        let buf_len = self.umem.area.get_buf_len();

        for _ in 0..ready {
            let buf: BufMmap<T>;

            unsafe {
                let addr = _xsk_ring_cons__comp_addr(self.cq.as_mut(), idx);
                let ptr = self.umem.area.get_ptr().offset(*addr as isize);
                idx += 1;

                buf = BufMmap {
                    addr: *addr,
                    len: 0,
                    data: std::slice::from_raw_parts_mut(ptr as *mut u8, buf_len as usize),
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
