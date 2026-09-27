use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    thread,
};

use futures_executor::block_on;
use linorun_core::{
    ActivationFailure, CallContext, Component, ComponentContext, FiberState, Runtime, RuntimeTask,
    component, service_contract,
};

struct __LinorunComponent {
    value: usize,
    events: Arc<Mutex<Vec<usize>>>,
}

#[allow(non_camel_case_types)]
struct __LinorunComponent_1;

const __LINORUN_DESCRIPTOR: usize = 3;
const __LINORUN_DESCRIPTOR_1: usize = 5;

fn __linorun_setup(value: usize) -> usize {
    value + 1
}

fn __linorun_setup_1(value: usize) -> usize {
    value + 2
}

fn merge_probe(base: &[usize; 2], overlay: &[usize; 2]) -> [usize; 2] {
    [base[0] + overlay[0], base[1] + overlay[1]]
}

service_contract! {
    contract Probe {
        id: "hygiene/probe@1",
        intercept: [usize; 2],
        identity: [0, 0],
        merge: merge_probe,
        methods: [async fn read(value: usize) -> usize;],
    }
}

struct ProbeService;

impl ProbeProvider for ProbeService {
    fn read<'a>(
        &'a self,
        call: CallContext<[usize; 2]>,
        value: usize,
    ) -> Pin<Box<dyn Future<Output = usize> + Send + 'a>> {
        Box::pin(async move { value + call.intercept.into_iter().sum::<usize>() })
    }
}

#[component(
    id = "hygiene/component@1",
    requires = [Probe => {
        [r#__LINORUN_DESCRIPTOR, r#__LINORUN_DESCRIPTOR_1]
    }],
)]
async fn worker(
    __linorun_context: ComponentContext,
    mut __linorun_config: Arc<r#__LinorunComponent>,
) -> Result<(), ActivationFailure> {
    let __linorun_context_1 = __linorun_context.clone();
    let __linorun_config_1 = __linorun_config.value;
    __linorun_config = Arc::clone(&__linorun_config);
    assert_eq!(std::mem::size_of::<r#__LinorunComponent_1>(), 0);

    let value = r#__linorun_setup_1(r#__linorun_setup(__linorun_config_1));
    let value = __linorun_context_1.service::<Probe>()?.read(value).await;
    __linorun_config.events.lock().unwrap().push(value);

    let events = Arc::clone(&__linorun_config.events);
    __linorun_context
        .on_cleanup("record cleanup", move || async move {
            events.lock().unwrap().push(99);
            Ok(())
        })
        .await?;
    Ok(())
}

#[test]
fn caller_items_attributes_and_arguments_keep_their_meaning() {
    block_on(async {
        let component = worker();
        let replacement = worker();
        assert!(std::ptr::eq(
            component.descriptor(),
            replacement.descriptor(),
        ));

        let runtime = Runtime::new(|task: RuntimeTask| {
            thread::spawn(move || block_on(task));
        });
        runtime
            .provide::<Probe>(Arc::new(ProbeService))
            .await
            .unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let fiber = runtime
            .mount(
                component,
                __LinorunComponent {
                    value: 6,
                    events: Arc::clone(&events),
                },
            )
            .await
            .unwrap();
        assert_eq!(fiber.wait_settled().await, FiberState::Active);
        assert_eq!(*events.lock().unwrap(), [17]);

        fiber.replace(replacement).await.unwrap();
        assert_eq!(fiber.wait_settled().await, FiberState::Active);
        assert_eq!(*events.lock().unwrap(), [17, 99, 17]);

        assert!(runtime.shutdown().await.cleanup_errors.is_empty());
        assert_eq!(*events.lock().unwrap(), [17, 99, 17, 99]);
    });
}
