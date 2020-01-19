/// Internal buffer used to hold packets. It can be extended with T.
pub struct Buf<'a, T> {
    pub addr: u64, // TODO: Does this need to be a u64?
    pub len: u32,
    pub data: &'a mut [u8],
    pub custom: T,
}

impl<'a, T> Drop for Buf<'a, T> {
    fn drop(&mut self) {
        //todo!("bug");
    }
}
