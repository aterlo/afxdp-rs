use crate::buf::Buf;

pub trait BufPool<T, U> where T: Buf<U>, U: std::default::Default {
    fn get(&mut self, bufs: &mut Vec<T>, num: usize) -> usize;
    fn put(&mut self, bufs: &mut Vec<T>, num: usize) -> usize;
    fn put_buf(&mut self, buf: T) -> usize;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool;
}