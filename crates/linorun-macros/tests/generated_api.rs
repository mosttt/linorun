use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    thread,
};

use futures_executor::block_on;
use linorun_core::{
    Activation, CallContext, Component, ComponentContext, ComponentDescriptor, Runtime,
    RuntimeTask, TaskSpawner,
};
use linorun_macros::{component_descriptor, service_contract};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LogIntercept(Vec<&'static str>);

fn merge_log(base: &LogIntercept, overlay: &LogIntercept) -> LogIntercept {
    LogIntercept(base.0.iter().chain(&overlay.0).copied().collect())
}

service_contract! {
    contract Logger {
        id: "macros/logger@1",
        intercept: LogIntercept,
        identity: LogIntercept::default(),
        merge: merge_log,
        methods: [
            async fn info(__linorun_call: String) -> Result<(), String>;
            async fn ping() -> usize;
        ],
    }
}

component_descriptor! {
    static CONSUMER_DESCRIPTOR: () {
        id: "macros/consumer@1",
        requires: [Logger => LogIntercept(vec!["declared"])],
        provides: [],
    }
}

struct LoggerImplementation {
    calls: Arc<Mutex<Vec<(String, LogIntercept)>>>,
}

impl LoggerProvider for LoggerImplementation {
    fn info<'a>(
        &'a self,
        call: CallContext<LogIntercept>,
        message: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push((message, call.intercept));
            Ok(())
        })
    }

    fn ping<'a>(
        &'a self,
        _: CallContext<LogIntercept>,
    ) -> Pin<Box<dyn Future<Output = usize> + Send + 'a>> {
        Box::pin(async { self.calls.lock().unwrap().len() })
    }
}

struct Consumer;

impl Component for Consumer {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CONSUMER_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let logger = context.context().service::<Logger>().unwrap();
        Activation::Once(Box::pin(async move {
            logger.info("ready".to_owned()).await.unwrap();
            assert_eq!(logger.ping().await, 1);
            Ok(None)
        }))
    }
}

#[derive(Clone, Copy)]
struct ThreadSpawner;

impl TaskSpawner for ThreadSpawner {
    fn spawn(&self, task: RuntimeTask) {
        thread::spawn(move || block_on(task));
    }
}

#[test]
fn generated_contract_client_and_descriptor_run_downstream() {
    block_on(async {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::builder(ThreadSpawner).build();
        let root = runtime
            .root()
            .intercept::<Logger>(LogIntercept(vec!["route"]));
        let provider: Arc<dyn LoggerProvider> = Arc::new(LoggerImplementation {
            calls: Arc::clone(&calls),
        });
        root.provide::<Logger>(provider).await.unwrap();
        let consumer = root.mount(Consumer, ()).await.unwrap();
        assert_eq!(
            consumer.wait_settled().await,
            linorun_core::FiberState::Active
        );
        assert_eq!(
            *calls.lock().unwrap(),
            [("ready".to_owned(), LogIntercept(vec!["declared", "route"]))]
        );
        runtime.shutdown().await;
    });
}

#[test]
fn generated_contract_intercept_obeys_identity_and_associativity() {
    let identity = LogIntercept::default();
    let a = LogIntercept(vec!["a"]);
    let b = LogIntercept(vec!["b"]);
    let c = LogIntercept(vec!["c"]);
    assert_eq!(merge_log(&identity, &a), a);
    assert_eq!(merge_log(&a, &identity), a);
    assert_eq!(
        merge_log(&merge_log(&a, &b), &c),
        merge_log(&a, &merge_log(&b, &c))
    );
}
