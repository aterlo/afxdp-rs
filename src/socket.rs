use std::cmp::min;
use std::convert::TryInto;
use std::ffi::CString;

use arraydeque::{ArrayDeque, Wrapping};
use errno::errno;
use libbpf_sys::{
    _xsk_ring_cons__peek, _xsk_ring_cons__release, _xsk_ring_cons__rx_desc,
    _xsk_ring_prod__needs_wakeup, _xsk_ring_prod__reserve, _xsk_ring_prod__submit,
    _xsk_ring_prod__tx_desc, xdp_desc, xsk_ring_cons, xsk_ring_prod, xsk_socket,
    xsk_socket__create, xsk_socket__delete, xsk_socket__fd, xsk_socket_config, XDP_COPY,
    XDP_FLAGS_UPDATE_IF_NOEXIST, XDP_USE_NEED_WAKEUP, XDP_ZEROCOPY,
    XSK_LIBBPF_FLAGS__INHIBIT_PROG_LOAD,
};
use libc::{poll, pollfd, sendto, EAGAIN, EBUSY, ENETDOWN, ENOBUFS, MSG_DONTWAIT, POLLIN};
use std::sync::Arc;

use crate::buf_mmap::BufMmap;
use crate::umem::Umem;
use crate::PENDING_LEN;

const POLL_TIMEOUT: i32 = 1000;
/// An AF_XDP socket
#[derive(Debug)]
pub struct Socket<'a, T: std::default::Default + std::marker::Copy> {
    umem: Arc<Umem<'a, T>>,
    socket: Box<xsk_socket>,
}
unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for Socket<'a, T> {}

/// A Rx only AF_XDP socket
#[derive(Debug)]
pub struct SocketRx<'a, T: std::default::Default + std::marker::Copy> {
    socket: Arc<Socket<'a, T>>,
    fd: std::os::raw::c_int,
    rx: Box<xsk_ring_cons>,
}
unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for SocketRx<'a, T> {}

/// A Tx only AF_XDP socket
#[derive(Debug)]
pub struct SocketTx<'a, T: std::default::Default + std::marker::Copy> {
    socket: Arc<Socket<'a, T>>,
    pub fd: std::os::raw::c_int,
    tx: Box<xsk_ring_prod>,
}
unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for SocketTx<'a, T> {}

#[derive(Debug)]
pub enum SocketError {
    Failed,
}

/// Configuration options for Socket
#[derive(Copy, Clone, Debug, Default)]
pub struct SocketOptions {
    /// Force XDP zero copy mode (XDP_ZEROCOPY flag)
    pub zero_copy_mode: bool,

    /// Force XDP copy mode (XDP_COPY flag)
    pub copy_mode: bool,
}

impl<'a, T: std::default::Default + std::marker::Copy> Socket<'a, T> {
    // Create a new Rx/Tx AF_XDP socket
    pub fn new(
        umem: Arc<Umem<'a, T>>,
        if_name: &str,
        queue: usize,
        rx_ring_size: u32,
        tx_ring_size: u32,
        options: SocketOptions,
    ) -> Result<(Arc<Socket<'a, T>>, SocketRx<'a, T>, SocketTx<'a, T>), SocketError> {
        let mut cfg = xsk_socket_config {
            rx_size: rx_ring_size,
            tx_size: tx_ring_size,
            xdp_flags: XDP_FLAGS_UPDATE_IF_NOEXIST,
            bind_flags: XDP_USE_NEED_WAKEUP as u16,
            libbpf_flags: 0,
        };

        if options.zero_copy_mode {
            cfg.bind_flags = cfg.bind_flags | XDP_ZEROCOPY as u16;
        }

        if options.copy_mode {
            cfg.bind_flags = cfg.bind_flags | XDP_COPY as u16;
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
                umem.umem.lock().unwrap().as_mut(),
                rx.as_mut(),
                tx.as_mut(),
                &cfg,
            );
        }

        if ret != 0 {
            println!("socket err: {}", ret);

            return Err(SocketError::Failed);
        }

        let arc = Arc::new(Socket {
            umem: umem,
            socket: unsafe { Box::from_raw(*xsk_ptr) },
        });

        let rx = SocketRx {
            socket: arc.clone(),
            fd: unsafe { xsk_socket__fd(*xsk_ptr) },
            rx: rx,
        };
        let tx = SocketTx {
            socket: arc.clone(),
            tx: tx,
            fd: unsafe { xsk_socket__fd(*xsk_ptr) },
        };

