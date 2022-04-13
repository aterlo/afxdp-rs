//
// Simple example that received frames from one link, parses the packet with Pnet and prints it.
//
// The link and associated channel are passed as command line args. The easiest way to direct all packets arriving
// at a link to a single channel is with ethtool -X.
//
use arraydeque::{ArrayDeque, Wrapping};
use rlimit::{setrlimit, Resource};
use std::cmp::min;
use structopt::StructOpt;

use afxdp::buf::Buf;
use afxdp::buf_mmap::BufMmap;
use afxdp::mmap_area::{MmapArea, MmapAreaOptions};
use afxdp::socket::{Socket, SocketOptions, SocketRx};
use afxdp::umem::{Umem, UmemFillQueue};
use afxdp::PENDING_LEN;
use libbpf_sys::{XSK_RING_CONS__DEFAULT_NUM_DESCS, XSK_RING_PROD__DEFAULT_NUM_DESCS};

use pnet::packet::ethernet::EthernetPacket;

// Parse the packet with Pnet and print
fn dump<'a>(
    input: &mut ArrayDeque<[BufMmap<'a, BufCustom>; PENDING_LEN], Wrapping>,
    buf_pool: &mut Vec<BufMmap<'a, BufCustom>>,
    count: &mut usize,
) {
    if input.is_empty() {
        return;
    }
    *count = *count + input.len();

    for buf in input.drain(0..) {
        let ethernet = EthernetPacket::new(buf.get_data());
        println!("{:?}", ethernet);

        buf_pool.push(buf);
    }
}

struct State<'a> {
    fq: UmemFillQueue<'a, BufCustom>,
    rx: SocketRx<'a, BufCustom>,
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
#[structopt(name = "dump")]
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

    assert!(setrlimit(Resource::MEMLOCK, rlimit::INFINITY, rlimit::INFINITY).is_ok());

    let options;
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

    assert!(bufs.len() == opt.bufnum);
    println!("Total buffers={} buffer size={}", opt.bufnum, opt.bufsize);

    let r = Umem::new(
        area.clone(),
        XSK_RING_CONS__DEFAULT_NUM_DESCS,
        XSK_RING_PROD__DEFAULT_NUM_DESCS,
    );
    let (umem1, _, mut umem1fq) = match r {
        Ok(umem) => umem,
        Err(err) => panic!("no umem for you: {:?}", err),
    };

    let mut options = SocketOptions::default();
    options.zero_copy_mode = opt.zero_copy;
    options.copy_mode = opt.copy;

    let r = Socket::new_rx(
        umem1.clone(),
        &opt.link_name,
        opt.link_channel,
        XSK_RING_CONS__DEFAULT_NUM_DESCS,
        options,
    );
    let (_, skt1rx) = match r {
        Ok(skt) => skt,
        Err(err) => panic!("no socket for you: {:?}", err),
    };

    // Fill the Umem
    let fill_size = min(XSK_RING_PROD__DEFAULT_NUM_DESCS as usize, opt.bufnum);
    println!("Adding {} buffers to the fill queue", fill_size);
    let r = umem1fq.fill(&mut bufs, fill_size);
    match r {
        Ok(n) => {
            if n != fill_size {
                panic!(
                    "Initial fill of umem incomplete. Wanted {} got {}.",
                    opt.bufnum, n
                );
            }
        }
        Err(err) => panic!("error: {:?}", err),
    }

    //
    // The loop
    //
    let mut v: ArrayDeque<[BufMmap<BufCustom>; PENDING_LEN], Wrapping> = ArrayDeque::new();

    let mut state = State {
        fq: umem1fq,
        rx: skt1rx,
        fq_deficit: 0,
    };

    let mut stats: Stats = Default::default();

    let custom = BufCustom {};
    loop {
        //
        // Receive
        //
        let r = state.rx.try_recv(&mut v, opt.batch_size, custom);
        match r {
            Ok(n) => {
                if n > 0 {
                    stats.rx_packets += n;
                    state.fq_deficit += n;
                } else if state.fq.needs_wakeup() {
                    state.rx.wake();
                }
            }
            Err(err) => {
                panic!("error: {:?}", err);
            }
        }

        //
        // Dump the packet headers
        //
        let mut count: usize = 0;
        dump(&mut v, &mut bufs, &mut count);

        if bufs.len() == 0 {
            println!("oops");
        }

        //
        // Fill buffers if required
        //
        const FILL_THRESHOLD: usize = 64;
        if state.fq_deficit >= FILL_THRESHOLD {
            let r = state.fq.fill(&mut bufs, state.fq_deficit);
            match r {
                Ok(n) => {
                    if n != FILL_THRESHOLD {
                        println!("under filled got {} wanted {}", n, FILL_THRESHOLD);
                    }
                    stats.fq_bufs += n;
                    state.fq_deficit -= n;
                }
                Err(err) => panic!("error: {:?}", err),
            }
        }
    }

    // Note this simple example doesn't have a clean shutdown to remove the XDP programs.
}
