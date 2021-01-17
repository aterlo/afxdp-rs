use std::cmp::min;
use std::marker::PhantomData;

use crate::buf::Buf;
use crate::buf_pool::BufPool;

#[derive(Debug)]
pub struct BufPoolVec<T, U> where T: Buf<U>, U: std::default::Default {
    phantom: PhantomData<U>,
    bufs: Vec<T>,
}

impl<T, U> BufPoolVec<T, U> where T: Buf<U>, U: std::default::Default {
    pub fn new(capacity: usize) -> BufPoolVec<T, U> {
        BufPoolVec {
            phantom: PhantomData,
            bufs: Vec::with_capacity(capacity),
        }
    }
}

impl<T, U> BufPool<T, U> for BufPoolVec<T,U > where T: Buf<U>, U: std::default::Default {
    fn get(&mut self, bufs: &mut Vec<T>, num: usize) -> usize {
        let ready = min(num, self.bufs.len());

        for _ in 0..ready {
            let r = self.bufs.pop();
            match r {
                Some(buf) => {
                    bufs.push(buf);
                }
                None => panic!("This should not happen"),
            }
        }

        ready
    }

    fn put(&mut self, bufs: &mut Vec<T>, num: usize) -> usize {
        let ready = min(num, bufs.len());

        for _ in 0..ready {
            let r = bufs.pop();
            match r {
                Some(buf) => {
                    self.bufs.push(buf);
                }
                None => panic!("This should not happen"),
            }
        }

        ready
    }

    fn put_buf(&mut self, buf: T) -> usize {
        self.bufs.push(buf);

        1
    }

    fn len(&self) -> usize {
        self.bufs.len()
    }

    fn is_empty(&self) -> bool {
        self.bufs.is_empty()
    }
}
