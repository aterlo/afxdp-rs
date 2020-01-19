use errno::errno;
use libc::{
    c_int, c_void, mmap, munmap, MAP_ANONYMOUS, MAP_HUGETLB, MAP_PRIVATE, PROT_READ, PROT_WRITE,
};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use crate::bufpool::{BufPool, BufPoolError};

/// A mapped memory area used to move packets between the kernel and userspace.
#[derive(Debug)]
pub struct MmapArea<'a, T: std::default::Default + std::marker::Copy> {
    phantom: PhantomData<&'a T>,
    pub buf_num: usize,
    pub buf_len: usize,
    pub ptr: *mut c_void, // TODO: Can this be be private. Only MmapArea should be able to create Bufs.
}
unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for MmapArea<'a, T> {}

#[derive(Debug)]
pub enum MmapError {
    Failed,
}

impl<'a, T: std::default::Default + std::marker::Copy> MmapArea<'a, T> {
    /// Allocate a new memory mapped area based on the size and number of buffers
    ///
    /// # Arguments
    ///
    /// * buf_num: The number of buffers to allocate in the memory mapped area
    /// * buf_len: The length of each buffer
    pub fn new(
        buf_num: usize,
        buf_len: usize,
    ) -> Result<(Arc<MmapArea<'a, T>>, Arc<Mutex<BufPool<'a, T>>>), MmapError> {
        let ptr: *mut c_void;

        unsafe {
            ptr = mmap(
                0 as *mut c_void,
                buf_num * buf_len,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB,
                -1,
                0,
            );
        }

        // TODO: Is this the correct way to check for NULL c_void?
        if ptr == std::ptr::null_mut() {
            return Err(MmapError::Failed);
        }

        let ma = Arc::new(MmapArea {
            buf_num: buf_num,
            buf_len: buf_len,
            ptr: ptr,
            phantom: PhantomData,
        });

        let r: Result<Arc<Mutex<BufPool<T>>>, BufPoolError> =
            BufPool::new(ma.clone(), buf_num, buf_len);
        if let Ok(bp) = r {
            Ok((ma, bp))
        } else {
            Err(MmapError::Failed)
        }
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for MmapArea<'a, T> {
    fn drop(&mut self) {
        let r: c_int;

        unsafe {
            r = munmap(self.ptr, self.buf_num * self.buf_len);
        }

        if r != 0 {
            let errno = errno().0;
            println!("munmap failed errno: {}", errno);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default, Copy, Clone)]
    struct BufCustom {}

    #[test]
    fn small() {
        let r: Result<(Arc<MmapArea<BufCustom>>, Arc<Mutex<BufPool<BufCustom>>>), MmapError> =
            MmapArea::new(10, 10);

        match r {
            Ok((area, buf_pool)) => {
                assert_eq!(area.buf_num, 10);
                assert_eq!(area.buf_len, 10);

                assert_eq!(buf_pool.lock().unwrap().len(), 10);
            }
            Err(err) => panic!("{:?}", err),
        }
    }
}
