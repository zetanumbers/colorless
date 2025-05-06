use colorless::{Coroutine, Stackify};
use futures_lite::future::{block_on, yield_now};

#[test]
fn trivial() {
    block_on(Coroutine::new(|| {}))
}

#[test]
fn nested() {
    block_on(Coroutine::new(|| Coroutine::new(|| ()).await_()))
}

#[test]
fn simple() {
    block_on(Coroutine::new(|| {
        yield_now().await_();
        Coroutine::new(|| yield_now().await_()).await_();
        yield_now().await_();
    }))
}
