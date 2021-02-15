use std::convert::TryFrom;
use std::fmt;

use crate::buf::Buf;

#[derive(Debug)]
pub struct BufMmap<'a, T>
where
    T: std::default::Default,
{
    /// addr is is the address in the umem area (offset into area)
    pub(crate) addr: u64,
    /// len is the length of the buffer that is valid packet data
    pub(crate) len: u16,
    /// data is the slice of u8 that contains the packet data
    pub(crate) data: &'a mut [u8],
    /// user is the user defined type
    pub(crate) user: T,
}

impl<T> Buf<T> for BufMmap<'_, T>
where
    T: std::default::Default,
{
    fn get_data(&self) -> &[u8] {
        &self.data[0..]
    }

    fn get_data_mut(&mut self) -> &mut [u8] {
        &mut self.data[0..]
    }

    fn get_capacity(&self) -> u16 {
        u16::try_from(self.data.len()).unwrap()
    }

    fn get_len(&self) -> u16 {
        self.len
    }

    fn set_len(&mut self, len: u16) {
        self.len = len;
    }

    fn get_user(&self) -> &T {
        &self.user
    }

    fn get_user_mut(&mut self) -> &mut T {
        &mut self.user
    }
}

impl<'a, T> fmt::Display for BufMmap<'a, T>
where
    T: std::default::Default,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "BufMMap addr={} len={} data={:?}",
            self.addr,
            self.len,
            &(self.data[0]) as *const u8
        )
    }
}

impl<'a, T> Drop for BufMmap<'a, T>
where
    T: std::default::Default,
{
    fn drop(&mut self) {}
}

/*
#[derive(Debug)]
pub struct BufMmapConst<'a, T, const N: u16> where T: std::default::Default {
    pub addr: u64,
    pub data: &'a mut [u8],
    pub user: T,
}

impl<T, const N: u16> Buf<T> for BufMmapConst<'_, T, N> where T: std::default::Default {
    fn get_data(&self) -> &[u8] {
        &self.data[0..]
    }

    fn get_data_mut(&mut self) -> &mut [u8] {
        &mut self.data[0..]
    }

    fn get_len(&self) -> u16 {
        N
    }

    fn get_user(&self) -> &T {
        &self.user
    }

    fn get_user_mut(&mut self) -> &mut T {
        &mut self.user
    }
}

impl<'a, T, const N: u16> Drop for BufMmapConst<'a, T, N> where T: std::default::Default {
    fn drop(&mut self) {
        //todo!("bug");
    }
}
*/
