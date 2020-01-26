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

/// Configuration options for MmapArea
#[derive(Debug)]
pub struct MmapAreaOptions {
    /// If set to true, the mmap call is passed MAP_HUGETLB
    pub huge_tlb: bool,
}

impl<'a, T: std::default::Default + std::marker::Copy> MmapArea<'a, T> {
    /// Allocate a new memory mapped area based on the size and number of buffers
    ///
    /// # Arguments
    ///
    /// * buf_num: The number of buffers to allocate in the memory mapped area
    /// * buf_len: The length of each buffer
    /// * options: Configuration options
    pub fn new(
        buf_num: usize,
        buf_len: usize,
        options: MmapAreaOptions,
    ) -> Result<(Arc<MmapArea<'a, T>>, Arc<Mutex<BufPool<'a, T>>>), MmapError> {
        let ptr: *mut c_void;
        let mut flags: c_int = MAP_PRIVATE | MAP_ANONYMOUS;

        if options.huge_tlb {
            flags = flags | MAP_HUGETLB
        }

        unsafe {
            ptr = mmap(
                0 as *mut c_void,
                buf_num * buf_len,
                PROT_READ | PROT_WRITE,
                flags,
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
    use std::convert::TryInto;

    use super::*;

    #[derive(Default, Copy, Clone, Debug)]
    struct BufCustom {}

    #[test]
    fn buf_values() {
        let options = MmapAreaOptions { huge_tlb: false };
        let r: Result<(Arc<MmapArea<BufCustom>>, Arc<Mutex<BufPool<BufCustom>>>), MmapError> =
            MmapArea::new(10, 8, options);

        let (area, buf_pool) = match r {
            Ok((area, buf_pool)) => (area, buf_pool),
            Err(err) => panic!("{:?}", err),
        };

        assert_eq!(area.buf_num, 10);
        assert_eq!(area.buf_len, 8);

        assert_eq!(buf_pool.lock().unwrap().len(), 10);

        let mut bufs = Vec::with_capacity(10);
        let r = buf_pool.lock().unwrap().get(&mut bufs, 10);
        match r {
            Ok(_) => {}
            Err(err) => panic!("error: {:?}", err),
        }

        assert_eq!(buf_pool.lock().unwrap().len(), 0);
        assert_eq!(bufs.len(), 10);

        //
        // Write a value to each buf and then ensure we read the same values out
        //
        let base: u64 = 3983989832773837873;

        for (i, buf) in bufs.iter_mut().enumerate() {
            let val = i as u64 + base;
            let bytes = val.to_ne_bytes();
            buf.data[0] = bytes[0];
            buf.data[1] = bytes[1];
            buf.data[2] = bytes[2];
            buf.data[3] = bytes[3];
            buf.data[4] = bytes[4];
            buf.data[5] = bytes[5];
            buf.data[6] = bytes[6];
            buf.data[7] = bytes[7];
        }

        for (i, buf) in bufs.iter_mut().enumerate() {
            let val: u64 = i as u64 + base;

            let (int_bytes, _rest) = buf.data.split_at(std::mem::size_of::<u64>());
            let val2 = u64::from_ne_bytes(int_bytes.try_into().unwrap());

            assert_eq!(val, val2);
        }
    }
}
