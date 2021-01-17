/// The Buf trait represents a packet buffer
pub trait Buf<T> where T: std::default::Default {
    /// Returns a reference to the u8 slice of the buffer
    fn get_data(&self) -> &[u8];

    /// Returns a mutable reference to the u8 slice of the buffer
    fn get_data_mut(&mut self) -> &mut [u8];

    /// Returns the total capacity of the buffer
    fn get_capacity(&self) -> u16;

    /// Returns the length of the portion of the buffer that contains packet data
    fn get_len(&self) -> u16;

    /// Sets the length of the portion of the buffer that is contains packet data
    fn set_len(&mut self, len: u16);

    /// Returns a reference to the embedded user struct
    fn get_user(&self) -> &T;

    /// Returns a mutable reference to the embeded user struct
    fn get_user_mut(&mut self) -> &mut T;
}

/*
pub trait BufConst<T, const N: usize> where T: std::default::Default {
    fn get_data(&self) -> &[u8; N];
    fn get_data_mut(&mut self) -> &mut [u8; N];

    fn get_len(&self) -> u16;

    fn get_user(&self) -> &T;
    fn get_user_mut(&mut self) -> &mut T;
}
*/