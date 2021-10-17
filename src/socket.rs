use std::cmp::min;
use std::convert::TryInto;
use std::ffi::CString;
use std::sync::Arc;

use arraydeque::{ArrayDeque, Wrapping};
use errno::errno;
use libbpf_sys::{
    _xsk_ring_cons__peek, _xsk_ring_cons__release, _xsk_ring_cons__rx_desc,
    _xsk_ring_prod__needs_wakeup, _xsk_ring_prod__reserve, _xsk_ring_prod__submit,
    _xsk_ring_prod__tx_desc, xdp_desc, xsk_ring_cons, xsk_ring_prod, xsk_socket,
    xsk_socket__create, xsk_socket__delete, xsk_socket__fd, xsk_socket_config, xsk_umem, XDP_COPY,
    XDP_FLAGS_UPDATE_IF_NOEXIST, XDP_USE_NEED_WAKEUP, XDP_ZEROCOPY,
};
use libc::{poll, pollfd, sendto, EAGAIN, EBUSY, ENETDOWN, ENOBUFS, MSG_DONTWAIT, POLLIN};
use thiserror::Error;

use crate::umem::Umem;
use crate::util;
use crate::PENDING_LEN;
use crate::{buf_mmap::BufMmap, AF_XDP_RESERVED};

const POLL_TIMEOUT: i32 = 0; // Busy polling

/// A Rx and Tx AF_XDP socket which is used to receive and transmit packets via AF_XDP.
#[derive(Debug)]
pub struct Socket<'a, T: std::default::Default + std::marker::Copy> {
    umem: Arc<Umem<'a, T>>,
    socket: Box<xsk_socket>,
    if_name_c: CString,
}

/// A Rx only AF_XDP socket
#[derive(Debug)]
pub struct SocketRx<'a, T: std::default::Default + std::marker::Copy> {
    socket: Arc<Socket<'a, T>>,
    fd: std::os::raw::c_int,
    rx: Box<xsk_ring_cons>,
}
// SocketRx is not Send by default because of the *mut u32 in xsk_ring_cons. According to the Rustonomicon,
// raw pointers are not Send/Sync as a 'lint'. I believe it is safe to mark MmapArea as Sync in this context.
// https://doc.rust-lang.org/nomicon/send-and-sync.html
unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for SocketRx<'a, T> {}

/// A Tx only AF_XDP socket
#[derive(Debug)]
pub struct SocketTx<'a, T: std::default::Default + std::marker::Copy> {
    socket: Arc<Socket<'a, T>>,
    fd: std::os::raw::c_int,
    tx: Box<xsk_ring_prod>,
}
// SocketTx is not Send by default because of the *mut u32 in xsk_ring_prod. According to the Rustonomicon,
// raw pointers are not Send/Sync as a 'lint'. I believe it is safe to mark MmapArea as Sync in this context.
// https://doc.rust-lang.org/nomicon/send-and-sync.html
unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for SocketTx<'a, T> {}

#[derive(Debug, Error)]
pub enum SocketNewError {
    #[error("socket create failed: {0}")]
    Create(std::io::Error),
    #[error("socket ring size not a power of two")]
    RingNotPowerOfTwo,
}

#[derive(Debug)]
pub enum SocketError {
    Failed,
}

/// Configuration options for AF_XDP sockets
#[derive(Copy, Clone, Debug, Default)]
pub struct SocketOptions {
    /// Force XDP zero copy mode (XDP_ZEROCOPY flag)
    pub zero_copy_mode: bool,

    /// Force XDP copy mode (XDP_COPY flag)
    pub copy_mode: bool,

    /// Tx ring size (must be a power of two)
    pub tx_ring_size: u32,

    /// Rx ring size (must be a power of two)
    pub rx_ring_size: u32,
}

