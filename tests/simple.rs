use colorless::{Coroutine, Stackify};
use futures_lite::future::{block_on, yield_now, zip};

#[test]
fn trivial() {
    block_on(Coroutine::new(|| {}))
}

#[test]
fn nested() {
    block_on(Coroutine::new(|| Coroutine::new(|| ()).await_().unwrap()))
}

#[test]
fn simple() {
    block_on(Coroutine::new(|| {
        yield_now().await_().unwrap();
        Coroutine::new(|| yield_now().await_().unwrap())
            .await_()
            .unwrap();
        yield_now().await_().unwrap();
    }));
}

#[test]
fn zip_two() {
    block_on(zip(
        Coroutine::new(|| {
            yield_now().await_().unwrap();
            Coroutine::new(|| yield_now().await_().unwrap())
                .await_()
                .unwrap();
            yield_now().await_().unwrap();
            yield_now().await_().unwrap();
        }),
        Coroutine::new(|| {
            yield_now().await_().unwrap();
            yield_now().await_().unwrap();
            Coroutine::new(|| yield_now().await_().unwrap())
                .await_()
                .unwrap();
            yield_now().await_().unwrap();
        }),
    ));
}
