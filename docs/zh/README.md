# Linorun 文档中心

[简体中文](README.md) | [English](../en/README.md)

从要完成的任务开始，需要控制启动边界或维护运行库时，再查 API、架构和语义参考。各篇文档均提供中英版本。

## 按任务开始

| 文档 | 解决什么问题 |
| --- | --- |
| [开始使用](guides/getting-started.md) | 安装、启动一个组件、等待、关闭 |
| [管理资源](tutorials/reversible-component.md) | 登记清理、失败回收、显式步骤 |
| [使用服务](tutorials/typed-service.md) | 定义接口、实现服务、声明依赖并调用 |
| [组件编写](api/component-authoring.md) | 配置、子组件、Context/Scope、高级操作 |
| [服务契约](api/service-contracts.md) | 宏支持范围、调用元数据、手写契约 |
| [确定性测试](guides/deterministic-testing.md) | 控制异步边界、检查状态与事件 |
| [概念说明](guides/concepts.md) | 从实际任务理解内部术语 |

## 可运行示例

```sh
cargo run -p linorun-core --example hello
cargo run -p linorun-core --example application
cargo run -p linorun-core --example reversible_component
cargo run -p linorun-macros --example typed_service
```

前两个展示普通异步函数和完整应用；后两个保留显式分步激活与手写服务适配的高级用法。

## 完整 API 与诊断

- [Core API](api/core-api.md)
- [可观测性：快照、事件与指标](operations/observability.md)
- [术语表](reference/glossary.md)

## 架构与并发

- [架构总览](architecture/overview.md)
- [数据模型](architecture/data-model.md)
- [Crate 与模块边界](architecture/crate-boundaries.md)
- [Runtime Actor](concurrency/runtime-actor.md)
- [转换协议](concurrency/transition-protocol.md)

## 运行时语义

运行时规范 定义 Core 0.1 的规范；专题文档解释各项规则、不变量和实现约束。

- [运行时规范](reference/runtime-specification.md)
- [Fiber 生命周期](semantics/fiber-lifecycle.md)
- [Effect 与回收](semantics/effects.md)
- [Service 与 Coeffect](semantics/services-and-coeffects.md)
- [Context、隔离与拦截](semantics/context-isolation-interception.md)
- [失败与关闭边界](semantics/failure-shutdown-boundary.md)
- [七条不变量](reference/invariants.md)

## 维护与测试

- [一致性测试策略](testing/conformance-strategy.md)
- [场景目录](testing/scenario-catalog.md)