        Ok((arc, rx, tx))
    }

    // Create a new Rx AF_XDP socket
    pub fn new_rx(
        umem: Arc<Umem<'a, T>>,
        if_name: &str,
        queue: usize,
        rx_ring_size: u32,
        options: SocketOptions,
    ) -> Result<(Arc<Socket<'a, T>>, SocketRx<'a, T>), SocketError> {
        let mut cfg = xsk_socket_config {
            rx_size: rx_ring_size,
            tx_size: 0,
            xdp_flags: XDP_FLAGS_UPDATE_IF_NOEXIST,
            bind_flags: XDP_USE_NEED_WAKEUP as u16,
            libbpf_flags: 0,
        };

        if options.zero_copy_mode {
            cfg.bind_flags = cfg.bind_flags | XDP_ZEROCOPY as u16;
        }

        if options.copy_mode {
            cfg.bind_flags = cfg.bind_flags | XDP_COPY as u16;
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
                umem.umem.lock().unwrap().as_mut(),
                rx.as_mut(),
                std::ptr::null_mut(),
                &cfg,
            );
        }

        if ret != 0 {
            return Err(SocketError::Failed);
        }

        let arc = Arc::new(Socket {
            umem: umem,
            socket: unsafe { Box::from_raw(*xsk_ptr) },
        });

        let rx = SocketRx {
            socket: arc.clone(),
            fd: unsafe { xsk_socket__fd(*xsk_ptr) },
            rx: rx,
        };

        Ok((arc, rx))
    }

    // Create a new Tx AF_XDP socket
    pub fn new_tx(
        umem: Arc<Umem<'a, T>>,
        if_name: &str,
        queue: usize,
        tx_ring_size: u32,
        options: SocketOptions,
    ) -> Result<(Arc<Socket<'a, T>>, SocketTx<'a, T>), SocketError> {
        let mut cfg = xsk_socket_config {
            rx_size: 0,
            tx_size: tx_ring_size,
            xdp_flags: XDP_FLAGS_UPDATE_IF_NOEXIST,
            bind_flags: XDP_USE_NEED_WAKEUP as u16,
            libbpf_flags: XSK_LIBBPF_FLAGS__INHIBIT_PROG_LOAD,
        };

        if options.zero_copy_mode {
            cfg.bind_flags = cfg.bind_flags | XDP_ZEROCOPY as u16;
        }

        if options.copy_mode {
            cfg.bind_flags = cfg.bind_flags | XDP_COPY as u16;
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
                umem.umem.lock().unwrap().as_mut(),
                std::ptr::null_mut(),
                tx.as_mut(),
                &cfg,
            );
        }

        if ret != 0 {
            return Err(SocketError::Failed);
        }

        let arc = Arc::new(Socket {
            umem,
            socket: unsafe { Box::from_raw(*xsk_ptr) },
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

        let buf_len = self.socket.umem.area.get_buf_len();

        for _ in 0..rcvd {
            let desc: *const xdp_desc;
            let b: BufMmap<T>;

            unsafe {
                desc = _xsk_ring_cons__rx_desc(self.rx.as_mut(), idx_rx);
                let addr = (*desc).addr.try_into().unwrap();
                let len = (*desc).len.try_into().unwrap();
                let ptr = self.socket.umem.area.get_ptr().offset(addr as isize);

                b = BufMmap {
                    addr,
                    len,
                    data: std::slice::from_raw_parts_mut(ptr as *mut u8, buf_len),
                    user,
                };
            }

            let r = bufs.push_back(b);
            match r {
                Some(_) => {
                    // Since we set batch_size above based on how much space there is, this should
                    // never happen.
                    panic!("there should be space");
                }
                None => {}
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
    #[inline]
    pub fn try_send(
        &mut self,
        bufs: &mut ArrayDeque<[BufMmap<T>; PENDING_LEN], Wrapping>,
        mut batch_size: usize,
    ) -> Result<usize, SocketError> {
        let mut idx_tx: u32 = 0;
        let ready;

        batch_size = min(bufs.len(), batch_size);

        unsafe {
            ready =
                _xsk_ring_prod__reserve(self.tx.as_mut(), batch_size as u64, &mut idx_tx) as usize;
        }

        for _ in 0..ready {
            let b: BufMmap<T>;

            b = bufs.pop_front().unwrap();

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
                            // These error codes are OK according to the kernel xdpsock_user example
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
