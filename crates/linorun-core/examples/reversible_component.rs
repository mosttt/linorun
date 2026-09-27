use std::{
    sync::{Arc, Mutex},
    thread,
};

use futures_executor::block_on;
use futures_util::stream;
use linorun_core::{
    Activation, ActivationFailure, Component, ComponentContext, ComponentDescriptor, Runtime,
    RuntimeTask, TaskSpawner, effect,
};
use linorun_macros::component_descriptor;

#[derive(Clone, Copy)]
struct ThreadSpawner;

impl TaskSpawner for ThreadSpawner {
    fn spawn(&self, task: RuntimeTask) {
        thread::spawn(move || block_on(task));
    }
}

#[derive(Clone)]
struct WorkspaceConfig {
    name: String,
}

struct Workspace {
    trace: Arc<Mutex<Vec<String>>>,
}

component_descriptor! {
    static WORKSPACE: WorkspaceConfig {
        id: "example/workspace@1",
        requires: [],
        provides: [],
    }
}

impl Component for Workspace {
    type Config = WorkspaceConfig;

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &WORKSPACE
    }

    fn activate(&self, _: ComponentContext, config: Arc<Self::Config>) -> Activation {
        let state = (0_u8, config.name.clone(), Arc::clone(&self.trace));
        Activation::Steps(Box::pin(stream::unfold(
            state,
            |(step, name, trace)| async move {
                match step {
                    0 => {
                        trace.lock().unwrap().push(format!("open {name}"));
                        let cleanup_name = name.clone();
                        let cleanup_trace = Arc::clone(&trace);
                        let inverse = effect::inverse(move || async move {
                            cleanup_trace
                                .lock()
                                .unwrap()
                                .push(format!("close {cleanup_name}"));
                            Ok(())
                        });
                        Some((Ok::<_, ActivationFailure>(inverse), (1, name, trace)))
                    }
                    1 => {
                        trace.lock().unwrap().push(format!("start {name} worker"));
                        let cleanup_name = name.clone();
                        let cleanup_trace = Arc::clone(&trace);
                        let inverse = effect::inverse(move || async move {
                            cleanup_trace
                                .lock()
                                .unwrap()
                                .push(format!("stop {cleanup_name} worker"));
                            Ok(())
                        });
                        Some((Ok(inverse), (2, name, trace)))
                    }
                    _ => None,
                }
            },
        )))
    }
}

fn main() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::builder(ThreadSpawner).build();
        let fiber = runtime
            .root()
            .mount(
                Workspace {
                    trace: Arc::clone(&trace),
                },
                WorkspaceConfig {
                    name: "personal-agent".to_owned(),
                },
            )
            .await
            .expect("component descriptor is valid");

        println!("settled at {:?}", fiber.wait_settled().await);
        println!("after activation: {:?}", trace.lock().unwrap());

        let report = fiber.dispose().await;
        assert!(report.cleanup_errors.is_empty());
        println!("after disposal:   {:?}", trace.lock().unwrap());

        let report = runtime.shutdown().await;
        assert!(report.cleanup_errors.is_empty());
    });
}
