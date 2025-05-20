use std::thread::available_parallelism;

use colorless::Stackify;
use colorless_executor::{Executor, ExecutorConfig, block_on};
use futures_lite::{StreamExt, future::yield_now};

#[test]
fn simple() {
    Executor::build_scoped(
        ExecutorConfig::default(),
        |tb| tb.run(),
        |exec| {
            let t1 = exec.spawn(|| yield_now().await_().unwrap());
            let t2 = exec.spawn(|| yield_now().await_().unwrap());
            block_on(t1);
            block_on(t2);
        },
    )
    .unwrap();
}

#[test]
fn broadcast() {
    Executor::build_scoped(
        ExecutorConfig::default(),
        |tb| tb.run(),
        |exec| {
            let bt = exec.broadcast(|| |_| yield_now().await_().unwrap());
            let finished = block_on(bt.count());
            assert_eq!(finished, available_parallelism().unwrap().get());
        },
    )
    .unwrap();
}
