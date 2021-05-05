use afxdp::buf_pool::BufPool;
use afxdp::buf_pool_vec::BufPoolVec;
use afxdp::buf_vec::BufVec;

use criterion::{criterion_group, criterion_main, Criterion};

#[derive(Default, Copy, Clone, Debug)]
struct BufCustom {}

fn getput<'a>(
    bufpool: &mut BufPoolVec<BufVec<BufCustom>, BufCustom>,
    bufs: &mut Vec<BufVec<BufCustom>>,
    num: usize,
) {
    let _r = bufpool.get(bufs, num);
    let _r = bufpool.put(bufs, num);
}

fn test_getput1(c: &mut Criterion) {
    const NUM: usize = 10000;
    let mut bufs = Vec::new();
    for _ in 0..NUM {
        let buf: BufVec<BufCustom> = BufVec::new(NUM, BufCustom {});
        bufs.push(buf);
    }

    let mut bufpool = BufPoolVec::new(bufs.len());
    let len = bufs.len();
    bufpool.put(&mut bufs, len);

    c.bench_function("getput1", |b| {
        b.iter(|| getput(&mut bufpool, &mut bufs, NUM))
    });
}

fn test_getput2(c: &mut Criterion) {
    const NUM: usize = 50000;
    let mut bufs = Vec::new();
    for _ in 0..NUM {
        let buf: BufVec<BufCustom> = BufVec::new(NUM, BufCustom {});
        bufs.push(buf);
    }

    let mut bufpool = BufPoolVec::new(bufs.len());
    let len = bufs.len();
    bufpool.put(&mut bufs, len);

    c.bench_function("getput2", |b| {
        b.iter(|| getput(&mut bufpool, &mut bufs, NUM))
    });
}

criterion_group!(benches, test_getput1, test_getput2);
criterion_main!(benches);
