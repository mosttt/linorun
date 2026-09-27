use linorun_core::{ActivationFailure, ComponentContext, FiberState, Runtime, component};

#[component(id = "example/hello@1")]
async fn hello(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    println!("Component started");
    ctx.on_cleanup("say goodbye", || async {
        println!("Component cleaned up");
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
    let component = runtime.mount(hello(), ()).await?;
    assert_eq!(component.wait_settled().await, FiberState::Active);
    let report = runtime.shutdown().await;
    assert!(report.cleanup_errors.is_empty());
    Ok(())
}
