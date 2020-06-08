//
// Simple example that forwards frames directly between two links
//
// This example pulls packets from one channel (ethtool -l) from each link. The channel ID is passed via the command
// line args. The easiest way to direct all packets received on a link to a single channel is with:
//     ethtool -X
//
use arraydeque::{ArrayDeque, Wrapping};
use rlimit::{setrlimit, Resource, RLIM_INFINITY};
use std::cmp::min;
use structopt::StructOpt;

use afxdp::buf::Buf;
use afxdp::bufpool::BufPool;
use afxdp::mmaparea::{MmapArea, MmapAreaOptions, MmapError};
use afxdp::socket::{Socket, SocketOptions, SocketRx, SocketTx};
use afxdp::umem::{Umem, UmemCompletionQueue, UmemFillQueue};
use afxdp::PENDING_LEN;

const RING_SIZE: u32 = 4096;
const SOCKET_BATCH_SIZE: usize = 1024;
const SERVICE_BATCH_SIZE: usize = 1024;
const FILL_THRESHOLD: usize = 512;

fn forward(
    tx: &mut SocketTx<BufCustom>,
    bufs: &mut ArrayDeque<[Buf<BufCustom>; PENDING_LEN], Wrapping>,
) -> Result<usize, ()> {
    if bufs.is_empty() {
        return Ok(0);
    }

    let r = tx.try_send(bufs, SOCKET_BATCH_SIZE);
    match r {
        Ok(n) => Ok(n),
        Err(_) => panic!("shouldn't happen"),
    }
}

struct State<'a> {
    cq: UmemCompletionQueue<'a, BufCustom>,
    fq: UmemFillQueue<'a, BufCustom>,
    rx: SocketRx<'a, BufCustom>,
    tx: SocketTx<'a, BufCustom>,
    fq_deficit: usize,
}

#[derive(Default)]
struct Stats {
    cq_bufs_received: usize,
    fq_bufs_filled: usize,
    rx_packets: usize,
    tx_packets: usize,
}

#[derive(Default, Copy, Clone)]
struct BufCustom {}

#[derive(StructOpt, Debug)]
#[structopt(name = "l2fwd-2link")]
struct Opt {
    #[structopt(long, default_value = "2048")]
    bufsize: usize,

    #[structopt(long, default_value = "4096")]
    bufnum: usize,

    #[structopt(long, default_value = "none")]
    link1_name: std::string::String,

    #[structopt(long, default_value = "0")]
    link1_channel: usize,

    #[structopt(long, default_value = "none")]
    link2_name: std::string::String,

    #[structopt(long, default_value = "0")]
    link2_channel: usize,

    #[structopt(long)]
    huge_tlb: bool,

    #[structopt(long)]
    zero_copy: bool,

    #[structopt(long)]
    copy: bool,
}

