use std::cmp::min;
use std::marker::PhantomData;

use crate::buf::Buf;
use crate::buf_pool::BufPool;

#[derive(Debug)]
pub struct BufPoolVec<T, U>
where
    T: Buf<U>,
    U: std::default::Default,
{
    phantom: PhantomData<U>,
    bufs: Vec<T>,
}

impl<T, U> BufPoolVec<T, U>
where
    T: Buf<U>,
    U: std::default::Default,
{
    pub fn new(capacity: usize) -> BufPoolVec<T, U> {
        BufPoolVec {
            phantom: PhantomData,
            bufs: Vec::with_capacity(capacity),
        }
    }
}

impl<T, U> BufPool<T, U> for BufPoolVec<T, U>
where
    T: Buf<U>,
    U: std::default::Default,
{
    fn get(&mut self, bufs: &mut Vec<T>, num: usize) -> usize {
        let ready = min(num, self.bufs.len());
        let start = self.bufs.len() - ready;

        bufs.extend(self.bufs.drain(start..));

        ready
    }

    fn put(&mut self, bufs: &mut Vec<T>, num: usize) -> usize {
        let ready = min(num, bufs.len());
        let start = bufs.len() - ready;

        self.bufs.extend(bufs.drain(start..));

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

#[cfg(test)]
mod tests {
    #[derive(Default, Copy, Clone, Debug)]
    struct BufCustom {}

    #[test]
    fn buf_pool_vec_put1() {
        use crate::buf_pool::BufPool;
        use crate::buf_pool_vec::BufPoolVec;
        use crate::buf_vec::BufVec;

        const NUM: usize = 97;

        let mut bufs = Vec::with_capacity(97);
        for _ in 0..NUM {
            let buf: BufVec<BufCustom> = BufVec::new(NUM, BufCustom {});
            bufs.push(buf);
        }

        let mut bufpool = BufPoolVec::new(bufs.len());

        let len = bufs.len();
        let r = bufpool.put(&mut bufs, len);
        assert!(r == NUM);
        assert!(bufpool.len() == NUM);

        let r = bufpool.get(&mut bufs, NUM);
        assert!(r == NUM);
        assert!(bufpool.len() == 0);
        assert!(bufs.len() == NUM);
    }
}
