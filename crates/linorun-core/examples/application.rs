//! A real async worker, a dependent component, live updates, failure recovery, and ordered shutdown.

use std::sync::{Arc, Mutex};

use linorun_core::{
    ActivationFailure, CallContext, CleanupError, ComponentContext, FiberState, Runtime, component,
    service_contract, service_impl,
};
use tokio::sync::{mpsc, oneshot};

type Trace = Arc<Mutex<Vec<String>>>;

fn record(trace: &Trace, message: String) {
    println!("{message}");
    trace.lock().unwrap().push(message);
}

service_contract! {
    contract Counter {
        id: "example/counter@1",
        methods: [
            /// Gets the next value from the background worker.
            async fn next() -> Result<String, String>;
        ],
    }
}

struct CounterService {
    requests: mpsc::Sender<oneshot::Sender<String>>,
}

#[service_impl]
impl CounterProvider for CounterService {
    async fn next(&self, _call: CallContext<()>) -> Result<String, String> {
        let (reply, value) = oneshot::channel();
        self.requests
            .send(reply)
            .await
            .map_err(|_| "counter service stopped".to_owned())?;
        value
            .await
            .map_err(|_| "counter service did not reply".to_owned())
    }
}

struct CounterConfig {
    name: &'static str,
    fail_setup: bool,
    trace: Trace,
}

#[component(id = "example/counter-worker@1", provides = [Counter])]
async fn counter(
    ctx: ComponentContext,
    config: Arc<CounterConfig>,
) -> Result<(), ActivationFailure> {
    let name = config.name;
    let trace = Arc::clone(&config.trace);
    let (requests, mut incoming) = mpsc::channel::<oneshot::Sender<String>>(16);
    let (stop, mut stopped) = oneshot::channel();
    let worker = tokio::spawn(async move {
        let mut count = 0;
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                request = incoming.recv() => {
                    let Some(reply) = request else { break };
                    count += 1;
                    let _ = reply.send(format!("{name}:{count}"));
                }
            }
        }
    });

    ctx.on_cleanup("stop counter worker", move || async move {
        let _ = stop.send(());
        worker
            .await
            .map_err(|error| CleanupError::user(error.to_string()))?;
        record(&trace, format!("Provider {name}: worker stopped"));
        Ok(())
    })
    .await?;

    if config.fail_setup {
        return Err(ActivationFailure::user("demonstrating a setup failure"));
    }

    ctx.provide::<Counter>(Arc::new(CounterService { requests }))
        .await?;
    record(&config.trace, format!("Provider {name}: service published"));
    Ok(())
}

#[component(id = "example/consumer@1", requires = [Counter])]
async fn consumer(ctx: ComponentContext, trace: Arc<Trace>) -> Result<(), ActivationFailure> {
    let counter = ctx.service::<Counter>()?;
    let value = counter.next().await.map_err(ActivationFailure::user)?;
    record(&trace, format!("Consumer started: {value}"));

    ctx.on_cleanup("finish consumer", move || async move {
        let value = counter.next().await.map_err(CleanupError::user)?;
        record(
            &trace,
            format!("Consumer cleanup: old service still works: {value}"),
        );
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Runtime::new(|task| {
        tokio::spawn(task);
    });
    let trace = Trace::default();
    let config = |name, fail_setup| CounterConfig {
        name,
        fail_setup,
        trace: Arc::clone(&trace),
    };

    println!("1. Mount the consumer: it waits for its missing service");
    let client = runtime.mount(consumer(), Arc::clone(&trace)).await?;
    assert_eq!(client.wait_settled().await, FiberState::Pending);

    println!("2. Mount the provider: the consumer starts automatically");
    let provider = runtime.mount(counter(), config("v1", false)).await?;
    runtime.wait_quiescent().await;
    assert_eq!(client.state().await, FiberState::Active);

    println!("3. Update config: clean up the consumer, stop the old service, then restart");
    provider.update_config(config("v2", false)).await?;
    runtime.wait_quiescent().await;
    assert_eq!(client.state().await, FiberState::Active);

    println!("4. Replace the implementation: the component handle stays valid");
    provider.replace(counter()).await?;
    runtime.wait_quiescent().await;
    assert_eq!(client.state().await, FiberState::Active);

    println!("5. Fail during setup: the already-started worker is cleaned up");
    provider.update_config(config("failed", true)).await?;
    let snapshot = runtime.wait_quiescent().await;
    assert_eq!(provider.state().await, FiberState::Failed);
    assert_eq!(client.state().await, FiberState::Pending);
    let failure = snapshot
        .fibers
        .iter()
        .find(|fiber| fiber.id == provider.id())
        .unwrap();
    println!("Failure details: {:?}", failure.failure);

    println!("6. Correct the config: provider and consumer recover");
    provider.update_config(config("v3", false)).await?;
    runtime.wait_quiescent().await;
    assert_eq!(client.state().await, FiberState::Active);

    println!("7. Shut down: wait for every component and worker to finish cleanup");
    let report = runtime.shutdown().await;
    assert!(report.cleanup_errors.is_empty());
    assert_eq!(client.state().await, FiberState::Disposed);
    assert_eq!(provider.state().await, FiberState::Disposed);

    let events = trace.lock().unwrap();
    let consumer_cleanup = events
        .iter()
        .position(|s| s == "Consumer cleanup: old service still works: v1:2")
        .unwrap();
    let provider_cleanup = events
        .iter()
        .position(|s| s == "Provider v1: worker stopped")
        .unwrap();
    assert!(consumer_cleanup < provider_cleanup);
    assert!(
        events
            .iter()
            .any(|s| s == "Provider failed: worker stopped")
    );
    println!("Verified dependency reload, failure recovery, and resource cleanup");
    Ok(())
}
