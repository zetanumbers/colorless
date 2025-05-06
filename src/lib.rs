use std::{
    cell::{Cell, UnsafeCell},
    pin::{Pin, pin},
    task,
};

use corosensei::CoroutineResult;
#[cfg(feature = "tlv")]
use tlv::Tlv;

#[cfg(feature = "tlv")]
pub mod tlv;

struct TaskContext(*mut ());

impl TaskContext {
    fn new(cx: &mut task::Context<'_>) -> Self {
        TaskContext(cx as *mut _ as *mut ())
    }
}

struct Yielder {
    inner: *const corosensei::Yielder<TaskContext, ()>,
    context: TaskContext,
    old_cx: *mut Option<Yielder>,
    #[cfg(feature = "tlv")]
    old_tlv: Tlv,
}

impl Yielder {
    fn cx(&mut self) -> &mut task::Context<'_> {
        unsafe { &mut *(self.context.0 as *mut task::Context<'_>) }
    }
}

thread_local! {
    static CONTEXT: Cell<Option<Yielder>> = const { Cell::new(None) };
}

pub struct Coroutine<R: 'static> {
    inner: corosensei::Coroutine<TaskContext, (), R>,
}

impl<R: 'static> Coroutine<R> {
    pub fn new<F>(f: F) -> Self
    where
        F: FnOnce() -> R + 'static,
        R: 'static,
    {
        #[cfg(feature = "tlv")]
        let tlv = tlv::get();
        Coroutine {
            inner: corosensei::Coroutine::new(move |inner, context| {
                #[cfg(feature = "tlv")]
                let old_tlv = tlv::get();
                let old_cx = UnsafeCell::new(None);
                // SAFETY: This scope makes sure `old_cx` variable stays valid
                {
                    unsafe {
                        *old_cx.get() = CONTEXT.replace(Some(Yielder {
                            inner,
                            context,
                            old_cx: old_cx.get(),
                            #[cfg(feature = "tlv")]
                            old_tlv,
                        }));
                    }
                    #[cfg(feature = "tlv")]
                    tlv::set(tlv);

                    struct ContextGuard {}

                    impl Drop for ContextGuard {
                        fn drop(&mut self) {
                            CONTEXT.with(|context| {
                                let cx = context.take().unwrap();
                                #[cfg(feature = "tlv")]
                                tlv::set(cx.old_tlv);
                                unsafe { context.set((*cx.old_cx).take()) };
                            });
                        }
                    }

                    let _cx_guard = ContextGuard {};
                    f()
                }
            }),
        }
    }
}

impl<R: 'static> Future for Coroutine<R> {
    type Output = R;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<R> {
        let res = self.get_mut().inner.resume(TaskContext::new(cx));
        match res {
            CoroutineResult::Yield(()) => task::Poll::Pending,
            CoroutineResult::Return(r) => task::Poll::Ready(r),
        }
    }
}

pub trait Stackify: IntoFuture {
    fn await_(self) -> Self::Output;
}

impl<F: IntoFuture> Stackify for F {
    fn await_(self) -> Self::Output {
        let mut fut = pin!(self.into_future());

        let Some(mut cx) = CONTEXT.take() else {
            panic!("Outside of any await context");
        };
        #[cfg(feature = "tlv")]
        let context_tlv = tlv::get();

        loop {
            match fut.as_mut().poll(cx.cx()) {
                task::Poll::Pending => unsafe {
                    CONTEXT.set((*cx.old_cx).take());

                    #[cfg(feature = "tlv")]
                    tlv::set(cx.old_tlv);

                    cx.context = (*cx.inner).suspend(());

                    #[cfg(feature = "tlv")]
                    {
                        cx.old_tlv = tlv::get();
                        tlv::set(context_tlv);
                    }
                    CONTEXT.with(|context| context.swap(Cell::from_mut(&mut *cx.old_cx)));
                },
                task::Poll::Ready(r) => {
                    CONTEXT.set(Some(cx));
                    return r;
                }
            }
        }
    }
}

pub fn sync_into_coroutine<F, R>(f: F) -> SyncIntoCoroutine<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    SyncIntoCoroutine(Coroutine::new(f))
}

pub struct SyncIntoCoroutine<R: 'static>(Coroutine<R>);

impl<R: 'static> IntoFuture for SyncIntoCoroutine<R> {
    type Output = R;
    type IntoFuture = Coroutine<R>;

    fn into_future(self) -> Self::IntoFuture {
        self.0
    }
}

unsafe impl<R: Send + 'static> Send for SyncIntoCoroutine<R> {}
unsafe impl<R: 'static> Sync for SyncIntoCoroutine<R> {}
