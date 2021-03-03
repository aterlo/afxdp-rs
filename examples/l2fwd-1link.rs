//
// Simple example that received frames from one link, swaps the MAC addresses and sends the packets back out
// the same link.
//
// The link and associated channel are passed as command line args. The easiest way to direct all packets arriving
// at a link to a single channel is with ethtool -X.
//
use arraydeque::{ArrayDeque, Wrapping};
use crossbeam_channel::{bounded, select, Receiver, Sender};
use rlimit::{setrlimit, Resource, Rlim};
use std::cmp::min;
use std::thread;
use std::time::{Duration, Instant};
use structopt::StructOpt;

use afxdp::buf::Buf;
use afxdp::buf_mmap::BufMmap;
use afxdp::mmap_area::{MmapArea, MmapAreaOptions};
use afxdp::socket::{Socket, SocketOptions, SocketRx, SocketTx};
use afxdp::umem::{Umem, UmemCompletionQueue, UmemFillQueue};
use afxdp::PENDING_LEN;
use libbpf_sys::{XSK_RING_CONS__DEFAULT_NUM_DESCS, XSK_RING_PROD__DEFAULT_NUM_DESCS};

fn swap_macs(bufs: &mut ArrayDeque<[BufMmap<BufCustom>; PENDING_LEN], Wrapping>) -> Result<(), ()> {
    let mut tmp1: [u8; 12] = Default::default();

    for buf in bufs {
        let data = buf.get_data_mut();

        tmp1.copy_from_slice(&mut data[0..12]);

        data[0..6].copy_from_slice(&tmp1[6..12]);
        data[6..12].copy_from_slice(&tmp1[0..6]);
    }

    Ok(())
}

fn forward(
    tx: &mut SocketTx<BufCustom>,
    bufs: &mut ArrayDeque<[BufMmap<BufCustom>; PENDING_LEN], Wrapping>,
    batch_size: usize,
) -> Result<usize, ()> {
    if bufs.is_empty() {
        return Ok(0);
    }

    let r = tx.try_send(bufs, batch_size);
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
    /// Default buffer size
    #[structopt(long, default_value = "2048")]
    bufsize: usize,

    /// How many buffers
    #[structopt(long, default_value = "65536")]
    bufnum: usize,

    /// Batch size
    #[structopt(long, default_value = "64")]
    batch_size: usize,

    /// The link to attach to
    #[structopt(long)]
    link_name: std::string::String,

    /// Link channel
    #[structopt(long, default_value = "0")]
    link_channel: usize,

    /// Use HUGE TLB
    #[structopt(long)]
    huge_tlb: bool,

    /// Use zero copy mode
    #[structopt(long)]
    zero_copy: bool,

    /// Copy mode
    #[structopt(long, conflicts_with = "zero-copy")]
    copy: bool,
}

fn main() {
    let opt = Opt::from_args();

    assert!(setrlimit(Resource::MEMLOCK, Rlim::INFINITY, Rlim::INFINITY).is_ok());

    let options: MmapAreaOptions;
    if opt.huge_tlb {
        options = MmapAreaOptions { huge_tlb: true };
    } else {
        options = MmapAreaOptions { huge_tlb: false };
    }
    let r = MmapArea::new(opt.bufnum, opt.bufsize, options);
    let (area, mut bufs) = match r {
        Ok((area, bufs)) => (area, bufs),
        Err(err) => panic!("no mmap for you: {:?}", err),
    };

    let r = Umem::new(
        area.clone(),
        XSK_RING_CONS__DEFAULT_NUM_DESCS,
        XSK_RING_PROD__DEFAULT_NUM_DESCS,
    );
    let (umem1, umem1cq, mut umem1fq) = match r {
        Ok(umem) => umem,
        Err(err) => panic!("no umem for you: {:?}", err),
    };

    let mut options = SocketOptions::default();
    options.zero_copy_mode = opt.zero_copy;
    options.copy_mode = opt.copy;

    let r = Socket::new(
        umem1.clone(),
        &opt.link_name,
        opt.link_channel,
        XSK_RING_CONS__DEFAULT_NUM_DESCS,
        XSK_RING_PROD__DEFAULT_NUM_DESCS,
        options,
    );
    let (_skt1, skt1rx, skt1tx) = match r {
        Ok(skt) => skt,
        Err(err) => panic!("no socket for you: {:?}", err),
    };

    // Fill the Umem
    let r = umem1fq.fill(
        &mut bufs,
        min(XSK_RING_PROD__DEFAULT_NUM_DESCS as usize, opt.bufnum),
    );
    match r {
        Ok(n) => {
            if n != min(XSK_RING_PROD__DEFAULT_NUM_DESCS as usize, opt.bufnum) {
                panic!(
                    "Initial fill of umem incomplete. Wanted {} got {}.",
                    opt.bufnum, n
                );
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

    let mut v: ArrayDeque<[BufMmap<BufCustom>; PENDING_LEN], Wrapping> = ArrayDeque::new();

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

            if ticks > 300 {
                break;
            }

            ticks += 1;

            last = now;
        }

        //
        // Service completion queue
        //
        let r = state.cq.service(&mut bufs, opt.batch_size);
        match r {
            Ok(n) => {
                stats.cq_bufs += n;
            }
            Err(err) => panic!("error: {:?}", err),
        }

        //
        // Receive
        //
        let r = state.rx.try_recv(&mut v, opt.batch_size, custom);
        match r {
            Ok(n) => {
                if n > 0 {
                    stats.rx_packets += n;

                    let r = swap_macs(&mut v);
                    match r {
                        Ok(_) => {}
                        Err(_) => println!("error"),
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
        // Forward
        //
        let r = forward(&mut state.tx, &mut v, opt.batch_size);
        match r {
            Ok(n) => stats.tx_packets += n,
            Err(err) => println!("error: {:?}", err),
        }

        //
        // Fill buffers if required
        //
        if state.fq_deficit > 0 {
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

    // Note this simple program doesn't have a clean shutdown to remove the XDP programs.
}
