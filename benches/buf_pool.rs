use afxdp::buf_pool::BufPool;
use afxdp::buf_pool_vec::BufPoolVec;
use afxdp::buf_vec::BufVec;

use criterion::{criterion_group, criterion_main, Criterion};

const NUM: usize = 10000;

#[derive(Default, Copy, Clone, Debug)]
struct BufCustom {}

fn getput<'a>(
    bufpool: &mut BufPoolVec<BufVec<BufCustom>, BufCustom>,
    bufs: &mut Vec<BufVec<BufCustom>>,
) {
    let _r = bufpool.get(bufs, NUM);
    let _r = bufpool.put(bufs, NUM);
}

fn test(c: &mut Criterion) {
    let mut bufs = Vec::new();
    for _ in 0..NUM {
        let buf: BufVec<BufCustom> = BufVec::new(NUM, BufCustom {});
        bufs.push(buf);
    }

    let mut bufpool = BufPoolVec::new(bufs.len());
    let len = bufs.len();
    bufpool.put(&mut bufs, len);

    c.bench_function("getput", |b| b.iter(|| getput(&mut bufpool, &mut bufs)));
}

criterion_group!(benches, test);
criterion_main!(benches);
