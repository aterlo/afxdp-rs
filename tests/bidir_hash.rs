//
// Test that generates randomly sized packets with random contents. Each packet also contains a sequence
// number which is used validate ordering. The contents of each packet are verified with a hash.
// See the test runner (common.rs) for how this is called.
// Note that this test requires that veth is put into promiscuous mode so it passes the whole frame without looking for
// a valid Ethernet frame or MAC address.
//
// This test requires that the veth interfaces exist. This will be automated later.
// Run:
// ip link add veth1 type veth peer name veth2
// ip link set veth1 up
// ip link set veth2 up
// ip link set veth1 promisc on
// ip link set veth2 promisc on

// Note with veth on 5.10, it seems that any burst size greater then 16 results in the test failing
// because not all the packets are received. I think this is a bug in the needs_wakeup implementation
// of veth because if I call wake() in every loop iteration (vs. just when the needs_wakeup flag is set)
// the test passes. I plan to revisit that after I can test with 5.11.
//
// TODO: This test fails when the sockets drop packets (as seen in rx_dropped XSK socket stats). To
// fix this we need take those drops into account before calling this a pass/fail. How often this happens is
// hardware and CPU utilization dependent.
//
use core::panic;
use std::cmp::min;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::convert::TryInto;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Add;
use std::time;

use rand::Rng;

use arraydeque::{ArrayDeque, Wrapping};

use afxdp::buf::Buf;
use afxdp::buf_mmap::BufMmap;
use afxdp::PENDING_LEN;

mod common;
use common::run;
use common::BufCustom;
use common::TrafficTest;

#[derive(Debug)]
struct PacketState {
    processed: bool,
    hash: u64,
}

#[derive(Debug)]
struct PortState {
    last_time: time::Instant,

    transmitted: usize,
    received: usize,

    out_of_order: usize,
    bad_hash: usize,
    duplicated: usize,

    seq: u64,

    last_received_seq: u64,

    hashes: HashMap<u64, PacketState>,
}

#[derive(Debug)]
struct BiDirHash {
    end_time: time::Instant,
    done_time_with_padding: time::Instant,

    packet_gap: time::Duration,
    burst_size: usize,
    total_packets: usize,

    port_state: [PortState; 2],

    done: bool,
}

impl BiDirHash {
    fn new(
        max_duration: time::Duration,
        total_packets: usize,
        packet_gap: time::Duration,
        burst_size: usize,
    ) -> BiDirHash {
        let end_time = time::Instant::now().add(max_duration);
        let done_time_with_padding = end_time.add(time::Duration::from_secs(2));

        let port1 = PortState {
            last_time: time::Instant::now(),
            transmitted: 0,
            received: 0,
            out_of_order: 0,
            bad_hash: 0,
            duplicated: 0,
            seq: 1,
            last_received_seq: 0,
            hashes: HashMap::with_capacity(1_000_000),
        };

        let port2 = PortState {
            last_time: time::Instant::now(),
            transmitted: 0,
            received: 0,
            out_of_order: 0,
            bad_hash: 0,
            duplicated: 0,
            seq: 1,
            last_received_seq: 0,
            hashes: HashMap::with_capacity(1_000_000),
        };

        BiDirHash {
            end_time,
            done_time_with_padding,
            packet_gap,
            burst_size,
            total_packets,
            port_state: [port1, port2],
            done: false,
        }
    }
}

impl fmt::Display for BiDirHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "port0_transmitted={} port0_received={} port0_out_of_order={} port0_bad_hash={} port0_duplicated={} port1_transmitted={} port1_received={} port1_out_of_order={} port1_bad_hash={} port1_duplicated={}",
            self.port_state[0].transmitted,
            self.port_state[0].received,
            self.port_state[0].out_of_order,
            self.port_state[0].bad_hash,
            self.port_state[0].duplicated,
            self.port_state[1].transmitted,
            self.port_state[1].received,
            self.port_state[1].out_of_order,
            self.port_state[1].bad_hash,
            self.port_state[0].duplicated,
        )
    }
}

impl TrafficTest for BiDirHash {
    fn done(&mut self, now: time::Instant) -> bool {
        if self.end_time.lt(&now) {
            // Test exceeded max time
            return true;
        }

        if self.port_state[0].transmitted < self.total_packets
            || self.port_state[1].transmitted < self.total_packets
        {
            return false;
        }

        if !self.done {
            // Mark as done since both ports have transmitted their count
            self.done = true;
            self.done_time_with_padding = now.add(time::Duration::from_secs(2));

            return false;
        }

        if self.done_time_with_padding.lt(&now) {
            return true;
        }

        false
    }

