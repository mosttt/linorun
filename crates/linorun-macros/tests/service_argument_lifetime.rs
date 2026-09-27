use std::{
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

use linorun_macros::service_impl;

struct Resource(Arc<AtomicUsize>);

impl Drop for Resource {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

// A local context isolates the macro's argument-lifetime contract from runtime scheduling.
struct CallContext<T>(T);

trait Provider {
    fn named<'a>(
        &'a self,
        call: CallContext<Resource>,
        resource: Resource,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
    fn wildcard<'a>(
        &'a self,
        call: CallContext<Resource>,
        resource: Resource,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
    fn complete<'a>(
        &'a self,
        call: CallContext<Resource>,
        resource: Resource,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

struct Service;

#[service_impl]
impl Provider for Service {
    async fn named(&self, _call: CallContext<Resource>, _resource: Resource) {
        pending::<()>().await;
    }

    async fn wildcard(&self, _: CallContext<Resource>, _: Resource) {
        pending::<()>().await;
    }

    async fn complete(&self, _: CallContext<Resource>, _: Resource) {}
}

async fn native(_call: CallContext<Resource>, _resource: Resource) {
    pending::<()>().await;
}

#[test]
fn unpolled_and_pending_provider_futures_retain_all_owned_arguments() {
    let service = Service;
    for method in 0..3 {
        for poll_before_drop in [false, true] {
            let drops = Arc::new(AtomicUsize::new(0));
            let call = CallContext(Resource(Arc::clone(&drops)));
            let resource = Resource(Arc::clone(&drops));
            let mut future: Pin<Box<dyn Future<Output = ()> + Send + '_>> = match method {
                0 => Box::pin(native(call, resource)),
                1 => service.named(call, resource),
                _ => service.wildcard(call, resource),
            };
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            if poll_before_drop {
                assert_eq!(
                    future
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop())),
                    Poll::Pending
                );
                assert_eq!(drops.load(Ordering::SeqCst), 0);
            }
            drop(future);
            assert_eq!(drops.load(Ordering::SeqCst), 2);
        }
    }
}

#[test]
fn completed_provider_future_releases_arguments_at_completion() {
    let drops = Arc::new(AtomicUsize::new(0));
    let service = Service;
    let mut future = service.complete(
        CallContext(Resource(Arc::clone(&drops))),
        Resource(Arc::clone(&drops)),
    );
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(())
    );
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    drop(future);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
}
