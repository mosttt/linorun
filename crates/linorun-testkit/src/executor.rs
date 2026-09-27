use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

use futures_util::task::{ArcWake, waker_ref};
use linorun_core::{RuntimeTask, TaskSpawner};

#[derive(Clone)]
pub struct DeterministicExecutor {
    inner: Arc<ExecutorInner>,
}

struct ExecutorInner {
    queue: Mutex<VecDeque<Arc<Task>>>,
    active: AtomicUsize,
}

struct Task {
    executor: Weak<ExecutorInner>,
    future: Mutex<Option<RuntimeTask>>,
    queued: AtomicBool,
}

impl ArcWake for Task {
    fn wake_by_ref(task: &Arc<Self>) {
        if task.queued.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(executor) = task.executor.upgrade() {
            executor
                .queue
                .lock()
                .expect("executor queue mutex poisoned")
                .push_back(Arc::clone(task));
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DriveError {
    pub active_tasks: usize,
}

impl std::fmt::Display for DriveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "future is pending while the deterministic executor has no runnable task ({} active)",
            self.active_tasks
        )
    }
}

impl std::error::Error for DriveError {}

impl DeterministicExecutor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ExecutorInner {
                queue: Mutex::new(VecDeque::new()),
                active: AtomicUsize::new(0),
            }),
        }
    }

    /// Polls one runnable runtime task.
    pub fn run_one(&self) -> bool {
        let Some(task) = self
            .inner
            .queue
            .lock()
            .expect("executor queue mutex poisoned")
            .pop_front()
        else {
            return false;
        };
        task.queued.store(false, Ordering::Release);
        let mut future = task.future.lock().expect("task future mutex poisoned");
        let Some(runtime_task) = future.as_mut() else {
            return true;
        };
        let waker = waker_ref(&task);
        let mut context = Context::from_waker(&waker);
        if runtime_task.as_mut().poll(&mut context).is_ready() {
            *future = None;
            self.inner.active.fetch_sub(1, Ordering::AcqRel);
        }
        true
    }

    /// Runs until no task is currently runnable and returns the number of polls.
    pub fn run_until_stalled(&self) -> usize {
        let mut polls = 0;
        while self.run_one() {
            polls += 1;
        }
        polls
    }

    #[must_use]
    pub fn runnable_tasks(&self) -> usize {
        self.inner
            .queue
            .lock()
            .expect("executor queue mutex poisoned")
            .len()
    }

    #[must_use]
    pub fn active_tasks(&self) -> usize {
        self.inner.active.load(Ordering::Acquire)
    }

    /// Drives an external future together with the runtime tasks spawned here.
    pub fn drive<F>(&self, future: F) -> Result<F::Output, DriveError>
    where
        F: Future,
    {
        let mut future = Box::pin(future);
        loop {
            let mut context = Context::from_waker(Waker::noop());
            if let Poll::Ready(output) = Pin::as_mut(&mut future).poll(&mut context) {
                return Ok(output);
            }
            if !self.run_one() {
                return Err(DriveError {
                    active_tasks: self.active_tasks(),
                });
            }
        }
    }
}

impl Default for DeterministicExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskSpawner for DeterministicExecutor {
    fn spawn(&self, future: RuntimeTask) {
        self.inner.active.fetch_add(1, Ordering::AcqRel);
        let task = Arc::new(Task {
            executor: Arc::downgrade(&self.inner),
            future: Mutex::new(Some(future)),
            queued: AtomicBool::new(true),
        });
        self.inner
            .queue
            .lock()
            .expect("executor queue mutex poisoned")
            .push_back(task);
    }
}
