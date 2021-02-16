use crate::buf::Buf;

/// BufPool represents a pool of [Buf](crate::buf::Buf)s.
pub trait BufPool<T, U>
where
    T: Buf<U>,
    U: std::default::Default,
{
    /// Get up to `num` bufs from the pool.
    fn get(&mut self, bufs: &mut Vec<T>, num: usize) -> usize;

    /// Add up to `num` bufs to the pool.
    fn put(&mut self, bufs: &mut Vec<T>, num: usize) -> usize;

    /// Add a single `buf` to the pool.
    fn put_buf(&mut self, buf: T) -> usize;

    /// Get the number of bufs in the pool.
    fn len(&self) -> usize;

    /// Indicate whether or not the pool is empty.
    fn is_empty(&self) -> bool;
}
