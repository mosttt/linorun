# Linorun

[简体中文](README.zh-CN.md) | [English](README.md)

Linorun 是一个 Rust 组件运行库：组件需要的服务就绪后自动启动，停止时自动清理已登记的资源。服务提供者发生变化时，依赖它的组件会按正确顺序重新启动。

你编写普通异步函数，声明需要的服务，并登记资源的关闭方式；Linorun 负责启动、依赖变化、失败回收和整体关闭。

```rust
use linorun_core::{ActivationFailure, ComponentContext, component};

#[component(id = "app/worker@1")]
async fn worker(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    println!("Worker started");
    ctx.on_cleanup("stop worker", || async {
        println!("Worker stopped");
        Ok(())
    }).await?;
    Ok(())
}
```

先运行最小完整示例，再运行包含真实异步任务、服务更新和失败恢复的应用：

```sh
cargo run -p linorun-core --example hello
cargo run -p linorun-core --example application
```

- [开始使用](docs/zh/guides/getting-started.md)：接入方式、完整程序与资源清理。
- [使用服务](docs/zh/tutorials/typed-service.md)：定义并调用类型化服务。
- [管理资源](docs/zh/tutorials/reversible-component.md)：分步准备、失败回收与显式步骤。
- [文档中心](docs/zh/README.md)：按任务阅读，也可查阅完整高级参考。

全部宏均可从 `linorun_core` 导入。运行库接入应用现有的异步执行器；示例使用 Tokio，库本身不要求 Tokio。

工作区还包含 `linorun-macros` 与 `linorun-testkit`。高级用法包括手写 Component、分步激活、自定义 Effect driver、Context/Scope、服务隔离、调用元数据、快照、事件和确定性调度。运行时语义由 [运行时规范](docs/zh/reference/runtime-specification.md) 定义。
