use colorless::Stackify;
use colorless_executor::{Executor, ExecutorConfig, block_on, spawn};
use futures_lite::future::yield_now;

#[test]
fn simple() {
    Executor::build_scoped(
        ExecutorConfig::default(),
        |tb| tb.run(),
        |exec| {
            block_on(exec.spawn(|| {
                let t1 = spawn(|| yield_now().await_());
                let t2 = spawn(|| yield_now().await_());
                t1.await_();
                t2.await_();
            }))
        },
    )
    .unwrap();
}
