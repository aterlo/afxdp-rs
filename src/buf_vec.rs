use std::{convert::TryFrom, u16};

use crate::buf::Buf;

#[derive(Debug, Default)]
pub struct BufVec<T>
where
    T: std::default::Default,
{
    pub(crate) data: Vec<u8>,
    pub(crate) len: u16,
    pub(crate) user: T,
}

impl<T> BufVec<T>
where
    T: std::default::Default,
{
    pub fn new(capacity: usize, user: T) -> BufVec<T> {
        BufVec {
            data: vec![0; capacity],
            len: 0,
            user,
        }
    }
}

impl<T> Buf<T> for BufVec<T>
where
    T: std::default::Default,
{
    fn get_data(&self) -> &[u8] {
        &self.data[0..]
    }

    fn get_data_mut(&mut self) -> &mut [u8] {
        println!("len {}", self.data[0..].len());
        &mut self.data[0..]
    }

    fn get_capacity(&self) -> u16 {
        u16::try_from(self.data.capacity()).unwrap()
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
