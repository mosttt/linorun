use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures_core::Stream;

use crate::{CleanupError, EffectError};

pub type InverseFuture = Pin<Box<dyn Future<Output = Result<(), CleanupError>> + Send + 'static>>;
pub type InverseFn = Box<dyn FnOnce() -> InverseFuture + Send + 'static>;

pub trait EffectDriver: Send + 'static {
    fn poll_step(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<DriverEvent, EffectError>>;
}

pub enum DriverEvent {
    Yield { inverse: InverseFn, has_next: bool },
    Done,
}

pub trait IntoEffectDriver {
    type Driver: EffectDriver;

    fn into_driver(self) -> Self::Driver;
}

pub mod effect {
    use super::*;

    pub fn inverse<F, Fut>(cleanup: F) -> InverseFn
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), CleanupError>> + Send + 'static,
    {
        Box::new(move || Box::pin(cleanup()))
    }

    pub fn sync<F>(forward: F) -> impl IntoEffectDriver
    where
        F: FnOnce() -> Result<Option<InverseFn>, EffectError> + Send + 'static,
    {
        SyncAdapter(forward)
    }

    pub fn once<F, Fut>(forward: F) -> impl IntoEffectDriver
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Option<InverseFn>, EffectError>> + Send + 'static,
    {
        OnceAdapter(forward)
    }

    pub fn iter<F, I>(forward: F) -> impl IntoEffectDriver
    where
        F: FnOnce() -> I + Send + 'static,
        I: IntoIterator<Item = Result<InverseFn, EffectError>> + Send + 'static,
        I::IntoIter: Send + 'static,
    {
        IterAdapter(forward)
    }

    pub fn steps<F, S>(forward: F) -> impl IntoEffectDriver
    where
        F: FnOnce() -> S + Send + 'static,
        S: Stream<Item = Result<InverseFn, EffectError>> + Send + 'static,
    {
        StepsAdapter(forward)
    }

    struct SyncAdapter<F>(F);

    impl<F> IntoEffectDriver for SyncAdapter<F>
    where
        F: FnOnce() -> Result<Option<InverseFn>, EffectError> + Send + 'static,
    {
        type Driver = SyncDriver<F>;

        fn into_driver(self) -> Self::Driver {
            SyncDriver {
                forward: Some(Box::new(self.0)),
            }
        }
    }

    struct SyncDriver<F> {
        forward: Option<Box<F>>,
    }

    impl<F> Unpin for SyncDriver<F> {}

    impl<F> EffectDriver for SyncDriver<F>
    where
        F: FnOnce() -> Result<Option<InverseFn>, EffectError> + Send + 'static,
    {
        fn poll_step(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Result<DriverEvent, EffectError>> {
            let Some(forward) = self.get_mut().forward.take() else {
                return Poll::Ready(Ok(DriverEvent::Done));
            };

            Poll::Ready(forward().map(|inverse| match inverse {
                Some(inverse) => DriverEvent::Yield {
                    inverse,
                    has_next: false,
                },
                None => DriverEvent::Done,
            }))
        }
    }

    struct OnceAdapter<F>(F);

    impl<F, Fut> IntoEffectDriver for OnceAdapter<F>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Option<InverseFn>, EffectError>> + Send + 'static,
    {
        type Driver = OnceDriver<F, Fut>;

        fn into_driver(self) -> Self::Driver {
            OnceDriver {
                state: OnceState::Init(Some(Box::new(self.0))),
            }
        }
    }

    enum OnceState<F, Fut> {
        Init(Option<Box<F>>),
        Polling(Pin<Box<Fut>>),
        Done,
    }

    struct OnceDriver<F, Fut> {
        state: OnceState<F, Fut>,
    }

    impl<F, Fut> Unpin for OnceDriver<F, Fut> {}

    impl<F, Fut> EffectDriver for OnceDriver<F, Fut>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Option<InverseFn>, EffectError>> + Send + 'static,
    {
        fn poll_step(
            self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Result<DriverEvent, EffectError>> {
            let this = self.get_mut();

            if let OnceState::Init(forward) = &mut this.state {
                let forward = forward.take().expect("init closure is present");
                this.state = OnceState::Polling(Box::pin(forward()));
            }

            let OnceState::Polling(future) = &mut this.state else {
                return Poll::Ready(Ok(DriverEvent::Done));
            };

            let Poll::Ready(result) = future.as_mut().poll(context) else {
                return Poll::Pending;
            };
            this.state = OnceState::Done;

            Poll::Ready(result.map(|inverse| match inverse {
                Some(inverse) => DriverEvent::Yield {
                    inverse,
                    has_next: false,
                },
                None => DriverEvent::Done,
            }))
        }
    }

    struct IterAdapter<F>(F);

    impl<F, I> IntoEffectDriver for IterAdapter<F>
    where
        F: FnOnce() -> I + Send + 'static,
        I: IntoIterator<Item = Result<InverseFn, EffectError>> + Send + 'static,
        I::IntoIter: Send + 'static,
    {
        type Driver = IterDriver<F, I>;

        fn into_driver(self) -> Self::Driver {
            IterDriver {
                state: IterState::Init(Some(Box::new(self.0))),
            }
        }
    }

    enum IterState<F, I> {
        Init(Option<Box<F>>),
        Iterating(Box<I>),
        Done,
    }

    struct IterDriver<F, I>
    where
        I: IntoIterator,
    {
        state: IterState<F, I::IntoIter>,
    }

    impl<F, I> Unpin for IterDriver<F, I> where I: IntoIterator {}

    impl<F, I> EffectDriver for IterDriver<F, I>
    where
        F: FnOnce() -> I + Send + 'static,
        I: IntoIterator<Item = Result<InverseFn, EffectError>> + Send + 'static,
        I::IntoIter: Send + 'static,
    {
        fn poll_step(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Result<DriverEvent, EffectError>> {
            let this = self.get_mut();

            if let IterState::Init(forward) = &mut this.state {
                let forward = forward.take().expect("init closure is present");
                this.state = IterState::Iterating(Box::new(forward().into_iter()));
            }

            let IterState::Iterating(iter) = &mut this.state else {
                return Poll::Ready(Ok(DriverEvent::Done));
            };

            match iter.next() {
                Some(Ok(inverse)) => Poll::Ready(Ok(DriverEvent::Yield {
                    inverse,
                    has_next: true,
                })),
                Some(Err(error)) => {
                    this.state = IterState::Done;
                    Poll::Ready(Err(error))
                }
                None => {
                    this.state = IterState::Done;
                    Poll::Ready(Ok(DriverEvent::Done))
                }
            }
        }
    }

    struct StepsAdapter<F>(F);

    impl<F, S> IntoEffectDriver for StepsAdapter<F>
    where
        F: FnOnce() -> S + Send + 'static,
        S: Stream<Item = Result<InverseFn, EffectError>> + Send + 'static,
    {
        type Driver = StepsDriver<F, S>;

        fn into_driver(self) -> Self::Driver {
            StepsDriver {
                state: StepsState::Init(Some(Box::new(self.0))),
            }
        }
    }

    enum StepsState<F, S> {
        Init(Option<Box<F>>),
        Polling(Pin<Box<S>>),
        Done,
    }

    struct StepsDriver<F, S> {
        state: StepsState<F, S>,
    }

    impl<F, S> Unpin for StepsDriver<F, S> {}

    impl<F, S> EffectDriver for StepsDriver<F, S>
    where
        F: FnOnce() -> S + Send + 'static,
        S: Stream<Item = Result<InverseFn, EffectError>> + Send + 'static,
    {
        fn poll_step(
            self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Result<DriverEvent, EffectError>> {
            let this = self.get_mut();

            if let StepsState::Init(forward) = &mut this.state {
                let forward = forward.take().expect("init closure is present");
                this.state = StepsState::Polling(Box::pin(forward()));
            }

            let StepsState::Polling(stream) = &mut this.state else {
                return Poll::Ready(Ok(DriverEvent::Done));
            };

            match stream.as_mut().poll_next(context) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Some(Ok(inverse))) => Poll::Ready(Ok(DriverEvent::Yield {
                    inverse,
                    has_next: true,
                })),
                Poll::Ready(Some(Err(error))) => {
                    this.state = StepsState::Done;
                    Poll::Ready(Err(error))
                }
                Poll::Ready(None) => {
                    this.state = StepsState::Done;
                    Poll::Ready(Ok(DriverEvent::Done))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::Waker,
    };

    use super::*;

    fn poll(driver: Pin<&mut impl EffectDriver>) -> Poll<Result<DriverEvent, EffectError>> {
        let mut context = Context::from_waker(Waker::noop());
        driver.poll_step(&mut context)
    }

    #[test]
    fn into_driver_does_not_run_forward() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let adapter = effect::sync(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        });

        let mut driver = Box::pin(adapter.into_driver());
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        assert!(matches!(
            poll(driver.as_mut()),
            Poll::Ready(Ok(DriverEvent::Done))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn once_yields_a_final_inverse() {
        let adapter =
            effect::once(|| future::ready(Ok(Some(effect::inverse(|| async { Ok(()) })))));
        let mut driver = Box::pin(adapter.into_driver());

        let Poll::Ready(Ok(DriverEvent::Yield { has_next, inverse })) = poll(driver.as_mut())
        else {
            panic!("expected a final yield");
        };

        assert!(!has_next);
        let mut cleanup = inverse();
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            cleanup.as_mut().poll(&mut context),
            Poll::Ready(Ok(()))
        ));
    }

    #[test]
    fn iterator_exposes_a_boundary_after_each_item() {
        let adapter = effect::iter(|| {
            vec![
                Ok(effect::inverse(|| async { Ok(()) })),
                Ok(effect::inverse(|| async { Ok(()) })),
            ]
        });
        let mut driver = Box::pin(adapter.into_driver());

        assert!(matches!(
            poll(driver.as_mut()),
            Poll::Ready(Ok(DriverEvent::Yield { has_next: true, .. }))
        ));
        assert!(matches!(
            poll(driver.as_mut()),
            Poll::Ready(Ok(DriverEvent::Yield { has_next: true, .. }))
        ));
        assert!(matches!(
            poll(driver.as_mut()),
            Poll::Ready(Ok(DriverEvent::Done))
        ));
    }
}