    fn transmit<'a>(
        &mut self,
        now: time::Instant,
        port: usize, // 0 or 1
        bufs: &mut Vec<BufMmap<'a, BufCustom>>,
        pending: &mut ArrayDeque<[BufMmap<'a, BufCustom>; PENDING_LEN], Wrapping>,
    ) -> usize {
        let port = &mut self.port_state[port];

        if port.transmitted < self.total_packets && port.last_time.add(self.packet_gap).lt(&now) {
            // The most packets we can send is the space in the output buffer and the number of available
            // buffers in bufs.
            let mut packet_count = min(pending.capacity() - pending.len(), bufs.len());

            // Next check if we are at the end of the test with only a partial batch size left
            if self.total_packets - port.transmitted >= self.burst_size {
                if packet_count < self.burst_size {
                    // Don't have a full burst, don't do anything
                    return 0;
                } else {
                    packet_count = self.burst_size;
                }
            } else {
                // Partial batch size left, test is almost done
                packet_count = min(packet_count, self.total_packets - port.transmitted);
            }

            // Create and queue up the packets
            for _ in 0..packet_count {
                let mut buf = bufs.pop().unwrap();

                // Get a random number between 64 and 1500 for the size
                let mut rng = rand::thread_rng();
                let len = rng.gen_range(64..1500);
                buf.set_len(len);

                let data = buf.get_data_mut();

                // Set the first 8 bytes to the sequence number
                port.seq += 1;
                let bytes = port.seq.to_ne_bytes();
                data[0] = bytes[0];
                data[1] = bytes[1];
                data[2] = bytes[2];
                data[3] = bytes[3];
                data[4] = bytes[4];
                data[5] = bytes[5];
                data[6] = bytes[6];
                data[7] = bytes[7];

                // Fill the rest of the buffer with random bytes
                let r = rng.try_fill(&mut data[8..]);
                match r {
                    Ok(_) => {}
                    Err(err) => {
                        panic!("{:?}", err)
                    }
                }

                // Hash the filled buffer and store the value
                let mut hasher = DefaultHasher::new();
                buf.get_data()[0..len as usize].hash(&mut hasher);
                let last_hash = hasher.finish();

                let r = port.hashes.insert(
                    port.seq,
                    PacketState {
                        processed: false,
                        hash: last_hash,
                    },
                );
                match r {
                    None => {}
                    Some(_) => panic!("shouldn't happen"),
                }

                // Send it
                //println!("Sending: {} {:?}", buf, buf.get_data());
                let r = pending.push_back(buf);
                match r {
                    None => {}
                    Some(_) => panic!("shouldn't happen"),
                }
            }

            port.transmitted += packet_count;
            port.last_time = now;

            return packet_count;
        }

        0
    }

    fn receive<'a>(
        &mut self,
        _now: time::Instant,
        port: usize, // 0 or 1
        bufs: &mut Vec<BufMmap<'a, BufCustom>>,
        pending: &mut ArrayDeque<[BufMmap<'a, BufCustom>; PENDING_LEN], Wrapping>,
    ) -> usize {
        let mut received: usize = 0;

        let other_port = (port + 1) % 2;

        for _ in 0..pending.len() {
            let r = pending.pop_front();
            match r {
                Some(mut buf) => {
                    //println!("received: {}: {:?}", buf, buf.get_data());

                    let data = buf.get_data_mut();

                    // Extract the sequence number
                    let seq = u64::from_ne_bytes(data[0..8].try_into().unwrap());

                    if seq <= self.port_state[port].last_received_seq {
                        self.port_state[port].out_of_order += 1;
                    } else {
                        self.port_state[port].last_received_seq = seq
                    }

                    // Look up the sequence in the other port's map
                    let r = self.port_state[other_port].hashes.get(&seq);
                    match r {
                        Some(val) => {
                            let mut hasher = DefaultHasher::new();
                            buf.get_data()[0..buf.get_len() as usize].hash(&mut hasher);
                            let hash = hasher.finish();

                            if hash != val.hash {
                                self.port_state[port].bad_hash += 1;
                            }

                            if val.processed {
                                self.port_state[port].duplicated += 1;
                            }
                        }
                        None => {}
                    }

                    bufs.push(buf);
                    received += 1;
                }
                None => {
                    panic!("this shouldn't happen");
                }
            }
        }

        self.port_state[port].received += received;

        received
    }

    fn finalize(&self) {
        let mut failed: bool = false;

        error_check(
            "transmitted1",
            &mut failed,
            self.total_packets,
            self.port_state[0].transmitted,
        );

        error_check(
            "transmitted2",
            &mut failed,
            self.total_packets,
            self.port_state[1].transmitted,
        );

        error_check(
            "transmit-receive1",
            &mut failed,
            self.port_state[0].transmitted,
            self.port_state[1].received,
        );

        error_check(
            "transmit-receive2",
            &mut failed,
            self.port_state[1].transmitted,
            self.port_state[0].received,
        );

        error_check("bad_hash1", &mut failed, self.port_state[0].bad_hash, 0);

        error_check("bad_hash2", &mut failed, self.port_state[1].bad_hash, 0);

        error_check(
            "out_of_order1",
            &mut failed,
            self.port_state[0].out_of_order,
            0,
        );

        error_check(
            "out_of_order2",
            &mut failed,
            self.port_state[1].out_of_order,
            0,
        );

        error_check("duplicated1", &mut failed, self.port_state[0].duplicated, 0);

        error_check("duplicated2", &mut failed, self.port_state[1].duplicated, 0);

        if failed {
            println!("{}", self);
            panic!("failed");
        }
    }
}

fn error_check(name: &str, failed: &mut bool, val1: usize, val2: usize) {
    if val1 != val2 {
        *failed = true;

        println!("{} -- {} {} -- {}", name, val1, val2, val1 - val2,);
    }
}

#[test]
fn bi_dir_hash1() {
    let test = BiDirHash::new(
        time::Duration::from_secs(30),
        16 * (2 << 17),
        time::Duration::from_micros(100),
        16,
    );

    // let test = BiDirHash::new(
    //     time::Duration::from_secs(60),
    //     8000199,
    //     time::Duration::from_micros(100),
    //     16,
    // );

    run(test);
}
