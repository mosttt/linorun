use std::{
    sync::{Arc, Mutex},
    thread,
};

use futures_executor::block_on;
use linorun_core::{
    ActivationFailure, CallContext, Component, ComponentContext, FiberState, Runtime, RuntimeTask,
    TaskSpawner,
};
use linorun_macros::{component, service_contract, service_impl};

service_contract! {
    contract Recorder {
        id: "authoring/recorder@1",
        methods: [
            /// Records a message for the current component episode.
            async fn record(__linorun_call: String) -> Result<(), String>;
            /// Returns the number of recorded messages.
            async fn count() -> usize;
        ],
    }
}

struct RecorderService(Arc<Mutex<Vec<String>>>);

struct RecorderConfig {
    records: Arc<Mutex<Vec<String>>>,
}

#[service_impl]
impl RecorderProvider for RecorderService {
    async fn record(&self, _: CallContext<()>, __linorun_call: String) -> Result<(), String> {
        self.0.lock().unwrap().push(__linorun_call);
        Ok(())
    }

    async fn count(&self, _: CallContext<()>) -> usize {
        self.0.lock().unwrap().len()
    }
}

#[component(id = "authoring/recorder-component@1", provides = [Recorder])]
async fn recorder(
    context: ComponentContext,
    config: Arc<RecorderConfig>,
) -> Result<(), ActivationFailure> {
    context
        .provide::<Recorder>(Arc::new(RecorderService(Arc::clone(&config.records))))
        .await
        .unwrap();
    Ok(())
}

struct WorkerConfig {
    name: String,
    fail: bool,
    cleanups: Arc<Mutex<Vec<String>>>,
}

/// A component whose generated argument names deliberately overlap macro internals.
#[component(id = "authoring/worker@1", requires = [Recorder => ()])]
async fn worker(
    __linorun_context: ComponentContext,
    __linorun_config: Arc<WorkerConfig>,
) -> Result<(), ActivationFailure> {
    let recorder = __linorun_context.service::<Recorder>().unwrap();
    recorder
        .record(__linorun_config.name.clone())
        .await
        .map_err(ActivationFailure::user)?;
    assert!(recorder.count().await > 0);
    for resource in ["first", "second"] {
        let cleanups = Arc::clone(&__linorun_config.cleanups);
        let name = format!("{} {resource}", __linorun_config.name);
        __linorun_context
            .on_cleanup(resource, move || async move {
                cleanups.lock().unwrap().push(name);
                Ok(())
            })
            .await
            .unwrap();
    }
    if __linorun_config.fail {
        return Err(ActivationFailure::user(
            "setup failed after acquiring resources",
        ));
    }
    Ok(())
}

#[component(id = "authoring/empty@1")]
async fn empty(_context: ComponentContext) -> Result<(), ActivationFailure> {
    Ok(())
}

#[derive(Clone, Copy)]
struct ThreadSpawner;

impl TaskSpawner for ThreadSpawner {
    fn spawn(&self, task: RuntimeTask) {
        thread::spawn(move || block_on(task));
    }
}

#[test]
fn async_components_and_provider_call_replace_and_cleanup_downstream() {
    block_on(async {
        let first = worker();
        let replacement = worker();
        assert!(std::ptr::eq(first.descriptor(), replacement.descriptor()));
        let runtime = Runtime::builder(ThreadSpawner).build();
        let records = Arc::new(Mutex::new(Vec::new()));
        let cleanups = Arc::new(Mutex::new(Vec::new()));
        let provider = runtime
            .root()
            .mount(
                recorder(),
                RecorderConfig {
                    records: Arc::clone(&records),
                },
            )
            .await
            .unwrap();
        assert_eq!(provider.wait_settled().await, FiberState::Active);
        let consumer = runtime
            .root()
            .mount(
                first,
                WorkerConfig {
                    name: "worker".to_owned(),
                    fail: false,
                    cleanups: Arc::clone(&cleanups),
                },
            )
            .await
            .unwrap();
        assert_eq!(consumer.wait_settled().await, FiberState::Active);
        consumer.replace(replacement).await.unwrap();
        assert_eq!(consumer.wait_settled().await, FiberState::Active);
        assert_eq!(*records.lock().unwrap(), ["worker", "worker"]);
        assert_eq!(*cleanups.lock().unwrap(), ["worker second", "worker first"]);

        let unit = runtime.root().mount(empty(), ()).await.unwrap();
        assert_eq!(unit.wait_settled().await, FiberState::Active);
        assert!(runtime.shutdown().await.cleanup_errors.is_empty());
        assert_eq!(
            *cleanups.lock().unwrap(),
            [
                "worker second",
                "worker first",
                "worker second",
                "worker first",
            ]
        );
    });
}

#[test]
fn failed_async_setup_rolls_back_registered_resources_and_can_restart() {
    block_on(async {
        let runtime = Runtime::builder(ThreadSpawner).build();
        let records = Arc::new(Mutex::new(Vec::new()));
        let cleanups = Arc::new(Mutex::new(Vec::new()));
        runtime
            .root()
            .provide::<Recorder>(Arc::new(RecorderService(Arc::clone(&records))))
            .await
            .unwrap();
        let consumer = runtime
            .root()
            .mount(
                worker(),
                WorkerConfig {
                    name: "failed".to_owned(),
                    fail: true,
                    cleanups: Arc::clone(&cleanups),
                },
            )
            .await
            .unwrap();
        assert_eq!(consumer.wait_settled().await, FiberState::Failed);
        assert_eq!(*cleanups.lock().unwrap(), ["failed second", "failed first"]);

        consumer
            .update_config(WorkerConfig {
                name: "recovered".to_owned(),
                fail: false,
                cleanups: Arc::clone(&cleanups),
            })
            .await
            .unwrap();
        assert_eq!(consumer.wait_settled().await, FiberState::Active);
        assert_eq!(*records.lock().unwrap(), ["failed", "recovered"]);
        assert!(runtime.shutdown().await.cleanup_errors.is_empty());
        assert_eq!(
            *cleanups.lock().unwrap(),
            [
                "failed second",
                "failed first",
                "recovered second",
                "recovered first",
            ]
        );
    });
}