impl<'a, T: std::default::Default + std::marker::Copy> Socket<'a, T> {
    /// Create a new Rx/Tx AF_XDP socket
    ///
    /// # Arguments
    /// * umem: The Umem the new socket will use
    /// * queue: The NIC queue/channel this socket will bind to
    /// * tx_ring_size: The size of the Tx ring
    /// * rx_ring_size: The size of the Rx ring
    /// * options: Configuration options
    pub fn new(
        umem: Arc<Umem<'a, T>>,
        if_name: &str,
        queue: usize,
        rx_ring_size: u32,
        tx_ring_size: u32,
        options: SocketOptions,
    ) -> Result<(Arc<Socket<'a, T>>, SocketRx<'a, T>, SocketTx<'a, T>), SocketNewError> {
        // Verify that the passed ring sizes are a power of two.
        // https://www.kernel.org/doc/html/latest/networking/af_xdp.html
        if !util::is_pow_of_two(rx_ring_size) || !util::is_pow_of_two(tx_ring_size) {
            return Err(SocketNewError::RingNotPowerOfTwo);
        }

        let mut cfg = xsk_socket_config {
            rx_size: rx_ring_size,
            tx_size: tx_ring_size,
            xdp_flags: XDP_FLAGS_UPDATE_IF_NOEXIST,
            bind_flags: XDP_USE_NEED_WAKEUP as u16,
            libbpf_flags: 0,
        };

        if options.zero_copy_mode {
            cfg.bind_flags |= XDP_ZEROCOPY as u16;
        }

        if options.copy_mode {
            cfg.bind_flags |= XDP_COPY as u16;
        }

        // Heap allocate since they are passed to the C function
        let mut rx: Box<xsk_ring_cons> = Default::default();
        let mut tx: Box<xsk_ring_prod> = Default::default();

        // C function has double indirection
        let mut xsk: *mut xsk_socket = std::ptr::null_mut();
        let xsk_ptr: *mut *mut xsk_socket = &mut xsk;

        let if_name_c = CString::new(if_name).unwrap();

        let ret: std::os::raw::c_int;
        unsafe {
            ret = xsk_socket__create(
                xsk_ptr,
                if_name_c.as_ptr(),
                queue as u32,
                umem.get_ptr() as *mut xsk_umem,
                rx.as_mut(),
                tx.as_mut(),
                &cfg,
            );
        }

        if ret != 0 {
            let errno = errno().0;
            return Err(SocketNewError::Create(std::io::Error::from_raw_os_error(
                errno,
            )));
        }

        let arc = Arc::new(Socket {
            umem,
            socket: unsafe { Box::from_raw(*xsk_ptr) },
            if_name_c,
        });

        let rx = SocketRx {
            socket: arc.clone(),
            fd: unsafe { xsk_socket__fd(*xsk_ptr) },
            rx,
        };
        let tx = SocketTx {
            socket: arc.clone(),
            tx,
            fd: unsafe { xsk_socket__fd(*xsk_ptr) },
        };

        Ok((arc, rx, tx))
    }

    /// Create a new Rx-only AF_XDP socket
    pub fn new_rx(
        umem: Arc<Umem<'a, T>>,
        if_name: &str,
        queue: usize,
        rx_ring_size: u32,
        options: SocketOptions,
    ) -> Result<(Arc<Socket<'a, T>>, SocketRx<'a, T>), SocketNewError> {
        // Verify that the passed ring size is a power of two.
        // https://www.kernel.org/doc/html/latest/networking/af_xdp.html
        if !util::is_pow_of_two(rx_ring_size) {
            return Err(SocketNewError::RingNotPowerOfTwo);
        }

        let mut cfg = xsk_socket_config {
            rx_size: rx_ring_size,
            tx_size: 0,
            xdp_flags: XDP_FLAGS_UPDATE_IF_NOEXIST,
            bind_flags: XDP_USE_NEED_WAKEUP as u16,
            libbpf_flags: 0,
        };

        if options.zero_copy_mode {
            cfg.bind_flags |= XDP_ZEROCOPY as u16;
        }

        if options.copy_mode {
            cfg.bind_flags |= XDP_COPY as u16;
        }

        // Heap allocate since they are passed to the C function
        let mut rx: Box<xsk_ring_cons> = Default::default();

        // C function has double indirection
        let mut xsk: *mut xsk_socket = std::ptr::null_mut();
        let xsk_ptr: *mut *mut xsk_socket = &mut xsk;

        let if_name_c = CString::new(if_name).unwrap();

        let ret: std::os::raw::c_int;
        unsafe {
            ret = xsk_socket__create(
                xsk_ptr,
                if_name_c.as_ptr(),
                queue as u32,
                umem.get_ptr() as *mut xsk_umem,
                rx.as_mut(),
                std::ptr::null_mut(),
                &cfg,
            );
        }

        if ret != 0 {
            let errno = errno().0;
            return Err(SocketNewError::Create(std::io::Error::from_raw_os_error(
                errno,
            )));
        }

        let arc = Arc::new(Socket {
            umem,
            socket: unsafe { Box::from_raw(*xsk_ptr) },
            if_name_c,
        });

        let rx = SocketRx {
            socket: arc.clone(),
            fd: unsafe { xsk_socket__fd(*xsk_ptr) },
            rx,
        };

        Ok((arc, rx))
    }

