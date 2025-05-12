use std::{
    cell::Cell,
    io,
    marker::PhantomData,
    num::NonZero,
    pin::Pin,
    ptr,
    sync::atomic::{self, AtomicUsize},
    task, thread,
};

use crossbeam_utils::CachePadded;
use futures_lite::Stream;
use pin_project_lite::pin_project;

use colorless::{SyncIntoCoroutine, sync_into_coroutine};

pub use futures_lite::{self, future::block_on};

pub type SyncFn = dyn Fn() + Send + Sync;

#[non_exhaustive]
#[derive(Default)]
pub struct ExecutorConfig {
    pub thread_name: Option<String>,
    pub acquire_thread_handler: Option<Box<SyncFn>>,
    pub release_thread_handler: Option<Box<SyncFn>>,
    pub num_threads: Option<NonZero<usize>>,
    pub stack_size: Option<NonZero<usize>>,
    pub deadlock_handler: Option<Box<SyncFn>>,
}

impl ExecutorConfig {
    pub const fn new() -> Self {
        ExecutorConfig {
            thread_name: None,
            acquire_thread_handler: None,
            release_thread_handler: None,
            num_threads: None,
            stack_size: None,
            deadlock_handler: None,
        }
    }
}

pub struct ThreadBuilder<'a> {
    task_receiver: async_channel::Receiver<SyncIntoCoroutine<()>>,
    acquire_thread_handler: Option<&'a SyncFn>,
    release_thread_handler: Option<&'a SyncFn>,
    executor: &'a Executor,
    unsend: PhantomData<*mut ()>,
}

impl ThreadBuilder<'_> {
    pub fn run(self) {
        EXECUTOR.set(Some(unsafe {
            ptr::NonNull::new_unchecked(self.executor as *const _ as *mut _)
        }));

        let ex = unsend::executor::Executor::new();

        pin_project! {
            struct WorkerFuture<'a, F> {
                acquire_thread_handler: Option<&'a SyncFn>,
                release_thread_handler: Option<&'a SyncFn>,
                #[pin]
                inner: F,
            }
        }

        impl<F: Future> Future for WorkerFuture<'_, F> {
            type Output = F::Output;

            fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<Self::Output> {
                let project = self.project();
                if let Some(acquire_thread) = *project.acquire_thread_handler {
                    acquire_thread()
                }
                let out = project.inner.poll(cx);
                if let Some(release_thread) = *project.release_thread_handler {
                    release_thread()
                }

                out
            }
        }

        block_on(WorkerFuture {
            acquire_thread_handler: self.acquire_thread_handler,
            release_thread_handler: self.release_thread_handler,
            inner: ex.run(async {
                loop {
                    let Ok(task) = self.task_receiver.recv().await else {
                        return;
                    };
                    ex.spawn(task.into_future()).detach();
                }
            }),
        })
    }
}

pub struct Executor {
    unblocked_worker_count: CachePadded<AtomicUsize>,
    task_sender: async_channel::Sender<SyncIntoCoroutine<()>>,
    deadlock_handler: Option<Box<SyncFn>>,
}

impl Executor {
    pub fn build_scoped<W, F, R>(
        config: ExecutorConfig,
        wrapper: W,
        with_executor: F,
    ) -> io::Result<R>
    where
        W: Fn(ThreadBuilder) + Sync,
        F: FnOnce(&Self) -> R,
    {
        let num_threads = config
            .num_threads
            .map_or_else(thread::available_parallelism, Ok)?;
        let (task_sender, task_receiver) = async_channel::unbounded();
        let executor = Executor {
            task_sender,
            unblocked_worker_count: CachePadded::new(AtomicUsize::new(num_threads.get())),
            deadlock_handler: config.deadlock_handler,
        };

        thread::scope(|scope| {
            let mut workers = Vec::with_capacity(num_threads.get());
            for i in 0..num_threads.get() {
                let mut thread_builder = thread::Builder::new();
                if let Some(thread_name) = &config.thread_name {
                    thread_builder = thread_builder.name(format!("{thread_name}_{i}"))
                }
                if let Some(stack_size) = &config.stack_size {
                    thread_builder = thread_builder.stack_size(stack_size.get())
                }
                let res = thread_builder.spawn_scoped(scope, || {
                    let worker_builder = ThreadBuilder {
                        task_receiver: task_receiver.clone(),
                        acquire_thread_handler: config.acquire_thread_handler.as_deref(),
                        release_thread_handler: config.release_thread_handler.as_deref(),
                        executor: &executor,
                        unsend: PhantomData,
                    };
                    wrapper(worker_builder)
                });
                match res {
                    Ok(worker) => workers.push(worker),
                    Err(e) => {
                        task_receiver.close();
                        for worker in workers {
                            worker.join().unwrap();
                        }
                        return Err(e);
                    }
                }
            }

            struct CloseChannel<'a>(&'a async_channel::Receiver<SyncIntoCoroutine<()>>);

            impl Drop for CloseChannel<'_> {
                fn drop(&mut self) {
                    self.0.close();
                }
            }

            let _g = CloseChannel(&task_receiver);

            Ok(with_executor(&executor))
        })
    }

    pub fn spawn<F, R>(&self, f: F) -> Task<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let (result_sender, result_receiver) = async_channel::bounded(1);
        self.task_sender
            .try_send(sync_into_coroutine(move || {
                result_sender.try_send(f()).unwrap()
            }))
            .unwrap();
        Task { result_receiver }
    }

    fn mark_unblocked(&self) {
        if self.deadlock_handler.is_some() {
            self.unblocked_worker_count
                .fetch_add(1, atomic::Ordering::SeqCst);
        }
    }

    fn mark_blocked(&self) {
        if let Some(deadlock_handler) = &self.deadlock_handler {
            let old_count = self
                .unblocked_worker_count
                .fetch_sub(1, atomic::Ordering::SeqCst);

            if old_count == 1 {
                deadlock_handler()
            }
        }
    }
}

pub fn mark_blocked() {
    unsafe {
        EXECUTOR
            .get()
            .expect("called `mark_blocked` outside of a executor's worker thread")
            .as_ref()
            .mark_blocked();
    }
}

pub fn mark_unblocked() {
    unsafe {
        EXECUTOR
            .get()
            .expect("called `mark_unblocked` outside of a executor's worker thread")
            .as_ref()
            .mark_unblocked();
    }
}

pub fn spawn<F, R>(f: F) -> Task<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    unsafe {
        EXECUTOR
            .get()
            .expect("called `spawn` outside of a executor's worker thread")
            .as_ref()
            .spawn(f)
    }
}

thread_local! {
    static EXECUTOR: Cell<Option<ptr::NonNull<Executor>>> = const { Cell::new(None) };
}

pin_project! {
    pub struct Task<R> {
        #[pin]
        result_receiver: async_channel::Receiver<R>,
    }
}

impl<R> Future for Task<R> {
    type Output = R;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<Self::Output> {
        self.project()
            .result_receiver
            .poll_next(cx)
            .map(|r| r.expect("task has panicked"))
    }
}
