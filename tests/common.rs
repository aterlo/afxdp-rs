//
// Test infrastructure which runs two threads, each attached to the opposite ends of a veth.
// It then passes traffic between the interfaces as defined by the passed in test instance.
//
use std::cmp::min;
use std::hint;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time;
use std::{fmt::Display, io};

use arraydeque::{ArrayDeque, Wrapping};
use rlimit::{setrlimit, Resource};

use afxdp::buf_pool::BufPool;
use afxdp::mmap_area::{MmapArea, MmapAreaOptions, MmapError};
use afxdp::socket::{Socket, SocketOptions, SocketRx, SocketTx};
use afxdp::umem::{Umem, UmemCompletionQueue, UmemFillQueue};
use afxdp::PENDING_LEN;
use afxdp::{buf_mmap::BufMmap, buf_pool_vec::BufPoolVec};

const RING_SIZE: u32 = 512;

const BUF_SIZE: usize = 2048;
const BUF_NUM: usize = 4096;
const SOCKET_BATCH_SIZE: usize = 64;
const FILL_THRESHOLD: usize = 64;
const FILL_BATCH_SIZE: usize = 128;
const SERVICE_THRESHOLD: usize = 64;
const SERVICE_BATCH_SIZE: usize = 128;

#[derive(Default, Copy, Clone)]
pub struct BufCustom {}

pub trait TrafficTest {
    fn done(&mut self, now: time::Instant) -> bool;

    fn transmit<'a>(
        &mut self,
        now: time::Instant,
        port: usize,
        bufs: &mut Vec<BufMmap<'a, BufCustom>>,
        pending: &mut ArrayDeque<[BufMmap<'a, BufCustom>; PENDING_LEN], Wrapping>,
    ) -> usize;

    fn receive<'a>(
        &mut self,
        now: time::Instant,
        port: usize,
        bufs: &mut Vec<BufMmap<'a, BufCustom>>,
        pending: &mut ArrayDeque<[BufMmap<'a, BufCustom>; PENDING_LEN], Wrapping>,
    ) -> usize;

    fn finalize(&self);
}

struct WorkerConfig<'a, T>
where
    T: TrafficTest,
{
    test: Arc<Mutex<T>>,

    bp: BufPoolVec<BufMmap<'a, BufCustom>, BufCustom>,

    port: usize, // 0 or 1
    link_rx: SocketRx<'a, BufCustom>,
    link_tx: SocketTx<'a, BufCustom>,
    link_cq: UmemCompletionQueue<'a, BufCustom>,
    link_fq: UmemFillQueue<'a, BufCustom>,
}

fn gen_worker_config<T>(
    link_name: &str,
    link_channel: usize,
    port: usize, // 0 or 1
    buf_size: usize,
    buf_num: usize,
    test: Arc<Mutex<T>>,
) -> WorkerConfig<T>
where
    T: TrafficTest,
{
    assert!(port <= 1);

    let options: MmapAreaOptions = Default::default();

    let r: Result<(std::sync::Arc<MmapArea<BufCustom>>, Vec<BufMmap<BufCustom>>), MmapError> =
        MmapArea::new(buf_num, buf_size, options);
    let (area, mut bufs) = match r {
        Ok((area, bufs)) => (area, bufs),
        Err(err) => panic!("Failed to create MmapArea: {:?}", err),
    };
    println!(
        "Created MmapArea with {} buffers of size {} for a total of {} bytes",
        bufs.len(),
        buf_size,
        bufs.len() * buf_size,
    );

    // Add all the Bufs to the Buf pool
    let mut bp: BufPoolVec<BufMmap<BufCustom>, BufCustom> = BufPoolVec::new(bufs.len());
    let len = bufs.len();
    let r = bp.put(&mut bufs, len);
    assert!(r == len);

    //
    // Create the AF_XDP umem and sockets
    //
    let r = Umem::new(area.clone(), RING_SIZE, RING_SIZE);
    let (umem1, umem1cq, umem1fq) = match r {
        Ok(umem) => umem,
        Err(err) => panic!("Failed to create Umem: {:?}", err),
    };

    let options = SocketOptions::default();

    let r = Socket::new(
        umem1.clone(),
        link_name,
        link_channel,
        RING_SIZE,
        RING_SIZE,
        options,
    );
    let (_skt1, skt1rx, skt1tx) = match r {
        Ok(skt) => skt,
        Err(err) => panic!(
            "Failed to create socket for {}:{} - {:?}",
            link_name, link_channel, err
        ),
    };

    WorkerConfig {
        bp,
        test,
        port,
        link_rx: skt1rx,
        link_tx: skt1tx,
        link_cq: umem1cq,
        link_fq: umem1fq,
    }
}
#[derive(Default, Debug, Clone, Copy)]
struct Stats {
    cq_bufs_received: usize,
    fq_bufs_filled: usize,
    rx_packets: usize,
    tx_packets: usize,

    cq_deficit: usize,
    fq_deficit: usize,
}

