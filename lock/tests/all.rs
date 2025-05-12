use std::sync::Arc;

use colorless_executor::{Executor, ExecutorConfig, block_on};
use colorless_lock::Mutex;

#[test]
fn count() {
    let counter = Arc::new(Mutex::new(0));
    Executor::build_scoped(
        ExecutorConfig::default(),
        |tb| tb.run(),
        |exec| {
            let tasks: Vec<_> = (0..16)
                .into_iter()
                .map(|_| {
                    let counter = Arc::clone(&counter);
                    exec.spawn(move || {
                        for _ in 0..10_000 {
                            *counter.lock() += 1;
                        }
                    })
                })
                .collect();
            for task in tasks {
                block_on(task);
            }
            assert_eq!(*counter.lock(), 160_000);
        },
    )
    .unwrap();
}
