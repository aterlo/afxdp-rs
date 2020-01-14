# AFXDP-rs

This module provides a Rust interface for AF_XDP built on libbpf-sys (https://github.com/alexforster/libbpf-sys).

AF_XDP:

* <https://www.kernel.org/doc/html/latest/networking/af_xdp.html>
* <https://lwn.net/Articles/750845/>

The goals of this crate, in order, are:

1. Correctness
2. Performance
3. Ease of use

## Current Status

The API works for my current use cases but I expect it will change to achieve higher performance and usability.

If you have knowledge of Rust FFI and general Rust unsafe things I would greatly appreciate some help auditing this crate as my experience in this area is limited.

## AF_XDP Features

* HUGE TLB: At present this module always uses HUGETLB to map the memory.
* ZEROCOPY: At present this module always passes the ZEROCOPY flag which only works with a small number of drivers.
* Only the chunked memory mode is supported.

## To Do

* Currently this module is not 'safe' because Bufs can outlive the memory pool they are associated with. I believe fixing this will require adding an Arc to each Buf. I have not had the time yet to determine the performance impact of this and would appreciate any other ideas.
* Allow more configurability of AF XDP features.
