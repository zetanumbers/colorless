use std::sync::Arc;

use colorless::Stackify;
use colorless_executor::{Executor, ExecutorConfig, block_on, spawn};
use colorless_lock::Mutex;

#[test]
fn count() {
    let counter = Arc::new(Mutex::new(0));
    Executor::build_scoped(
        ExecutorConfig::default(),
        |tb| tb.run(),
        |exec| {
            block_on(exec.spawn(move || {
                let tasks: Vec<_> = (0..16)
                    .into_iter()
                    .map(|_| {
                        let counter = Arc::clone(&counter);
                        spawn(move || {
                            for _ in 0..10_000 {
                                *counter.lock() += 1;
                            }
                        })
                    })
                    .collect();
                for task in tasks {
                    task.await_();
                }
                assert_eq!(*counter.lock(), 160_000);
            }))
        },
    )
    .unwrap();
}
