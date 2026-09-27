use std::{sync::Arc, thread};

use futures_executor::block_on;
use linorun_core::{
    ActivationFailure, CallContext, ComponentContext, FiberState, Runtime, component,
    service_contract, service_impl,
};

struct Send;
struct Sync;

service_contract! {
    contract Probe {
        id: "marker-hygiene/probe@1",
        methods: [async fn value() -> usize;],
    }
}

struct Provider;

#[service_impl]
impl ProbeProvider for Provider {
    async fn value(&self, _: CallContext<()>) -> usize {
        42
    }
}

#[component(id = "marker-hygiene/consumer@1", requires = [Probe])]
async fn consumer(context: ComponentContext) -> Result<(), ActivationFailure> {
    assert_eq!(context.service::<Probe>()?.value().await, 42);
    Ok(())
}

#[test]
fn caller_marker_names_do_not_shadow_generated_trait_bounds() {
    let _ = (Send, Sync);
    block_on(async {
        let runtime = Runtime::new(|task| {
            thread::spawn(move || block_on(task));
        });
        runtime.provide::<Probe>(Arc::new(Provider)).await.unwrap();
        let fiber = runtime.mount(consumer(), ()).await.unwrap();
        assert_eq!(fiber.wait_settled().await, FiberState::Active);
        assert!(runtime.shutdown().await.cleanup_errors.is_empty());
    });
}
