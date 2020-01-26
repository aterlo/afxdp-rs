//
// Simple example that received frames from one link, swaps the MAC addresses and sends the packets back out
// the same link.
//
// The link and associated channel are passed as command line args. The easiest way to direct all packets arriving
// at a link to a single channel is with ethtool -X.
//
use arraydeque::{ArrayDeque, Wrapping};
use crossbeam_channel::{bounded, select, Receiver, Sender};
use rlimit::{setrlimit, Resource, RLIM_INFINITY};
use std::thread;
use std::time::{Duration, Instant};
use structopt::StructOpt;

use afxdp::buf::Buf;
use afxdp::mmaparea::{MmapArea, MmapAreaOptions};
use afxdp::socket::{Socket, SocketRx, SocketTx};
use afxdp::umem::{Umem, UmemCompletionQueue, UmemFillQueue};
use afxdp::PENDING_LEN;

const RING_SIZE: u32 = 4096;
const BATCH_SIZE: usize = 1024;
const SERVICE_BATCH_SIZE: usize = 1024;
const FILL_THRESHOLD: usize = 512;

fn swap_macs(bufs: &mut ArrayDeque<[Buf<BufCustom>; PENDING_LEN], Wrapping>) -> Result<(), ()> {
    let mut tmp1: [u8; 6] = Default::default();
    let mut tmp2: [u8; 6] = Default::default();

    for buf in bufs {
        tmp1.copy_from_slice(&buf.data[0..6]);
        tmp2.copy_from_slice(&buf.data[6..12]);

        buf.data[0..6].copy_from_slice(&tmp2);
        buf.data[6..12].copy_from_slice(&tmp1);
    }

    Ok(())
}

fn forward(
    tx: &mut SocketTx<BufCustom>,
    bufs: &mut ArrayDeque<[Buf<BufCustom>; PENDING_LEN], Wrapping>,
) -> Result<usize, ()> {
    let r = tx.try_send(bufs, BATCH_SIZE);
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

#[derive(Default, Debug, Clone, Copy)]
struct Stats {
    cq_bufs: usize,
    fq_bufs: usize,
    rx_packets: usize,
    tx_packets: usize,
}

#[derive(Default, Copy, Clone)]
struct BufCustom {}

#[derive(StructOpt, Debug)]
#[structopt(name = "l2fwd-1link")]
struct Opt {
    #[structopt(long, default_value = "2048")]
    bufsize: usize,

    #[structopt(long, default_value = "4096")]
    bufnum: usize,

    #[structopt(long, default_value = "none")]
    link_name: std::string::String,

    #[structopt(long, default_value = "0")]
    link_channel: usize,

    #[structopt(long)]
    hugetlb: bool,
}

fn main() {
    let opt = Opt::from_args();

    if opt.link_name == "none" {
        println!("Link name parameter is required");
        return;
    }

    assert!(setrlimit(Resource::MEMLOCK, RLIM_INFINITY, RLIM_INFINITY).is_ok());

    let options: MmapAreaOptions;
    if opt.hugetlb {
        options = MmapAreaOptions { huge_tlb: true };
    } else {
        options = MmapAreaOptions { huge_tlb: false };
    }
    let r = MmapArea::new(opt.bufnum, opt.bufsize, options);
    let (area, buf_pool) = match r {
        Ok((area, buf_pool)) => (area, buf_pool),
        Err(err) => panic!("no mmap for you: {:?}", err),
    };

    let r = Umem::new(area.clone(), RING_SIZE, RING_SIZE);
    let (umem1, umem1cq, mut umem1fq) = match r {
        Ok(umem) => umem,
        Err(err) => panic!("no umem for you: {:?}", err),
    };

    let r = Socket::new(
        umem1.clone(),
        &opt.link_name,
        opt.link_channel,
        RING_SIZE,
        RING_SIZE,
    );
    let (_skt1, skt1rx, skt1tx) = match r {
        Ok(skt) => skt,
        Err(err) => panic!("no socket for you: {:?}", err),
    };

    // Create a local buf pool and get bufs from the global pool. Since there are no other users of the pool, grab
    // all the bufs.
    let mut bufs: Vec<Buf<BufCustom>> = Vec::with_capacity(opt.bufnum);
    let r = buf_pool.lock().unwrap().get(&mut bufs, opt.bufnum);
    match r {
        Ok(n) => {
            if n != opt.bufnum {
                panic!("failed to get initial bufs {} {}", n, opt.bufnum,);
            }
        }
        Err(err) => panic!("error: {:?}", err),
    }

    let r = umem1fq.fill(&mut bufs, opt.bufnum);
    match r {
        Ok(n) => {
            if n != opt.bufnum {
                panic!("fill incomplete");
            }
        }
        Err(err) => panic!("error: {:?}", err),
    }

    //
    // Start a thread to print stats
    //
    let (sender, receiver): (Sender<Stats>, Receiver<Stats>) = bounded(100);

    thread::spawn(move || loop {
        select! {
            recv(receiver) -> msg => {
                match msg {
                    Ok(s) => {println!("{:?}", s)}
                    Err(_) => { break; }
                }
            }
        }
    });

    //
    // The loop
    //

    let mut v: ArrayDeque<[Buf<BufCustom>; PENDING_LEN], Wrapping> = ArrayDeque::new();

    let mut state = State {
        cq: umem1cq,
        fq: umem1fq,
        rx: skt1rx,
        tx: skt1tx,
        fq_deficit: 0,
    };

    let mut stats: Stats = Default::default();

    let mut now;
    let mut last = Instant::now();
    let mut ticks: usize = 0;
    let custom = BufCustom {};
    loop {
        now = Instant::now();
        if now.duration_since(last) > Duration::from_secs(1) {
            let r = sender.send(stats);
            match r {
                Ok(_) => {}
                Err(err) => {
                    println!("error: {:?}", err);
                }
            }

            if ticks > 60 {
                break;
            }

            ticks += 1;

            last = now;
        }

        //
        // Service completion queue
        //
        let r = state.cq.service(&mut bufs, SERVICE_BATCH_SIZE);
        match r {
            Ok(n) => {
                stats.cq_bufs += n;
            }
            Err(err) => panic!("error: {:?}", err),
        }

        //
        // Receive
        //
        let r = state.rx.try_recv(&mut v, BATCH_SIZE, custom);
        match r {
            Ok(n) => {
                if n > 0 {
                    stats.rx_packets += n;

                    let r = swap_macs(&mut v);
                    match r {
                        Ok(_) => {}
                        Err(_) => println!("error"),
                    }

                    let r = forward(&mut state.tx, &mut v);
                    match r {
                        Ok(n) => stats.tx_packets += n,
                        Err(err) => println!("error: {:?}", err),
                    }

                    state.fq_deficit += n;
                } else {
                    if state.fq.needs_wakeup() {
                        state.rx.wake();
                    }
                }
            }
            Err(err) => {
                panic!("error: {:?}", err);
            }
        }

        //
        // Fill buffers if required
        //
        if state.fq_deficit > FILL_THRESHOLD {
            let r = state.fq.fill(&mut bufs, state.fq_deficit);
            match r {
                Ok(n) => {
                    stats.fq_bufs += n;
                    state.fq_deficit -= n;
                }
                Err(err) => panic!("error: {:?}", err),
            }
        }
    }
}
