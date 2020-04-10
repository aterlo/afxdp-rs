use afxdp::buf::Buf;
use afxdp::bufpool::BufPool;
use afxdp::mmaparea::{MmapArea, MmapAreaOptions, MmapError};
use std::sync::{Arc, Mutex};

use criterion::{criterion_group, criterion_main, Criterion};

const NUM: usize = 10000;

#[derive(Default, Copy, Clone, Debug)]
struct BufCustom {}

fn getput<'a>(
    bufpool: &mut Arc<Mutex<BufPool<'a, BufCustom>>>,
    bufs: &mut Vec<Buf<'a, BufCustom>>,
) {
    let _r = bufpool.lock().unwrap().get(bufs, NUM);
    let _r = bufpool.lock().unwrap().put(bufs, NUM);
}

fn test(c: &mut Criterion) {
    let options = MmapAreaOptions { huge_tlb: false };
    let r: Result<(Arc<MmapArea<BufCustom>>, Arc<Mutex<BufPool<BufCustom>>>), MmapError> =
        MmapArea::new(NUM, 128, options);

    let (_area, buf_pool) = match r {
        Ok((area, buf_pool)) => (area, buf_pool),
        Err(err) => panic!("{:?}", err),
    };

    let mut bufs = Vec::with_capacity(NUM);

    c.bench_function("getput", |b| {
        b.iter(|| getput(&mut buf_pool.clone(), &mut bufs))
    });
}

criterion_group!(benches, test);
criterion_main!(benches);
