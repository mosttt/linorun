use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use futures_channel::{mpsc, oneshot};
use futures_core::Stream;

/// The resolving side of a one-shot controlled future.
pub struct Control<T> {
    sender: Arc<Mutex<Option<oneshot::Sender<T>>>>,
}

impl<T> Clone for Control<T> {
    fn clone(&self) -> Self {
        Self {
            sender: Arc::clone(&self.sender),
        }
    }
}

impl<T> Control<T> {
    /// Resolves the future. The value is returned if another clone already won.
    pub fn complete(&self, value: T) -> Result<(), T> {
        let Some(sender) = self.sender.lock().expect("control mutex poisoned").take() else {
            return Err(value);
        };
        sender.send(value)
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.sender
            .lock()
            .expect("control mutex poisoned")
            .is_some()
    }
}

/// A future completed explicitly through [`Control`].
pub struct Controlled<T> {
    receiver: oneshot::Receiver<T>,
}

impl<T> Future for Controlled<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver)
            .poll(context)
            .map(|result| result.expect("controlled future sender was dropped"))
    }
}

#[must_use]
pub fn controlled<T>() -> (Control<T>, Controlled<T>) {
    let (sender, receiver) = oneshot::channel();
    (
        Control {
            sender: Arc::new(Mutex::new(Some(sender))),
        },
        Controlled { receiver },
    )
}

/// The producer side of a manually advanced stream.
#[derive(Clone)]
pub struct StepSender<T> {
    sender: mpsc::UnboundedSender<T>,
}

impl<T> StepSender<T> {
    pub fn push(&self, item: T) -> Result<(), T> {
        self.sender
            .unbounded_send(item)
            .map_err(|error| error.into_inner())
    }

    pub fn close(&self) {
        self.sender.close_channel();
    }
}

/// The consumer side of a manually advanced stream.
pub struct StepStream<T> {
    receiver: mpsc::UnboundedReceiver<T>,
}

impl<T> Stream for StepStream<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.receiver).poll_next(context)
    }
}

#[must_use]
pub fn step_stream<T>() -> (StepSender<T>, StepStream<T>) {
    let (sender, receiver) = mpsc::unbounded();
    (StepSender { sender }, StepStream { receiver })
}
