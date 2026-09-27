# 管理资源：启动、失败与清理

[简体中文](reversible-component.md) | [English](../../en/tutorials/reversible-component.md)

组件通常会打开连接、启动任务或登记服务。每个资源准备成功后，立刻登记它的关闭方式。
这样第二步失败时，第一步也有人负责清理。

## 先用普通异步函数

从 `linorun_core` 导入 component、ComponentContext 和 ActivationFailure：

```rust
#[component(id = "app/workspace@1")]
async fn workspace(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    println!("Open workspace");
    ctx.on_cleanup("close workspace", || async {
        println!("Close workspace");
        Ok(())
    }).await?;

    println!("Start worker");
    ctx.on_cleanup("stop worker", || async {
        println!("Stop worker");
        Ok(())
    }).await?;
    Ok(())
}
```

这里用输出演示顺序。真实应用把资源或停止信号移入清理闭包；清理返回前要等待资源关闭。
退出时先输出 Stop worker，再输出 Close workspace。中途返回 Err 也会执行此前已登记的清理。
一个清理失败会记录在报告里，其余清理继续；不会自动重试同一个清理函数。

`on_cleanup` 会等到清理已登记才返回；返回的 EffectHandle 支持提前 dispose。
资源应该在取得后立即登记，避免把几个可能失败的步骤都做完才登记第一个资源。
若关闭恰好发生在资源获取途中，已经开始的启动或 Effect 步骤仍可完成清理登记。
这个保证依赖资源获取和登记都在同一个受跟踪步骤内；过期 Context 不再接受新登记。
后台任务的完整写法见 [application.rs](../../../crates/linorun-core/examples/application.rs)：
清理中发送停止信号并等待任务退出，且消费者清理时旧服务仍能处理请求。

## 1. 何时需要显式步骤

有多个资源并不意味着必须使用 Steps。普通异步组件函数中的整个启动过程是一个不可中断的
生命周期步骤；多个 on_cleanup 负责分别登记资源，不把这个启动过程拆成多个生命周期步骤。

如果你希望运行库在每个启动阶段之间检查依赖是否变化、是否有人请求重启，使用
`Activation::steps(stream)` 或原有 `Activation::Steps`。完整高级示例仍保留在
[reversible_component.rs](../../../crates/linorun-core/examples/reversible_component.rs)：

~~~bash
cargo run -p linorun-core --example reversible_component
~~~


~~~text
step 1: open workspace  -> yield close workspace
step 2: start worker    -> yield stop worker
stream end              -> Fiber Active
~~~


每次 yield 都是一个检查边界：先收下这一阶段的清理动作，再决定继续下一阶段还是回收。

## 2. 构造逐步 stream

以下是手写 Component::activate 中的主要部分。示例用 stream::unfold 保存步骤、配置和执行记录：

~~~rust
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
                    cleanup_trace.lock().unwrap()
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
                    cleanup_trace.lock().unwrap()
                        .push(format!("stop {cleanup_name} worker"));
                    Ok(())
                });
                Some((Ok(inverse), (2, name, trace)))
            }
            _ => None,
        }
    },
)))
~~~


只有资源准备成功，才交出对应 inverse。第一步成功、第二步失败时，第一步已交出的 inverse 仍会执行。
`component_descriptor!` 只负责静态身份、配置与服务声明，步骤和资源所有权由 Rust 代码表达。

## 3. 清理顺序与 accumulator

accumulator 是运行库为本次启动保存的清理列表；seq 是登记顺序。同一 Fiber 同时最多执行一个 inverse。

| seq | 准备资源 | 对应清理 | 退出时顺序 |
| --- | --- | --- | --- |
| 1 | open workspace | close workspace | 2 |
| 2 | start worker | stop worker | 1 |

~~~text
settled at Active
after activation: ["open personal-agent", "start personal-agent worker"]
after disposal:   ["open personal-agent", "start personal-agent worker", "stop personal-agent worker", "close personal-agent"]
~~~


第二个 inverse 完成或报错后，才执行第一个。这一保证覆盖同一组件的所有 Effect，而不是只覆盖某一个列表。

## 4. 启动过程中请求 restart

已经开始的异步步骤会先完成；运行库不会在旁边再启动一个新版本。这称为 inertia：

~~~text
Loading(revision 0, step 1)
    restart: latest revision becomes 1
    step 1 continues running
    step 1 lands and its inverse is committed
Unloading(revision 0)
    inverse runs
Loading(revision 1, new episode)
~~~


完成后收下 inverse，再比较最新 ActivationKey。revision 变了就回收旧启动，之后开始新的启动。
分步组件在每个步骤边界检查；普通异步函数在整个函数完成后检查。

## 5. 启动失败和恢复

stream 可以返回：

~~~rust
Err(ActivationFailure::user("worker failed to start"))
~~~


运行库不再继续 poll stream，先按逆序清理已登记的资源，再进入 Failed。
wait_settled 返回 FiberState::Failed；失败详情在 snapshot 中，不作为等待 API 的 Err 返回。

~~~rust
if fiber.wait_settled().await == FiberState::Failed {
    fiber.restart().await?;
    let retried = fiber.wait_settled().await;
    assert!(matches!(retried, FiberState::Active | FiberState::Failed));
}
~~~


restart、update_config 或 replace 可以显式清除失败。依赖服务反复出现不会自动重试 Failed 组件。

## 6. 清理失败

inverse 返回 Result<(), CleanupError>：

~~~rust
effect::inverse(move || async move {
    stop_worker().await
        .map_err(|error| CleanupError::user(error.to_string()))?;
    Ok(())
})
~~~


错误写入 DisposeReport.cleanup_errors，其他清理继续。运行库至多尝试一次已登记的 inverse；
资源若需要专门的重试策略，由它的关闭函数负责。不要把失败报告解释为外部资源一定已经恢复。

## 7. 资源所有权

输出只演示顺序；真实 inverse 应持有它要关闭的资源：

~~~rust
let worker = start_worker().await?;

Ok(effect::inverse(move || async move {
    worker.stop().await?;
    Ok(())
}))
~~~


Drop 不能替代异步关闭。应用必须等待 dispose 或 shutdown，后台任务的停止和等待也属于清理函数。

继续阅读[服务](typed-service.md)、[组件编写参考](../api/component-authoring.md)或
[Effect 语义](../semantics/effects.md)。自定义 EffectDriver、同步/异步迭代以及原有 Once/Steps 接口全部保留。