fn main() {
    let opt = Opt::from_args();

    if opt.link1_name == "none" || opt.link2_name == "none" {
        println!("Link name parameters must be passed");
        return;
    }

    let initial_fill_num = min(opt.bufnum / 2, RING_SIZE as usize);

    assert!(setrlimit(Resource::MEMLOCK, RLIM_INFINITY, RLIM_INFINITY).is_ok());

    let options: MmapAreaOptions;
    if opt.huge_tlb {
        options = MmapAreaOptions { huge_tlb: true };
    } else {
        options = MmapAreaOptions { huge_tlb: false };
    }
    let r: Result<
        (
            std::sync::Arc<MmapArea<BufCustom>>,
            std::sync::Arc<std::sync::Mutex<BufPool<'_, BufCustom>>>,
        ),
        MmapError,
    > = MmapArea::new(opt.bufnum, opt.bufsize, options);
    let (area, buf_pool) = match r {
        Ok((area, buf_pool)) => (area, buf_pool),
        Err(err) => panic!("Failed to create MmapArea: {:?}", err),
    };

    let r = Umem::new(area.clone(), RING_SIZE, RING_SIZE);
    let (umem1, umem1cq, mut umem1fq) = match r {
        Ok(umem) => umem,
        Err(err) => panic!("Failed to create Umem: {:?}", err),
    };

    let r = Umem::new(area.clone(), RING_SIZE, RING_SIZE);
    let (umem2, umem2cq, mut umem2fq) = match r {
        Ok(umem) => umem,
        Err(err) => panic!("Failed to create Umem: {:?}", err),
    };

    let mut options = SocketOptions::default();
    options.zero_copy_mode = opt.zero_copy;
    options.copy_mode = opt.copy;

    let r = Socket::new(
        umem1.clone(),
        &opt.link1_name,
        opt.link1_channel,
        RING_SIZE,
        RING_SIZE,
        options,
    );
    let (_skt1, skt1rx, skt1tx) = match r {
        Ok(skt) => skt,
        Err(err) => panic!("Failed to create socket: {:?}", err),
    };

    let r = Socket::new(
        umem2.clone(),
        &opt.link2_name,
        opt.link2_channel,
        RING_SIZE,
        RING_SIZE,
        options,
    );
    let (_skt2, skt2rx, skt2tx) = match r {
        Ok(skt) => skt,
        Err(err) => panic!("Failed to create socket: {:?}", err),
    };

    //
    // Create our local pool of bufs
    // Since there are no other users of the pool, take all the bufs from the pool.
    //
    let mut bufs: Vec<Buf<BufCustom>> = Vec::with_capacity(opt.bufnum);
    let r = buf_pool.lock().unwrap().get(&mut bufs, opt.bufnum);
    match r {
        Ok(_) => {}
        Err(err) => panic!("error: {:?}", err),
    }

    //
    // umem1
    //
    let r = umem1fq.fill(&mut bufs, initial_fill_num);
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
    // umem2
    //
    let r = umem2fq.fill(&mut bufs, initial_fill_num);
    match r {
        Ok(n) => {
            if n != initial_fill_num {
                panic!(
                    "Initial fill of umem2 incomplete: {} of {}",
                    n, initial_fill_num
                );
            }
        }
        Err(err) => println!("error: {:?}", err),
    }

    //
    // The loop
    //

    let mut pending: [ArrayDeque<[Buf<BufCustom>; PENDING_LEN], Wrapping>; 2] =
        [ArrayDeque::new(), ArrayDeque::new()];

    let mut state: [State; 2] = [
        State {
            cq: umem1cq,
            fq: umem1fq,
            rx: skt1rx,
            tx: skt1tx,
            fq_deficit: 0,
        },
        State {
            cq: umem2cq,
            fq: umem2fq,
            rx: skt2rx,
            tx: skt2tx,
            fq_deficit: 0,
        },
    ];

    let mut stats: [Stats; 2] = [Default::default(), Default::default()];

    let mut pos: usize = 0;
    let bc = BufCustom {};
    loop {
        let other = (pos + 1) % 2;

        //
        // Service completion queue
        //
        let r = state[pos].cq.service(&mut bufs, SERVICE_BATCH_SIZE);
        match r {
            Ok(n) => {
                stats[pos].cq_bufs_received += n;
            }
            Err(err) => panic!("error: {:?}", err),
        }

        //
        // Receive
        //
        let r = state[pos]
            .rx
            .try_recv(&mut pending[pos], SOCKET_BATCH_SIZE, bc);
        match r {
            Ok(n) => {
                if n > 0 {
                    stats[pos].rx_packets += n;

                    state[pos].fq_deficit += n;
                } else {
                    if state[pos].fq.needs_wakeup() {
                        state[pos].rx.wake();
                    }
                }
            }
            Err(err) => {
                panic!("error: {:?}", err);
            }
        }

        //
        // Forward packets
        //
        let r = forward(&mut state[other].tx, &mut pending[pos]);
        match r {
            Ok(n) => stats[pos].tx_packets += n,
            Err(err) => println!("error: {:?}", err),
        }

        //
        // Fill buffers if required
        //
        if state[pos].fq_deficit >= FILL_THRESHOLD {
            let r = state[pos].fq.fill(&mut bufs, state[pos].fq_deficit);
            match r {
                Ok(n) => {
                    stats[pos].fq_bufs_filled += n;
                    state[pos].fq_deficit -= n;
                }
                Err(err) => panic!("error: {:?}", err),
            }
        }

        pos = (pos + 1) % 2;
    }
}
