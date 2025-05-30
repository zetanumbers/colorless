use std::{
    cell::{Cell, UnsafeCell},
    fmt,
    pin::{Pin, pin},
    task,
};

use corosensei::{CoroutineResult, stack::DefaultStack};

pub use corosensei::stack;

use pin_project_lite::pin_project;
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

pin_project! {
    pub struct Coroutine<R, Stack = stack::DefaultStack>
    where
        R: 'static,
        Stack: stack::Stack,
    {
        inner: corosensei::Coroutine<TaskContext, (), R, Stack>,
    }
}

impl<R: 'static> Coroutine<R> {
    pub fn new<F>(f: F) -> Self
    where
        F: FnOnce() -> R + 'static,
        R: 'static,
    {
        Self::with_stack(DefaultStack::default(), f)
    }
}

impl<R: 'static, Stack: stack::Stack> Coroutine<R, Stack> {
    pub fn with_stack<F>(stack: Stack, f: F) -> Self
    where
        F: FnOnce() -> R + 'static,
        R: 'static,
    {
        #[cfg(feature = "tlv")]
        let tlv = tlv::get();
        Coroutine {
            inner: corosensei::Coroutine::with_stack(stack, move |inner, context| {
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

    pub fn into_stack(self) -> Stack {
        self.inner.into_stack()
    }
}

impl<R: 'static, Stack: stack::Stack> Future for Coroutine<R, Stack> {
    type Output = R;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<R> {
        let res = self.project().inner.resume(TaskContext::new(cx));
        match res {
            CoroutineResult::Yield(()) => task::Poll::Pending,
            CoroutineResult::Return(r) => task::Poll::Ready(r),
        }
    }
}

pub trait Stackify: IntoFuture {
    fn await_(self) -> Result<Self::Output, OutsideOfContextError>;
}

impl<F: IntoFuture> Stackify for F {
    fn await_(self) -> Result<Self::Output, OutsideOfContextError> {
        let mut fut = pin!(self.into_future());

        let mut cx = CONTEXT.take().ok_or(OutsideOfContextError(()))?;
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
                    return Ok(r);
                }
            }
        }
    }
}

pub fn inside_context() -> bool {
    let cx = CONTEXT.take();
    let check = cx.is_some();
    CONTEXT.set(cx);
    check
}

#[derive(Debug)]
pub struct OutsideOfContextError(());

impl fmt::Display for OutsideOfContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "outside of any await context".fmt(f)
    }
}

pub fn sync_into_coroutine<F, R>(f: F) -> SyncIntoCoroutine<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    SyncIntoCoroutine(Coroutine::new(f))
}

pub fn sync_into_coroutine_with_stack<F, R, Stack>(
    stack: Stack,
    f: F,
) -> SyncIntoCoroutine<R, Stack>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
    Stack: stack::Stack,
{
    SyncIntoCoroutine(Coroutine::with_stack(stack, f))
}

pub struct SyncIntoCoroutine<R, Stack = stack::DefaultStack>(Coroutine<R, Stack>)
where
    R: 'static,
    Stack: stack::Stack;

impl<R: 'static, Stack: stack::Stack> IntoFuture for SyncIntoCoroutine<R, Stack> {
    type Output = R;
    type IntoFuture = Coroutine<R, Stack>;

    fn into_future(self) -> Self::IntoFuture {
        self.0
    }
}

// SAFETY: coroutine is not yet run, so it couldn't have stored any `!Send` object yet
unsafe impl<R: Send + 'static> Send for SyncIntoCoroutine<R> {}
unsafe impl<R: 'static> Sync for SyncIntoCoroutine<R> {}
