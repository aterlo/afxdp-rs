use std::marker::PhantomData;
use std::sync::{Arc};
use std::convert::TryInto;

use errno::errno;
use libc::{
    c_int, c_void, mmap, munmap, MAP_ANONYMOUS, MAP_HUGETLB, MAP_PRIVATE, PROT_READ, PROT_WRITE,
};

use crate::buf_mmap::BufMmap;

/// A mapped memory area used to move packets between the kernel and userspace.
#[derive(Debug)]
pub struct MmapArea<'a, T: std::default::Default + std::marker::Copy> {
    phantom: PhantomData<&'a T>,
    pub(crate) buf_num: usize,
    pub(crate) buf_len: usize,
    pub(crate) ptr: *mut c_void,
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
    ) -> Result<(Arc<MmapArea<'a, T>>, Vec<BufMmap<'a, T>>), MmapError> {
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
            buf_num,
            buf_len,
            ptr,
            phantom: PhantomData,
        });

        // Create the bufs
        let mut bufs = Vec::with_capacity(buf_num);

        for i in 0..buf_num {
            let buf: BufMmap<T>;
            unsafe {
                let ptr = ma.ptr.offset((i * buf_len) as isize);
                buf = BufMmap::<T> {
                    addr: i as u64,
                    len: buf_len.try_into().unwrap(), // TODO: Should this start at 0?
                    data: std::slice::from_raw_parts_mut(ptr as *mut u8, buf_len),
                    user: Default::default(),
                };
            }

            bufs.push(buf);
        }

        Ok((ma, bufs))
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
    use std::sync::Arc;
    use std::convert::TryInto;

    use super::{MmapArea, MmapAreaOptions, MmapError};
    use crate::buf_pool::BufPool;
    use crate::buf_mmap::BufMmap;
    use crate::buf_pool_vec::BufPoolVec;

    #[derive(Default, Copy, Clone, Debug)]
    struct BufCustom {}

    #[test]
    fn bufs_to_pool() {
        const BUF_NUM: usize = 10;

        let options = MmapAreaOptions { huge_tlb: false };
        let r: Result<(Arc<MmapArea<BufCustom>>, Vec<BufMmap<BufCustom>>), MmapError> =
            MmapArea::new(BUF_NUM, 8, options);

        let (area, mut bufs) = match r {
            Ok((area, bufs)) => (area, bufs),
            Err(err) => panic!("{:?}", err),
        };

        assert_eq!(area.buf_num, BUF_NUM);
        assert_eq!(area.buf_len, 8);
        assert_eq!(bufs.len(), BUF_NUM);

        let mut pool = BufPoolVec::new(BUF_NUM);

        let r = pool.put(&mut bufs, area.buf_num);
        assert_eq!(r, BUF_NUM);
        assert_eq!(pool.len(), BUF_NUM);
    }

    #[test]
    fn buf_values() {
        const BUF_NUM: usize = 88;

        let options = MmapAreaOptions { huge_tlb: false };
        let r: Result<(Arc<MmapArea<BufCustom>>, Vec<BufMmap<BufCustom>>), MmapError> =
            MmapArea::new(BUF_NUM, 8, options);

        let (area, mut bufs) = match r {
            Ok((area, buf_pool)) => (area, buf_pool),
            Err(err) => panic!("{:?}", err),
        };

        assert_eq!(area.buf_num, BUF_NUM);
        assert_eq!(area.buf_len, 8);
        assert_eq!(bufs.len(), BUF_NUM);

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
