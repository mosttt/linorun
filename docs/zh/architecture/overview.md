# 架构总览

简体中文 | [English](../../en/architecture/overview.md)

组件作者通常从 `#[component]`、`ComponentContext` 和 `on_cleanup` 开始。本文解释这些入口
背后的协调机制，供需要迭代 Effect、依赖切换或运行时维护的读者使用；便捷入口没有改变这些机制。

## 1. 核心思想

Linorun 把动态组件系统分成两个正交问题：

- 时间方向：组件安装的变化能否在退出时完整撤销；
- 空间方向：组件依赖的服务变化时，组件能否自动激活、退出和重新连接。

Effect 解决时间方向，coeffect/requirement 解决空间方向。Context 把两者放在同一个组件运行环境中。

## 2. 主要对象

    Runtime
      ├── Fiber registry
      ├── Binding store
      └── Dependency watchers

    Fiber
      ├── Component descriptor
      ├── Lifecycle state
      ├── Episode / CommittedView
      ├── Episode accumulator
      └── Child fibers

    Context
      ├── Service lookup
      ├── Isolation route
      └── Interception metadata

Component 是定义，Fiber 是实例。一个 Component 可以产生多个 Fiber；每个 Fiber 独立经历 Pending、Loading、Active、Unloading、Failed 和 Disposed。

## 3. 控制面与用户代码

Rust 多线程环境使用单个逻辑 Runtime Actor 作为线性化点：

- Actor 修改 Fiber、Binding 和 watcher；
- activation step 和 inverse 在 Actor 外执行；
- 完成结果携带 EpisodeId 返回 Actor；
- Actor 不在状态借用或锁内调用/await 用户 Future；移出的 erased value 在释放状态借用后
  通过 deferred-drop 列表析构。

Actor 不是 service data plane。普通 service 调用直接使用 Fiber 的 CommittedView，不逐次经过 Actor。

## 4. 依赖数据流

    Provider effect
        |
        v
    BindingSlot -> BindingRecord
        |
        v
    watcher refresh
        |
        v
    Consumer Target
        |
        v
    L-Begin -> CommittedView

已注册 Binding 对新 Consumer 可见，当且仅当 Provider 已 Active 且未 retired。Loading 或 Unloading Provider 的 Binding 不参与新 Target。

Target 只包含按 Requirement 顺序排列的 Provider FiberId。服务值改变不会触发 reload；Provider 身份改变会触发。

## 5. Episode

一次完整 episode 是：

    Reloading -> Active -> Unloading

它固定持有：

- EpisodeId；
- ActivationKey；
- CommittedView；
- implementation/config snapshot；
- Episode accumulator。

依赖在 episode 中途变化时，只改变最新 Target，不替换 CommittedView。

## 6. Provider/Consumer 顺序

Provider 退出时：

    Provider L-Leave
      -> remove Provider BindingRecord from store
      -> Consumer Target changes
      -> Consumer L-Leave / L-Divert
      -> Consumer cleanup
      -> Consumer leaves installed state
      -> Provider withdrawal guard opens
      -> Provider cleanup

Consumer cleanup 仍能读取旧 CommittedView，因此可以使用旧 Provider 完成资源归还。

这个顺序由 committed dependency 关系表达。

手动撤回单个 provide 时，只捕获 CommittedView 持有该 BindingId 的 Consumer Episode。
捕获的 Episode 离开 installed，或恢复为 key 与最新值一致的 Active 后，撤回等待结束；
同一 Fiber 的后续 Episode 和仅订阅 Slot 的 watcher 不延长等待。同 Provider 重新发布
同 Slot 不改变 Target 身份，原 Episode 可以继续持有旧 endpoint。

## 7. Effect 顺序

每个成功产生 inverse 的 effect step 获得 Fiber 内递增 seq；不产生 inverse 的步骤不创建占位 entry：

    forward 1 -> inverse seq 1
    forward 2 -> inverse seq 2
    forward 3 -> inverse seq 3

回收：

    inverse 3
    inverse 2
    inverse 1

同一 Fiber 的 inverse 串行运行。一个 inverse 失败只记录错误，不跳过剩余 inverse。

## 8. Inertia

异步 step 开始后不能被状态机假装取消：

    Loading step in flight
      + Target changes
      -> step still lands
      -> collect inverse
      -> compare latest ActivationKey
      -> continue or divert

Target 在落地前恢复为原值时，episode 继续。此时不创建新的 Episode。

step 的开始点是 Actor 写入 StepId 并把 driver 转交 RuntimeTask 的提交；不是执行器首次 poll。
因此已调度但尚未 poll 的 step 也遵守 inertia，下一 iteration 只能由下一次 Actor boundary
决定是否提交。

## 9. Context 派生

Context 是不可变值：

    root context
      ├── isolate(service, realm)
      └── intercept(service, metadata)

isolate 改变在派生 Context 上 mount/provide 时使用的 BindingSlot。

intercept 只改变调用 metadata，不改变 BindingSlot、Target 或 Fiber 生命周期。

`Context::service` 只读取当前 Fiber 声明的 Requirement 对应 CommittedEntry，并返回
contract-local typed Client。Fiber parent
只表示 mount/removal 所属关系，不提供隐式 service capability。

在同一 Fiber Context 上后来派生 isolate 不会重选已经 committed 的 endpoint；该 route
只影响后续 mount/provide。intercept overlay 会影响从派生 Context 新构造的 Client。

## 10. 封装边界

以下封装属于最终设计，用于阻止组件绕过 Core 语义：

- Context、ServiceRef 和 BindingRecord 字段私有，组件使用 contract-local Client；
- Provider cleanup 判断集中在内部函数；
- service endpoint 通过统一调用入口访问；
- 详细 lifecycle state 不成为公共可变结构。

## 11. 便捷入口与完整接口

`#[component]` 和 `Activation::run` 省去常见 async setup 的 trait/boxing 样板。ComponentContext
直接提供服务访问、资源登记和子组件操作；`on_cleanup` 在登记完成后返回可提前 dispose 的句柄。
Runtime 可接受任务提交闭包，并直接提供 Root 操作。

需要更精确边界时，仍可使用 `Component`、`Activation::Once/Steps`、`EffectDriver`、
`effect::sync/once/iter/steps`、独立 Context/Scope、RootContext、RuntimeBuilder 和所有快照/事件。
它们与便捷入口共享同一个 Actor、Episode accumulator 和错误/清理协议。