    /// Create a new Tx-only AF_XDP socket
    pub fn new_tx(
        umem: Arc<Umem<'a, T>>,
        if_name: &str,
        queue: usize,
        tx_ring_size: u32,
        options: SocketOptions,
    ) -> Result<(Arc<Socket<'a, T>>, SocketTx<'a, T>), SocketNewError> {
        // Verify that the passed ring size is a power of two.
        // https://www.kernel.org/doc/html/latest/networking/af_xdp.html
        if !util::is_pow_of_two(tx_ring_size) {
            return Err(SocketNewError::RingNotPowerOfTwo);
        }

        let mut cfg = xsk_socket_config {
            rx_size: 0,
            tx_size: tx_ring_size,
            xdp_flags: XDP_FLAGS_UPDATE_IF_NOEXIST,
            bind_flags: XDP_USE_NEED_WAKEUP as u16,
            libbpf_flags: 0,
        };

        if options.zero_copy_mode {
            cfg.bind_flags |= XDP_ZEROCOPY as u16;
        }

        if options.copy_mode {
            cfg.bind_flags |= XDP_COPY as u16;
        }

        // Heap allocate since they are passed to the C function
        let mut tx: Box<xsk_ring_prod> = Default::default();

        // C function has double indirection
        let mut xsk: *mut xsk_socket = std::ptr::null_mut();
        let xsk_ptr: *mut *mut xsk_socket = &mut xsk;

        let if_name_c = CString::new(if_name).unwrap();

        let ret: std::os::raw::c_int;
        unsafe {
            ret = xsk_socket__create(
                xsk_ptr,
                if_name_c.as_ptr(),
                queue as u32,
                umem.get_ptr() as *mut xsk_umem,
                std::ptr::null_mut(),
                tx.as_mut(),
                &cfg,
            );
        }

        if ret != 0 {
            let errno = errno().0;
            return Err(SocketNewError::Create(std::io::Error::from_raw_os_error(
                errno,
            )));
        }

        let arc = Arc::new(Socket {
            umem,
            socket: unsafe { Box::from_raw(*xsk_ptr) },
            if_name_c,
        });

        let tx = SocketTx {
            socket: arc.clone(),
            tx,
            fd: unsafe { xsk_socket__fd(*xsk_ptr) },
        };

        Ok((arc, tx))
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for Socket<'a, T> {
    fn drop(&mut self) {
        unsafe {
            // No null pointer check here because it is initialized to null and if the create fails,
            // it should still be null and xsk_socket__delete handles null.
            xsk_socket__delete(self.socket.as_mut());
        }
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> SocketRx<'a, T> {
    #[inline]
    pub fn wake(&mut self) {
        // This is inefficient, we should either store the fds or maybe even better yet signal out
        // so that the caller can call poll once for many fds.
        let fd1: pollfd = pollfd {
            fd: self.fd,
            events: POLLIN,
            revents: 0,
        };

        let mut fds: [pollfd; 1] = [fd1];

        let ret: i32;
        unsafe {
            ret = poll(&mut fds[0], 1, POLL_TIMEOUT);
        }
        if ret < 0 {
            let errno = errno().0;
            println!("poll error in SocketRx wake: ret={} errno={}", ret, errno);
        }
    }

    /// Attempt to receive up to `batch_size` buffers from the socket.
    ///
    #[inline]
    pub fn try_recv(
        &mut self,
        bufs: &mut ArrayDeque<[BufMmap<T>; PENDING_LEN], Wrapping>,
        mut batch_size: usize,
        user: T,
    ) -> Result<usize, SocketError> {
        let mut idx_rx: u32 = 0;
        let rcvd: usize;

        batch_size = min(bufs.capacity() - bufs.len(), batch_size);

        unsafe {
            rcvd = _xsk_ring_cons__peek(self.rx.as_mut(), batch_size as u64, &mut idx_rx) as usize;
        }
        if rcvd == 0 {
            // Note that the caller needs to check if the queue needs to be woken up
            return Ok(0);
        }

        let buf_len_available = self.socket.umem.area.get_buf_len() - AF_XDP_RESERVED as usize;

        for _ in 0..rcvd {
            let desc: *const xdp_desc;
            let b: BufMmap<T>;

            unsafe {
                desc = _xsk_ring_cons__rx_desc(self.rx.as_mut(), idx_rx);
                let addr = (*desc).addr;
                let len = (*desc).len.try_into().unwrap();
                let ptr = self.socket.umem.area.get_ptr().offset(addr as isize);

                b = BufMmap {
                    addr,
                    len,
                    data: std::slice::from_raw_parts_mut(ptr as *mut u8, buf_len_available),
                    user,
                };
            }

            let r = bufs.push_back(b);
            if r.is_some() {
                // Since we set batch_size above based on how much space there is, this should
                // never happen.
                panic!("there should be space");
            }

            idx_rx += 1;
        }

        unsafe {
            _xsk_ring_cons__release(self.rx.as_mut(), rcvd as u64);
        }

        Ok(rcvd)
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for SocketRx<'a, T> {
    fn drop(&mut self) {
        // Cleanup is in the parent Socket
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> SocketTx<'a, T> {
    /// Attempt to send up to `batch_size` buffers on the socket.
    #[inline]
    pub fn try_send(
        &mut self,
        bufs: &mut ArrayDeque<[BufMmap<T>; PENDING_LEN], Wrapping>,
        mut batch_size: usize,
    ) -> Result<usize, SocketError> {
        let mut idx_tx: u32 = 0;
        let ready;

        batch_size = min(bufs.len(), batch_size);

        if batch_size == 0 {
            return Ok(0);
        }

        unsafe {
            ready =
                _xsk_ring_prod__reserve(self.tx.as_mut(), batch_size as u64, &mut idx_tx) as usize;
        }

        for _ in 0..ready {
            let b = bufs.pop_front().unwrap();

            unsafe {
                let desc = _xsk_ring_prod__tx_desc(self.tx.as_mut(), idx_tx);
                (*desc).addr = b.addr;
                (*desc).len = b.len.try_into().unwrap();
            }

            idx_tx += 1;
        }

        if ready > 0 {
            unsafe {
                _xsk_ring_prod__submit(self.tx.as_mut(), ready as u64);
            }
        }

        self.wakeup_if_required();

        Ok(ready)
    }

    /// Identify if this socket needs to be woken up (see [AF_XDP docs](https://www.kernel.org/doc/html/latest/networking/af_xdp.html#xdp-use-need-wakeup-bind-flag)).
    pub fn needs_wakeup(&mut self) -> bool {
        let ret;

        unsafe {
            ret = _xsk_ring_prod__needs_wakeup(self.tx.as_mut());
        }

        if ret != 0 {
            return true;
        }

        false
    }

    fn wakeup_if_required(&mut self) -> bool {
        unsafe {
            if _xsk_ring_prod__needs_wakeup(self.tx.as_mut()) != 0 {
                let ret = sendto(
                    self.fd,
                    std::ptr::null(),
                    0,
                    MSG_DONTWAIT,
                    std::ptr::null(),
                    0,
                );
                if ret >= 0 {
                    return true;
                } else {
                    // The xdpsock_user.c sample application checks for these specific errno values and panics
                    // otherwise. Copying that behavior for now.
                    let errno = errno().0;
                    match errno {
                        ENOBUFS | EAGAIN | EBUSY | ENETDOWN => {
                            // These error codes are OK according to the kernel xdpsock_user example.
                            // Note that EAGAIN may need to be handled differently for SKB XSK sockets.
                            // https://lore.kernel.org/netdev/6e58cde8-9e38-079a-589d-7b7a860ef61e@iogearbox.net/T/
                        }
                        _ => panic!("xdpsock_user.c sample panics here"),
                    }
                }

                return true;
            }

            false
        }
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for SocketTx<'a, T> {
    fn drop(&mut self) {
        // Cleanup is in the parent Socket
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::buf_mmap::BufMmap;
    use crate::mmap_area::{MmapArea, MmapAreaOptions, MmapError};
    use crate::socket::{Socket, SocketNewError, SocketOptions};
    use crate::umem::Umem;

    #[derive(Default, Copy, Clone, Debug)]
    struct BufCustom {}

    // Test the ring size constraints during socket creation
    #[test]
    fn ring_size1() {
        use rlimit::{setrlimit, Resource, Rlim};
        use std::io;
        use std::io::Write;

        let r = setrlimit(Resource::MEMLOCK, Rlim::INFINITY, Rlim::INFINITY);
        match r {
            Err(_) => {
                writeln!(
                    &mut io::stdout(),
                    "Test skipped as it needs to be run as root"
                )
                .unwrap();
                return;
            }
            Ok(_) => {
                // Expected
            }
        }

        const BUF_NUM: usize = 1024;
        const BUF_LEN: usize = 2048;

        let options = MmapAreaOptions { huge_tlb: false };
        let r: Result<(Arc<MmapArea<BufCustom>>, Vec<BufMmap<BufCustom>>), MmapError> =
            MmapArea::new(BUF_NUM, BUF_LEN, options);

        let (area, _) = match r {
            Ok((area, buf_pool)) => (area, buf_pool),
            Err(err) => panic!("{:?}", err),
        };

        let r = Umem::new(area, 1024, 1024);
        let (umem, _cq, _fillq) = match r {
            Err(err) => {
                panic!("{}", err)
            }
            Ok((umem, cq, fq)) => (umem, cq, fq),
        };

        let options = SocketOptions::default();

        // With both rings set to a power of two, this should fail with a non power of two error.
        let mut rx_ring_size: u32 = 2048;
        let mut tx_ring_size: u32 = 2048;

        let r = Socket::new(
            umem.clone(),
            "link1",
            1,
            rx_ring_size,
            tx_ring_size,
            options,
        );
        match r {
            Err(SocketNewError::RingNotPowerOfTwo) => {
                panic!("this error should not have have been returned");
            }
            Err(_) => {
                // Expected
            }
            Ok(_) => {
                panic!("socket creation should have failed")
            }
        }

        // With the first not a power of two, it should fail.
        rx_ring_size = 101;
        tx_ring_size = 2048;

        let r = Socket::new(
            umem.clone(),
            "link1",
            1,
            rx_ring_size,
            tx_ring_size,
            options,
        );
        match r {
            Err(SocketNewError::RingNotPowerOfTwo) => {
                // Expected
            }
            Err(err) => panic!("wrong error: {:?}", err),
            Ok(_) => {
                panic!("socket creation should have failed");
            }
        }

        // With the second not a power of two, it should fail.
        rx_ring_size = 2048;
        tx_ring_size = 149;

        let r = Socket::new(
            umem.clone(),
            "link1",
            1,
            rx_ring_size,
            tx_ring_size,
            options,
        );
        match r {
            Err(SocketNewError::RingNotPowerOfTwo) => {
                // Expected
            }
            Err(err) => panic!("wrong error: {:?}", err),
            Ok(_) => {
                panic!("socket creation should have failed");
            }
        }

        // With both not a power of two it should fail.
        rx_ring_size = 1999;
        tx_ring_size = 149;

        let r = Socket::new(
            umem.clone(),
            "link1",
            1,
            rx_ring_size,
            tx_ring_size,
            options,
        );
        match r {
            Err(SocketNewError::RingNotPowerOfTwo) => {
                // Expected
            }
            Err(err) => panic!("wrong error: {:?}", err),
            Ok(_) => {
                panic!("socket creation should have failed");
            }
        }
    }
}
