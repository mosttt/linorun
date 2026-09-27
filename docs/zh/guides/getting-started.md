# 开始使用

[简体中文](getting-started.md) | [English](../../en/guides/getting-started.md)

先把一个组件启动并关闭，再按需增加服务和资源。入门不需要先了解内部状态机。

## 先运行

仓库使用 Rust 1.98.0，`rust-toolchain.toml` 已固定版本。在仓库根目录执行：

```sh
cargo run -p linorun-core --example hello
```

预期输出：

```text
Component started
Component cleaned up
```

这个程序已经完成挂载、启动、等待和关闭。完整源码见 [hello.rs](../../../crates/linorun-core/examples/hello.rs)。

## 写一个完整程序

应用依赖 `linorun-core` 即可使用运行库和宏。假设应用与本仓库是相邻目录：

```toml
[dependencies]
linorun-core = { path = "../linorun/crates/linorun-core" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

把路径换成你的仓库位置。Tokio 是这里选用的异步执行器，也可以接入其他执行器。

```rust
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
```

这里需要理解四件事：

1. `Runtime` 管理应用中的组件。传入的函数把任务交给应用执行器运行。
2. `#[component]` 把异步函数变成组件工厂。`hello()` 创建一个待挂载的组件；`id` 是稳定名称。
3. `ctx.on_cleanup` 登记关闭动作。函数返回前已经完成登记，组件退出时自动执行。
4. `mount` 安装组件并返回控制句柄；`()` 表示没有配置。`shutdown().await` 等待清理结束。

## 安装成功不等于启动完成

`mount(...).await` 只表示组件已经安装。组件可能仍在启动，也可能正在等待缺失的服务。`wait_settled()` 等到当前启动或退出过程到达稳定状态，然后返回实际状态：

| 状态 | 含义 |
| --- | --- |
| `Pending` | 需要的服务尚未就绪，组件还没启动 |
| `Loading` | 正在执行启动步骤 |
| `Active` | 启动成功 |
| `Unloading` | 正在等待或执行清理 |
| `Failed` | 启动失败，已尝试执行已登记的清理 |
| `Disposed` | 组件及其子组件已移除 |

`wait_settled` 返回 Pending、Active、Failed 或 Disposed。它不把 Pending 当成错误，也不等待未来的服务出现。如果需要等待整个运行库完成当前可推进的工作，使用 `runtime.wait_quiescent().await`。

## 资源该怎么关闭

每取得一个需要显式关闭的资源，就立刻登记它的关闭动作。需要等待后台任务退出时，在清理函数里发送停止信号并等待任务结束。多个动作按登记的逆序执行；启动中途失败也会执行已登记的动作。

`on_cleanup` 返回的句柄可以提前关闭该资源。以下片段放在组件函数里：

```rust
let cleanup = ctx.on_cleanup("release resource", || async {
    Ok(())
}).await?;
let report = cleanup.dispose().await;
```

提前关闭和自动关闭共用同一条回收路径，不会重复调用清理函数。具体资源、启动失败和分步执行见[管理资源](../tutorials/reversible-component.md)。

## 更新与故障处理

运行句柄提供 `restart()`、`update_config(config)`、`replace(implementation)` 和 `dispose()`。前三者让组件按正常清理顺序重新启动；`replace` 要求仍属于同一组件描述符。

启动失败的详情可以在 `runtime.snapshot().await` 的对应组件条目中读取。失败不会让其他无关组件退出；修正配置或显式重启后再试。`shutdown` 和 `dispose` 的返回报告包含清理错误，应用应检查。

## 继续运行一个真实应用

```sh
cargo run -p linorun-core --example application
```

[application.rs](../../../crates/linorun-core/examples/application.rs) 使用真实 Tokio 后台任务处理请求，依次演示：等待服务、自动启动、配置更新、实现替换、失败回收、修正后恢复、有序关闭。整个流程靠状态和消息同步，不靠 sleep 猜执行顺序。

接下来按需要选择：

- [使用服务](../tutorials/typed-service.md)：让组件相互调用。
- [组件编写参考](../api/component-authoring.md)：配置、子组件、隔离、显式步骤和自定义操作。
- [概念说明](concepts.md)：把操作与 Fiber、Episode、CommittedView 等术语对应起来。
- [Core API](../api/core-api.md)：查阅完整接口；原有高级能力全部可用。
- [确定性测试](deterministic-testing.md)：精确控制异步步骤并检查结果。
