use std::convert::TryFrom;

use crate::buf::Buf;

#[derive(Debug)]
pub struct BufMmap<'a, T> where T: std::default::Default {
    pub(crate) addr: u64, // TODO: Does this need to be a u64?, this should be called index not addr.
    pub(crate) len: u16,
    pub(crate) data: &'a mut [u8],
    pub(crate) user: T,
}

impl<T> Buf<T> for BufMmap<'_, T> where T: std::default::Default {
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

impl<'a, T> Drop for BufMmap<'a, T> where T: std::default::Default {
    fn drop(&mut self) {
        //todo!("bug");
    }
}

/*
#[derive(Debug)]
pub struct BufMmapConst<'a, T, const N: u16> where T: std::default::Default {
    pub addr: u64, // TODO: Does this need to be a u64? Pretty sure not.
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