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
use futures_lite::{FutureExt, Stream};
use pin_project_lite::pin_project;

use colorless::{SyncIntoCoroutine, sync_into_coroutine, sync_into_coroutine_with_stack_size};

pub use futures_lite::{self, future::block_on};

pub type SyncFn = dyn Fn() + Send + Sync;
pub type ThreadNameFn = dyn Fn(usize) -> String + Send + Sync;

#[non_exhaustive]
#[derive(Default)]
pub struct ExecutorConfig {
    pub thread_name: Option<Box<ThreadNameFn>>,
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
    broadcast_task_receiver: async_channel::Receiver<SyncIntoCoroutine<()>>,
    task_receiver: async_channel::Receiver<SyncIntoCoroutine<()>>,
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

                struct ReleaseThreadGuard<'a>(Option<&'a SyncFn>);

                impl Drop for ReleaseThreadGuard<'_> {
                    fn drop(&mut self) {
                        if let Some(release_thread) = self.0 {
                            release_thread()
                        }
                    }
                }

                let _guard = ReleaseThreadGuard(*project.release_thread_handler);

                project.inner.poll(cx)
            }
        }

        block_on(WorkerFuture {
            acquire_thread_handler: self.executor.acquire_thread_handler.as_deref(),
            release_thread_handler: self.executor.release_thread_handler.as_deref(),
            inner: ex.run(
                async {
                    loop {
                        let Ok(task) = self.task_receiver.recv().await else {
                            return;
                        };
                        ex.spawn(task.into_future()).detach();
                    }
                }
                .or(async {
                    loop {
                        let Ok(task) = self.broadcast_task_receiver.recv().await else {
                            return;
                        };
                        ex.spawn(task.into_future()).detach();
                    }
                }),
            ),
        })
    }
}

pub struct Executor {
    unblocked_worker_count: CachePadded<AtomicUsize>,
    task_sender: async_channel::Sender<SyncIntoCoroutine<()>>,
    deadlock_handler: Option<Box<SyncFn>>,
    broadcast_senders: Vec<async_channel::Sender<SyncIntoCoroutine<()>>>,
    stack_size: Option<NonZero<usize>>,
    acquire_thread_handler: Option<Box<SyncFn>>,
    release_thread_handler: Option<Box<SyncFn>>,
}

impl Executor {
    pub fn build_scoped<W, F, R>(
        config: ExecutorConfig,
        wrapper: W,
        with_executor: F,
    ) -> io::Result<R>
    where
        W: Fn(ThreadBuilder<'_>) + Sync,
        F: FnOnce(&Executor) -> R,
    {
        let num_threads = config
            .num_threads
            .map_or_else(thread::available_parallelism, Ok)?;
        let (task_sender, task_receiver) = async_channel::unbounded();
        let (broadcast_senders, broadcast_receivers): (Vec<_>, Vec<_>) = (0..num_threads.get())
            .map(|_| async_channel::unbounded())
            .collect();
        let executor = Executor {
            task_sender,
            unblocked_worker_count: CachePadded::new(AtomicUsize::new(num_threads.get())),
            deadlock_handler: config.deadlock_handler,
            broadcast_senders,
            stack_size: config.stack_size,
            acquire_thread_handler: config.acquire_thread_handler,
            release_thread_handler: config.release_thread_handler,
        };

        thread::scope(|scope| {
            let mut workers = Vec::with_capacity(num_threads.get());

            for (i, broadcast_task_receiver) in broadcast_receivers.into_iter().enumerate() {
                let mut thread_builder = thread::Builder::new();
                if let Some(thread_name) = &config.thread_name {
                    thread_builder = thread_builder.name(thread_name(i))
                }
                if let Some(stack_size) = &config.stack_size {
                    thread_builder = thread_builder.stack_size(stack_size.get())
                }
                let res = thread_builder.spawn_scoped(scope, || {
                    let worker_builder = ThreadBuilder {
                        broadcast_task_receiver,
                        task_receiver: task_receiver.clone(),
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
        let f = move || {
            let res = result_sender.try_send(f());
            if let Err(async_channel::TrySendError::Full(_)) = res {
                unreachable!()
            }
        };
        self.task_sender
            .try_send(if let Some(stack_size) = self.stack_size {
                sync_into_coroutine_with_stack_size(stack_size.get(), f)
                    .expect("unable to create a fresh coroutine for a task")
            } else {
                sync_into_coroutine(f)
            })
            .unwrap();
        Task { result_receiver }
    }

    pub fn num_threads(&self) -> usize {
        self.broadcast_senders.len()
    }

    pub fn broadcast<F, G, R>(&self, mut g: G) -> BroadcastTask<R>
    where
        G: FnMut() -> F,
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let (result_sender, result_receiver) = async_channel::bounded(self.num_threads());
        for (i, broadcast_task_sender) in self.broadcast_senders.iter().enumerate() {
            let result_sender = result_sender.clone();
            let f = g();
            let f = move || {
                let res = result_sender.try_send((i, f()));
                if let Err(async_channel::TrySendError::Full(_)) = res {
                    unreachable!()
                }
            };
            broadcast_task_sender
                .try_send(if let Some(stack_size) = self.stack_size {
                    sync_into_coroutine_with_stack_size(stack_size.get(), f)
                        .expect("unable to create a fresh coroutine for a broadcast task")
                } else {
                    sync_into_coroutine(f)
                })
                .unwrap();
        }
        BroadcastTask { result_receiver }
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

pub fn num_threads() -> usize {
    unsafe {
        EXECUTOR
            .get()
            .expect("called `num_threads` outside of a executor's worker thread")
            .as_ref()
            .num_threads()
    }
}

pub fn broadcast<F, G, R>(g: G) -> BroadcastTask<R>
where
    G: FnMut() -> F,
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    unsafe {
        EXECUTOR
            .get()
            .expect("called `broadcast` outside of a executor's worker thread")
            .as_ref()
            .broadcast(g)
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

pin_project! {
    pub struct BroadcastTask<R> {
        #[pin]
        result_receiver: async_channel::Receiver<(usize, R)>,
    }
}

impl<R> Stream for BroadcastTask<R> {
    type Item = (usize, R);

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> task::Poll<Option<Self::Item>> {
        self.project().result_receiver.poll_next(cx)
    }
}
