use std::cmp::min;
use std::sync::{Arc, Mutex};

use crate::buf::Buf;
use crate::mmaparea::MmapArea;

/// BufPool stores Bufs. It can be used in a single thread or wrapped in a Mutex to provide a buffer pool to multiple
/// threads.
pub struct BufPool<'a, T: std::default::Default + std::marker::Copy> {
    _area: Arc<MmapArea<'a, T>>,
    bufs: Vec<Buf<'a, T>>,
}

unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for BufPool<'a, T> {}

#[derive(Debug)]
pub enum BufPoolError {
    Failed,
}

impl<'a, T: std::default::Default + std::marker::Copy> BufPool<'a, T> {
    pub fn new(
        area: Arc<MmapArea<'a, T>>,
        buf_num: usize,
        buf_len: usize,
    ) -> Result<Arc<Mutex<BufPool<'a, T>>>, BufPoolError> {
        let mut bufs: Vec<Buf<T>> = Vec::with_capacity(buf_num);

        for i in 0..buf_num {
            let buf: Buf<T>;
            unsafe {
                let ptr = area.ptr.offset((i * buf_len) as isize);
                buf = Buf::<T> {
                    addr: i as u64,
                    len: buf_len as u32,
                    data: std::slice::from_raw_parts_mut(ptr as *mut u8, buf_len),
                    custom: Default::default(),
                };
            }

            bufs.push(buf);
        }

        let bp = BufPool {
            _area: area,
            bufs: bufs,
        };

        Ok(Arc::new(Mutex::new(bp)))
    }

    #[inline]
    pub fn get(&mut self, bufs: &mut Vec<Buf<'a, T>>, num: usize) -> Result<usize, BufPoolError> {
        let ready = min(num, self.bufs.len());

        // TODO: This probably isn't the fastest way to do this. Doable w/ one memcpy?
        for _ in 0..ready {
            let r = self.bufs.pop();
            match r {
                Some(buf) => {
                    bufs.push(buf);
                }
                None => panic!("shouldn't happen"),
            }
        }

        Ok(ready)
    }

    #[inline]
    pub fn put(&mut self, bufs: &mut Vec<Buf<'a, T>>, num: usize) -> Result<usize, BufPoolError> {
        let ready = min(num, bufs.len());

        // TODO: This probably isn't the fastest way to do this. Doable w/ one memcpy?
        for _ in 0..ready {
            let r = bufs.pop();
            match r {
                Some(buf) => {
                    self.bufs.push(buf);
                }
                None => panic!("shouldn't happen"),
            }
        }

        Ok(ready)
    }

    pub fn len(&self) -> usize {
        self.bufs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bufs.is_empty()
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for BufPool<'a, T> {
    fn drop(&mut self) {}
}
