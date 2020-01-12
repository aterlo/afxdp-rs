use std::cmp::min;
use std::ffi::CString;

use arraydeque::{ArrayDeque, Wrapping};
use errno::errno;
use libbpf_sys::{
    _xsk_ring_cons__comp_addr, _xsk_ring_cons__peek, _xsk_ring_cons__release,
    _xsk_ring_cons__rx_desc, _xsk_ring_prod__fill_addr, _xsk_ring_prod__needs_wakeup,
    _xsk_ring_prod__reserve, _xsk_ring_prod__submit, _xsk_ring_prod__tx_desc, xdp_desc,
    xsk_ring_cons, xsk_ring_prod, xsk_socket, xsk_socket__create, xsk_socket__delete,
    xsk_socket__fd, xsk_socket_config, xsk_umem, xsk_umem__create, xsk_umem__delete,
    xsk_umem_config, XDP_COPY, XDP_FLAGS_SKB_MODE, XDP_FLAGS_UPDATE_IF_NOEXIST,
    XDP_UMEM_UNALIGNED_CHUNK_FLAG, XDP_USE_NEED_WAKEUP, XDP_ZEROCOPY,
    XSK_LIBBPF_FLAGS__INHIBIT_PROG_LOAD, XSK_RING_CONS__DEFAULT_NUM_DESCS,
    XSK_RING_PROD__DEFAULT_NUM_DESCS, XSK_UMEM__DEFAULT_FRAME_HEADROOM,
};
use libc::{
    c_int, c_void, mmap, munmap, poll, pollfd, sendto, EAGAIN, EBUSY, ENOBUFS, MAP_ANONYMOUS,
    MAP_HUGETLB, MAP_PRIVATE, MSG_DONTWAIT, POLLIN, POLLOUT, PROT_READ, PROT_WRITE,
};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

const POLL_TIMEOUT: i32 = 1000;
pub const PENDING_LEN: usize = 4096; // TODO: Experiment with size

/// A mapped memory area used to move packets between the kernel and userspace.
#[derive(Debug)]
pub struct MmapArea<'a, T: std::default::Default + std::marker::Copy> {
    phantom: PhantomData<&'a T>,
    buf_num: usize,
    buf_len: usize,
    ptr: *mut c_void,
}
unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for MmapArea<'a, T> {}

#[derive(Debug)]
pub enum MmapError {
    Failed,
}

impl<'a, T: std::default::Default + std::marker::Copy> MmapArea<'a, T> {
    /// Allocate a new memory mapped area based on the size and number of buffers
    ///
    /// # Arguments
    ///
    /// * buf_num: The number of buffers to allocate in the memory mapped area
    /// * buf_len: The length of each buffer
    pub fn new(
        buf_num: usize,
        buf_len: usize,
    ) -> Result<(Arc<MmapArea<'a, T>>, Arc<Mutex<BufPool<'a, T>>>), MmapError> {
        let ptr: *mut c_void;

        unsafe {
            ptr = mmap(
                0 as *mut c_void,
                buf_num * buf_len,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB,
                -1,
                0,
            );
        }

        // TODO: Is this the correct way to check for NULL c_void?
        if ptr == std::ptr::null_mut() {
            return Err(MmapError::Failed);
        }

        let ma = Arc::new(MmapArea {
            buf_num: buf_num,
            buf_len: buf_len,
            ptr: ptr,
            phantom: PhantomData,
        });

        let r: Result<Arc<Mutex<BufPool<T>>>, BufPoolError> =
            BufPool::new(ma.clone(), buf_num, buf_len);
        //let r = BufPool::new(ma.clone(), buf_num, buf_len);
        if let Ok(bp) = r {
            Ok((ma, bp))
        } else {
            Err(MmapError::Failed)
        }
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for MmapArea<'a, T> {
    fn drop(&mut self) {
        let r: c_int;

        unsafe {
            r = munmap(self.ptr, self.buf_num * self.buf_len);
        }

        if r != 0 {
            let errno = errno().0;
            println!("munmap failed errno: {}", errno);
        }
    }
}

/// AF_XDP Umem
#[derive(Debug)]
pub struct Umem<'a, T: std::default::Default + std::marker::Copy> {
    area: Arc<MmapArea<'a, T>>,
    umem: Mutex<Box<xsk_umem>>,
}
unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for Umem<'a, T> {}

/// Completion queue per Umem
#[derive(Debug)]
pub struct UmemCompletionQueue<'a, T: std::default::Default + std::marker::Copy> {
    umem: Arc<Umem<'a, T>>,
    cq: Box<xsk_ring_cons>,
}
unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for UmemCompletionQueue<'a, T> {}

