//
// Simple example that forwards frames directly between two links
//
// This example pulls packets from one channel (ethtool -l) from each link. The channel ID is passed via the command
// line args. The easiest way to direct all packets received on a link to a single channel is with:
//     ethtool -X
//
use arraydeque::{ArrayDeque, Wrapping};
use rlimit::{setrlimit, Resource, Rlim};
use std::cmp::min;
use structopt::StructOpt;

use afxdp::buf_mmap::BufMmap;
use afxdp::mmap_area::{MmapArea, MmapAreaOptions, MmapError};
use afxdp::socket::{Socket, SocketOptions, SocketRx, SocketTx};
use afxdp::umem::{Umem, UmemCompletionQueue, UmemFillQueue};
use afxdp::PENDING_LEN;

const RING_SIZE: u32 = 2048;
const SOCKET_BATCH_SIZE: usize = 64;
const SERVICE_BATCH_SIZE: usize = 64;
const FILL_THRESHOLD: usize = 64;

fn forward(
    tx: &mut SocketTx<BufCustom>,
    bufs: &mut ArrayDeque<[BufMmap<BufCustom>; PENDING_LEN], Wrapping>,
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
    /// Default buffer size
    #[structopt(long, default_value = "2048")]
    bufsize: usize,

    /// Number of buffers
    #[structopt(long, default_value = "65536")]
    bufnum: usize,

    /// First link name
    #[structopt(long)]
    link1_name: std::string::String,

    /// First link channel
    #[structopt(long, default_value = "0")]
    link1_channel: usize,

    /// Second link name
    #[structopt(long)]
    link2_name: std::string::String,

    /// Second link channel
    #[structopt(long, default_value = "0")]
    link2_channel: usize,

    /// Use HUGE TLB
    #[structopt(long)]
    huge_tlb: bool,

    /// Zero copy mode
    #[structopt(long)]
    zero_copy: bool,

    /// Copy mode
    #[structopt(long, conflicts_with = "zero-copy")]
    copy: bool,
}

fn main() {
    let opt = Opt::from_args();

    let initial_fill_num = min(opt.bufnum / 2, RING_SIZE as usize);

    assert!(setrlimit(Resource::MEMLOCK, Rlim::INFINITY, Rlim::INFINITY).is_ok());

    let options: MmapAreaOptions;
    if opt.huge_tlb {
        options = MmapAreaOptions { huge_tlb: true };
    } else {
        options = MmapAreaOptions { huge_tlb: false };
    }
    let r: Result<(std::sync::Arc<MmapArea<BufCustom>>, Vec<BufMmap<BufCustom>>), MmapError> =
        MmapArea::new(opt.bufnum, opt.bufsize, options);
    let (area, mut bufs) = match r {
        Ok((area, bufs)) => (area, bufs),
        Err(err) => panic!("Failed to create MmapArea: {:?}", err),
    };
    println!(
        "Created MmapArea with {} buffers of size {}",
        bufs.len(),
        opt.bufsize
    );

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
    // umem1
    //
    println!("Filling umem1 with {} buffers", initial_fill_num);
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
    println!("Filling umem2 with {} buffers", initial_fill_num);
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
    let mut pending: [ArrayDeque<[BufMmap<BufCustom>; PENDING_LEN], Wrapping>; 2] =
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
    //let mut last_rx_packets = 0;
    loop {
        let other = (pos + 1) % 2;

        /*
        if stats[pos].rx_packets > last_rx_packets + 1000000 {
            println!("RX Packets: {} {}", stats[pos].rx_packets, last_rx_packets);
            last_rx_packets = stats[pos].rx_packets;
        }
        */

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
            Ok(n) => stats[other].tx_packets += n,
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

    // Note this simple program doesn't have a clean shutdown to remove the XDP programs.
}
