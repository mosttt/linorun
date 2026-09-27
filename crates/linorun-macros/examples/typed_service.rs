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

#[derive(Clone, Default)]
struct LogIntercept(Vec<&'static str>);

fn merge_log(base: &LogIntercept, overlay: &LogIntercept) -> LogIntercept {
    LogIntercept(base.0.iter().chain(&overlay.0).copied().collect())
}

service_contract! {
    contract Logger {
        id: "example/logger@1",
        intercept: LogIntercept,
        identity: LogIntercept::default(),
        merge: merge_log,
        methods: [
            async fn info(message: String) -> Result<(), String>;
        ],
    }
}

component_descriptor! {
    static CONSUMER: () {
        id: "example/log-consumer@1",
        requires: [Logger => LogIntercept(vec!["component"])],
        provides: [],
    }
}

struct LoggerImplementation {
    records: Arc<Mutex<Vec<String>>>,
}

impl LoggerProvider for LoggerImplementation {
    fn info<'a>(
        &'a self,
        call: CallContext<LogIntercept>,
        message: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            self.records
                .lock()
                .unwrap()
                .push(format!("{} {:?}", message, call.intercept.0));
            Ok(())
        })
    }
}

struct Consumer;

impl Component for Consumer {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CONSUMER
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let logger = context.context().service::<Logger>().unwrap();
        Activation::Once(Box::pin(async move {
            logger.info("ready".to_owned()).await.unwrap();
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

fn main() {
    block_on(async {
        let records = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::builder(ThreadSpawner).build();
        let root = runtime
            .root()
            .intercept::<Logger>(LogIntercept(vec!["request"]));
        let provider: Arc<dyn LoggerProvider> = Arc::new(LoggerImplementation {
            records: Arc::clone(&records),
        });
        root.provide::<Logger>(provider).await.unwrap();
        let consumer = root.mount(Consumer, ()).await.unwrap();
        consumer.wait_settled().await;
        assert_eq!(
            records.lock().unwrap().as_slice(),
            ["ready [\"component\", \"request\"]"]
        );
        runtime.shutdown().await;
    });
}