fn do_worker<T>(mut config: WorkerConfig<T>)
where
    T: TrafficTest,
{
    // In this example we don't share the global pool outside this thread so just take all the
    // buffers from it.
    let total_bufs = config.bp.len();
    let mut bufs = Vec::with_capacity(total_bufs);

    let r = config.bp.get(&mut bufs, total_bufs);
    if r != total_bufs {
        println!(
            "Failed to get initial bufs. Wanted {} got {}",
            total_bufs, r
        );
    }

    let initial_fill_num: usize = RING_SIZE as usize;
    println!("Filling umem with {} buffers", initial_fill_num);
    let r = config.link_fq.fill(&mut bufs, initial_fill_num);
    match r {
        Ok(n) => {
            if n != initial_fill_num {
                panic!(
                    "Initial fill of umem1 incomplete: {} of {}",
                    n, initial_fill_num
                );
            }
        }
        Err(err) => println!("error: {:?}", err),
    }

    //
    // Main AF_XDP loop
    //
    let mut pending_receive: ArrayDeque<[BufMmap<BufCustom>; PENDING_LEN], Wrapping> =
        ArrayDeque::new();
    let mut pending_send: ArrayDeque<[BufMmap<BufCustom>; PENDING_LEN], Wrapping> =
        ArrayDeque::new();

    let mut stats: Stats = Default::default();

    let bc = BufCustom {};
    let mut now: time::Instant;
    let mut did_work: bool;
    loop {
        now = time::Instant::now();
        did_work = false;

        //
        // Is the test done?
        //
        if config.test.lock().unwrap().done(now) {
            println!("pending_send: {}", pending_send.len());
            println!("pending_receive: {}", pending_receive.len());
            println!("stats.tx_packets: {}", stats.tx_packets);
            println!("stats.rx_packets: {}", stats.rx_packets);
            break;
        }

        //
        // Service completion queue
        //
        if stats.cq_deficit >= SERVICE_THRESHOLD {
            let r = config.link_cq.service(&mut bufs, SERVICE_BATCH_SIZE);
            match r {
                Ok(n) => {
                    stats.cq_bufs_received += n;
                    stats.cq_deficit -= n;

                    did_work = true;
                }
                Err(err) => panic!("error: {:?}", err),
            }
        }

        //
        // Receive
        //
        let r = config
            .link_rx
            .try_recv(&mut pending_receive, SOCKET_BATCH_SIZE, bc);
        match r {
            Ok(n) => {
                if n > 0 {
                    stats.rx_packets += n;
                    stats.fq_deficit += n;

                    did_work = true;
                } else if config.link_fq.needs_wakeup() {
                    config.link_rx.wake();
                }
            }
            Err(err) => {
                panic!("error: {:?}", err);
            }
        }

        //
        // Pass the received packets to the test
        //
        config
            .test
            .lock()
            .unwrap()
            .receive(now, config.port, &mut bufs, &mut pending_receive);

        //
        // Ask the test for packets and send them
        //
        config
            .test
            .lock()
            .unwrap()
            .transmit(now, config.port, &mut bufs, &mut pending_send);

        //
        // Send the pending packets
        //
        let r = config
            .link_tx
            .try_send(&mut pending_send, SOCKET_BATCH_SIZE);
        match r {
            Ok(n) => {
                if n > 0 {
                    stats.tx_packets += n;
                    stats.cq_deficit += n;

                    did_work = true;
                }
            }
            Err(err) => println!("shouldn't happen: {:?}", err),
        }

        //
        // Fill buffers if required
        //
        if stats.fq_deficit >= FILL_THRESHOLD {
            // Fill a maximum of FILL_BATCH_SIZE at a time. This is done on the assumption
            // we don't want this to ever do too many at once.
            let fill_num = min(FILL_BATCH_SIZE, stats.fq_deficit);
            let r = config.link_fq.fill(&mut bufs, fill_num);
            match r {
                Ok(n) => {
                    stats.fq_bufs_filled += n;
                    stats.fq_deficit -= n;

                    did_work = true;
                }
                Err(err) => panic!("error: {:?}", err),
            }
        }

        if !did_work {
            // No useful work was done this loop iteration
            hint::spin_loop();
        }
    }
}

pub fn run<T>(test: T)
where
    T: 'static + TrafficTest + Send + Display,
{
    let r = setrlimit(Resource::MEMLOCK, rlimit::INFINITY, rlimit::INFINITY);
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

    //
    // Wrap the test
    //
    let test = Arc::new(Mutex::new(test));

    //
    // Create the worker configurations
    //
    let worker1_config = gen_worker_config("veth1", 0, 0, BUF_SIZE, BUF_NUM, test.clone());
    let worker2_config = gen_worker_config("veth2", 0, 1, BUF_SIZE, BUF_NUM, test.clone());

    //
    // Start the workers
    //
    let mut thread_handles = Vec::new();

    let handle = thread::spawn(|| {
        do_worker(worker1_config);
    });
    thread_handles.push(handle);

    let handle = thread::spawn(|| {
        do_worker(worker2_config);
    });
    thread_handles.push(handle);

    //
    // Wait for the threads
    //
    for handle in thread_handles {
        let r = handle.join();
        match r {
            Ok(_) => {}
            Err(err) => println!("thread error: {:?}", err),
        }
    }

    //
    // Perform the last verification steps
    //
    test.lock().unwrap().finalize();
}