/// Fill queue per Umem
#[derive(Debug)]
pub struct UmemFillQueue<'a, T: std::default::Default + std::marker::Copy> {
    umem: Arc<Umem<'a, T>>,
    fq: Box<xsk_ring_prod>,
}
unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for UmemFillQueue<'a, T> {}

#[derive(Debug)]
pub enum UmemError {
    Failed,
}

impl<'a, T: std::default::Default + std::marker::Copy> Umem<'a, T> {
    /// Create a new Umem using the passed memory mapped area
    pub fn new(
        area: Arc<MmapArea<'a, T>>,
        completion_ring_size: u32,
        fill_ring_size: u32,
    ) -> Result<
        (
            Arc<Umem<'a, T>>,
            UmemCompletionQueue<'a, T>,
            UmemFillQueue<'a, T>,
        ),
        UmemError,
    > {
        let cfg = xsk_umem_config {
            fill_size: fill_ring_size,
            comp_size: completion_ring_size,
            frame_size: area.buf_len as u32,
            frame_headroom: XSK_UMEM__DEFAULT_FRAME_HEADROOM,
            flags: 0,
        };

        /*
        println!(
            "umem config: fill_size={} comp_size={} frame_size={} frame_headroom={} flags={}",
            cfg.fill_size, cfg.comp_size, cfg.frame_size, cfg.frame_headroom, cfg.flags
        );
        */

        // Allocate the rings on the heap since we pass these pointers to xsk_umem_create()
        let mut cq: Box<xsk_ring_cons> = Default::default();
        let mut fq: Box<xsk_ring_prod> = Default::default();

        // Double indirection in C function
        let mut umem: *mut xsk_umem = std::ptr::null_mut();
        let umem_ptr: *mut *mut xsk_umem = &mut umem;

        let ret: std::os::raw::c_int;
        unsafe {
            let size = (area.buf_num * area.buf_len) as u64;
            ret = xsk_umem__create(umem_ptr, area.ptr, size, fq.as_mut(), cq.as_mut(), &cfg);
        }

        if ret != 0 {
            return Err(UmemError::Failed);
        }

        let arc = Arc::new(Umem {
            area: area,
            umem: Mutex::new(unsafe { Box::from_raw(*umem_ptr) }),
        });

        let cq = UmemCompletionQueue {
            umem: arc.clone(),
            cq: cq,
        };
        let fq = UmemFillQueue {
            umem: arc.clone(),
            fq: fq,
        };

        Ok((arc, cq, fq))
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for Umem<'a, T> {
    fn drop(&mut self) {
        unsafe {
            xsk_umem__delete(self.umem.lock().unwrap().as_mut());
        }
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> UmemCompletionQueue<'a, T> {
    /// After packets have been transmitted, the buffer is returned via the completion queue. The service
    /// method processed the completion queue.
    pub fn service(
        &mut self,
        bufs: &mut Vec<Buf<T>>,
        batch_size: usize,
    ) -> Result<usize, UmemError> {
        let mut idx: u32 = 0;
        let ready: usize;
        let batch_size = min(bufs.capacity() - bufs.len(), batch_size);

        unsafe {
            ready = _xsk_ring_cons__peek(self.cq.as_mut(), batch_size, &mut idx);
        }
        if ready == 0 {
            return Ok(0);
        }

        let buf_len = self.umem.area.buf_len;

        for _ in 0..ready {
            let buf: Buf<T>;

            unsafe {
                let ptr = _xsk_ring_cons__comp_addr(self.cq.as_mut(), idx);
                idx += 1;

                // Note that the completion and fill queues operate on offsets not
                // buffer numbers while Buf contains the buffer number.
                buf = Buf {
                    addr: *ptr / buf_len as u64,
                    len: buf_len as u32,
                    data: std::slice::from_raw_parts_mut(ptr as *mut u8, buf_len as usize),
                    custom: Default::default(),
                };
            }

            bufs.push(buf);
        }

        unsafe {
            _xsk_ring_cons__release(self.cq.as_mut(), ready);
        }

        Ok(ready)
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for UmemCompletionQueue<'a, T> {
    fn drop(&mut self) {}
}

impl<'a, T: std::default::Default + std::marker::Copy> UmemFillQueue<'a, T> {
    /// In order to receive packets, the link needs buffers to write the packets to. These buffers are sent from
    /// userspace to the kernel via the fill queue.
    pub fn fill(
        &mut self,
        bufs: &mut Vec<Buf<T>>,
        mut batch_size: usize,
    ) -> Result<usize, UmemError> {
        let mut idx: u32 = 0;
        let ready: usize;

        batch_size = min(bufs.len(), batch_size);

        unsafe {
            ready = _xsk_ring_prod__reserve(self.fq.as_mut(), batch_size, &mut idx);
        }

        if ready > 0 {
            let buf_len = self.umem.area.buf_len;

            for _ in 0..ready {
                let b = bufs.pop();
                match b {
                    Some(b) => unsafe {
                        let ptr = _xsk_ring_prod__fill_addr(self.fq.as_mut(), idx);
                        idx += 1;

                        // Note that the completion and fill queues operate on offsets not
                        // buffer numbers while Buf contains the buffer number.
                        *ptr = (b.addr * buf_len as u64).into();
                    },
                    None => {
                        todo!("tried to get buffer from empty pool");
                    }
                }
            }
        }

        unsafe {
            _xsk_ring_prod__submit(self.fq.as_mut(), ready);
        }

        Ok(ready)
    }

    pub fn needs_wakeup(&mut self) -> bool {
        unsafe {
            if _xsk_ring_prod__needs_wakeup(self.fq.as_mut()) != 0 {
                return true;
            }

            false
        }
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for UmemFillQueue<'a, T> {
    fn drop(&mut self) {}
}

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
    fd: std::os::raw::c_int,
    tx: Box<xsk_ring_prod>,
}
unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for SocketTx<'a, T> {}

#[derive(Debug)]
pub enum SocketError {
    Failed,
}

impl<'a, T: std::default::Default + std::marker::Copy> Socket<'a, T> {
    // Create a new Rx/Tx AF_XDP socket
    pub fn new(
        umem: Arc<Umem<'a, T>>,
        if_name: &str,
        queue: usize,
        rx_ring_size: u32,
        tx_ring_size: u32,
    ) -> Result<(Arc<Socket<'a, T>>, SocketRx<'a, T>, SocketTx<'a, T>), SocketError> {
        let cfg = xsk_socket_config {
            rx_size: rx_ring_size,
            tx_size: tx_ring_size,
            xdp_flags: XDP_FLAGS_UPDATE_IF_NOEXIST,
            bind_flags: XDP_USE_NEED_WAKEUP as u16 | XDP_ZEROCOPY as u16,
            libbpf_flags: 0,
        };

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
    ) -> Result<(Arc<Socket<'a, T>>, SocketRx<'a, T>), SocketError> {
        let cfg = xsk_socket_config {
            rx_size: rx_ring_size,
            tx_size: 0,
            xdp_flags: XDP_FLAGS_UPDATE_IF_NOEXIST,
            bind_flags: XDP_USE_NEED_WAKEUP as u16 | XDP_ZEROCOPY as u16,
            libbpf_flags: 0,
        };

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
    ) -> Result<(Arc<Socket<'a, T>>, SocketTx<'a, T>), SocketError> {
        let cfg = xsk_socket_config {
            rx_size: 0,
            tx_size: tx_ring_size,
            xdp_flags: XDP_FLAGS_UPDATE_IF_NOEXIST,
            bind_flags: XDP_USE_NEED_WAKEUP as u16 | XDP_ZEROCOPY as u16,
            libbpf_flags: XSK_LIBBPF_FLAGS__INHIBIT_PROG_LOAD,
        };

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
            umem: umem,
            socket: unsafe { Box::from_raw(*xsk_ptr) },
        });

        let tx = SocketTx {
            socket: arc.clone(),
            tx: tx,
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
    pub fn wake(&mut self) {
        // This is inefficient, we should either store the fds or maybe even better yet signal out
        // so that the caller can call poll once for many fds.
        let fd1: pollfd = pollfd {
            fd: self.fd,
            events: POLLIN | POLLOUT,
            revents: 0,
        };

        let mut fds: [pollfd; 1] = [fd1];

        unsafe {
            poll(&mut fds[0], 1, POLL_TIMEOUT);
        }
    }

    pub fn try_recv(
        &mut self,
        bufs: &mut ArrayDeque<[Buf<T>; PENDING_LEN], Wrapping>,
        mut batch_size: usize,
        custom: T,
    ) -> Result<usize, SocketError> {
        let mut idx_rx: u32 = 0;
        let rcvd: usize;
        batch_size = min(bufs.capacity() - bufs.len(), batch_size);

        unsafe {
            rcvd = _xsk_ring_cons__peek(self.rx.as_mut(), batch_size, &mut idx_rx);
        }
        if rcvd == 0 {
            // Note that the caller needs to check if the queue needs to be woken up
            return Ok(0);
        }

        for _ in 0..rcvd {
            let desc: *const xdp_desc;
            let b: Buf<T>;

            unsafe {
                desc = _xsk_ring_cons__rx_desc(self.rx.as_mut(), idx_rx);
                let ptr = self.socket.umem.area.ptr.offset((*desc).addr as isize);
                b = Buf {
                    addr: (*desc).addr,
                    len: (*desc).len,
                    data: std::slice::from_raw_parts_mut(ptr as *mut u8, (*desc).len as usize),
                    custom: custom,
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
            _xsk_ring_cons__release(self.rx.as_mut(), rcvd);
        }

        Ok(rcvd)
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for SocketRx<'a, T> {
    fn drop(&mut self) {}
}

impl<'a, T: std::default::Default + std::marker::Copy> SocketTx<'a, T> {
    pub fn try_send(
        &mut self,
        bufs: &mut ArrayDeque<[Buf<T>; PENDING_LEN], Wrapping>,
        mut batch_size: usize,
    ) -> Result<usize, SocketError> {
        let mut idx_tx: u32 = 0;
        let ready;

        batch_size = min(bufs.len(), batch_size);

        unsafe {
            ready = _xsk_ring_prod__reserve(self.tx.as_mut(), batch_size, &mut idx_tx);
        }

        for _ in 0..ready {
            let b: Buf<T>;

            b = bufs.pop_front().unwrap();

            unsafe {
                let desc = _xsk_ring_prod__tx_desc(self.tx.as_mut(), idx_tx);
                (*desc).len = b.len;
                (*desc).addr = b.addr;
            }

            idx_tx += 1;
        }

        if ready > 0 {
            unsafe {
                _xsk_ring_prod__submit(self.tx.as_mut(), ready);
            }
        }

        self.wakeup_if_required();

        Ok(ready)
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
                        ENOBUFS | EAGAIN | EBUSY => {}
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
    fn drop(&mut self) {}
}

/// Internal buffer used to hold packets. It can be extended with T.
pub struct Buf<'a, T> {
    addr: u64,
    len: u32,
    pub data: &'a mut [u8],
    pub custom: T,
}

/// BufPool stores Bufs. It can be used in a single thread or wrapped in a Mutex to provide a buffer pool to multiple
/// threads.
pub struct BufPool<'a, T: std::default::Default + std::marker::Copy> {
    area: Arc<MmapArea<'a, T>>,
    bufs: Vec<Buf<'a, T>>,
}

unsafe impl<'a, T: std::default::Default + std::marker::Copy> Send for BufPool<'a, T> {}

#[derive(Debug)]
pub enum BufPoolError {
    Failed,
}

impl<'a, T: std::default::Default + std::marker::Copy> BufPool<'a, T> {
    fn new(
        area: Arc<MmapArea<'a, T>>,
        buf_num: usize,
        buf_len: usize,
    ) -> Result<Arc<Mutex<BufPool<'a, T>>>, BufPoolError> {
        let mut bufs: Vec<Buf<T>> = Vec::with_capacity(buf_num);

        for i in 0..buf_num {
            let buf: Buf<T>;
            unsafe {
                let ptr = area.ptr.offset(i as isize);
                buf = Buf::<T> {
                    addr: i as u64,
                    len: buf_len as u32,
                    data: std::slice::from_raw_parts_mut(ptr as *mut u8, buf_len),
                    custom: Default::default(),
                };
            }

            bufs.push(buf);
        }

        let bp = BufPool {
            area: area,
            bufs: bufs,
        };

        Ok(Arc::new(Mutex::new(bp)))
    }

    pub fn get(&mut self, bufs: &mut Vec<Buf<'a, T>>, num: usize) -> Result<usize, BufPoolError> {
        let ready = min(num, self.bufs.len());

        // TODO: This probably isn't the fastest way to do this. Doable w/ one memcpy?
        for _ in 0..ready {
            let r = self.bufs.pop();
            match r {
                Some(buf) => {
                    bufs.push(buf);
                }
                None => panic!("shouldn't happen"),
            }
        }

        Ok(ready)
    }

    pub fn put(&mut self, bufs: &mut Vec<Buf<'a, T>>, num: usize) -> Result<usize, BufPoolError> {
        let ready = min(num, bufs.len());

        // TODO: This probably isn't the fastest way to do this. Doable w/ one memcpy?
        for _ in 0..ready {
            let r = bufs.pop();
            match r {
                Some(buf) => {
                    self.bufs.push(buf);
                }
                None => panic!("shouldn't happen"),
            }
        }

        Ok(ready)
    }

    pub fn len(&self) -> usize {
        self.bufs.len()
    }
}

impl<'a, T: std::default::Default + std::marker::Copy> Drop for BufPool<'a, T> {
    fn drop(&mut self) {}
}
