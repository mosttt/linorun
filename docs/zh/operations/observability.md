# 可观测性

[简体中文](observability.md) | [English](../../en/operations/observability.md)

## 1. 目标

可观测性必须能回答：

- Runtime 是否静止；
- Fiber 当前状态和 Target；
- Fiber committed 到哪些 Provider；
- 哪个 Provider 正在等待哪些 Consumer；
- 哪个 activation/effect step 或 inverse 正在运行；
- cleanup 是否失败；
- shutdown 进行到哪里。

## 2. RuntimeSnapshot

~~~rust
pub struct RuntimeSnapshot {
    pub sequence: u64,
    pub runtime_state: RuntimeState,
    pub quiescent: bool,
    pub fibers: Vec<FiberSnapshot>,
    pub bindings: Vec<BindingSnapshot>,
    pub effects: Vec<EffectSnapshot>,
}
~~~

Snapshot 在一个 Actor turn 中取得，内部字段相互一致。
DTO 字段公开；调用方修改自己持有的 DTO 不会反向修改 RuntimeCore。

## 3. FiberSnapshot

~~~rust
pub struct FiberSnapshot {
    pub id: FiberId,
    pub parent: Option<FiberId>,
    pub mount_effect: Option<EffectId>,
    pub component: ComponentId,
    pub state: FiberState,
    pub retired: bool,
    pub activation_revision: u64,

    pub latest_key: Option<ActivationKeySnapshot>,
    pub episode: Option<EpisodeSnapshot>,
    pub relied_by: Vec<FiberId>,
    pub children: Vec<FiberId>,
    pub failure: Option<ActivationFailure>,
}
~~~

EpisodeSnapshot：

~~~rust
pub struct EpisodeSnapshot {
    pub id: EpisodeId,
    pub committed_key: ActivationKeySnapshot,
    pub committed_bindings: Vec<BindingId>,
    pub phase: EpisodePhase,
    pub running_step: Option<StepId>,
    pub accumulator: Vec<InverseSnapshot>,
    pub next_cleanup_seq: Option<u64>,
    pub cleanup_failures: Vec<CleanupFailure>,
}

pub struct ActivationKeySnapshot {
    pub providers: Vec<FiberId>,
    pub revision: u64,
}

pub enum EpisodePhase {
    Loading,
    Active,
    WaitingWithdrawal,
    Cleaning,
}
~~~

FiberSnapshot.failure 在 Failed 时投影 Inactive 中保存的 ActivationFailure；在 L-Raise 后的
Unloading 期间，仅当 outcome revision 仍等于 activation_revision 时投影 pending failure；
其他状态为 None。

EpisodePhase 映射：Reloading 为 Loading，Active 为 Active；Unloading 且已有 Running inverse
时为 Cleaning，其余为 WaitingWithdrawal。`running_step` 只是 activation
step；独立 effect step 位于 EffectSnapshot。`next_cleanup_seq` 是当前 scheduler 若能立即选择
时的最大 requested Armed seq，guard/boundary 未开或集合为空时为 None。
WaitingWithdrawal 同时覆盖等待 committed Consumer 和等待 episode forward step boundary；
具体阻塞者分别由 `relied_by` 与 EffectSnapshot.running_step 给出。

## 4. BindingSnapshot

~~~rust
pub struct BindingSnapshot {
    pub id: BindingId,
    pub service: ServiceId,
    pub realm: RealmId,
    pub provider: FiberId,
    pub visible: bool,
    pub committed_consumers: Vec<FiberId>,
    pub owner_effect: EffectId,
}
~~~

BindingSnapshot.committed_consumers 只包含 CommittedEntry.binding 等于该 BindingId 的 Fiber；FiberSnapshot.relied_by 来自 consumers_by_provider。

## 5. EffectSnapshot

~~~rust
pub struct EffectSnapshot {
    pub id: EffectId,
    pub fiber: FiberId,
    pub episode: EpisodeId,
    pub label: Arc<str>,
    pub state: EffectSnapshotState,
    pub running_step: Option<StepId>,
    pub forward_error: Option<EffectError>,
    pub inverses: Vec<InverseSnapshot>,
}

pub enum EffectSnapshotState {
    Applying,
    Armed,
    Disposing,
}

pub struct InverseSnapshot {
    pub seq: u64,
    pub owner: CleanupOwner,
    pub label: Option<Arc<str>>,
    pub state: InverseSnapshotState,
    pub waiting_on: Vec<FiberId>,
}

pub enum InverseSnapshotState {
    Armed,
    Running,
}

~~~

EffectSnapshot 的 inverses 由 Episode.accumulator 和 `cleanup_running` 中 owner 为该 EffectId
的 entries 投影。
Applying 表示 driver 尚未到终止 boundary；Armed 表示 forward 已自然完成、等待 cleanup；
Disposing 表示 disposal 已请求且仍有 step/action 未完成。EffectCompletion 发布与 live record
移除在同一 Actor turn 完成，不进入 snapshot enum。
`running_step` 精确给出当前 in-flight Effect step；Applying driver 在两个 Actor turn 之间
始终由一个 RuntimeTask 持有，因此该字段为 Some，Armed 时为 None。Disposing 在等待当前
step 时为 Some，在等待 inverse 时为 None。
InverseSnapshot.waiting_on 只在 Running Withdraw 中投影仍需等待的已提交消费者，并按 FiberId
排序。删除 Binding 时仅捕获真正引用该 BindingId 的 `(FiberId, EpisodeId)`；仅声明需求的
watcher 不进入等待集合。User/RetireFiber action 及非 Running entry 的列表为空。
捕获的 Episode 已结束，或仍是该 Episode 且 Active、ActivationKey 与最新意图一致时，等待完成。
不跟随消费者后续的 Episode，也不等待其后代移除。这不同于公共 `wait_settled`。
Fiber-wide guard 尚未选择 inverse 时，由 FiberSnapshot.relied_by 表示阻塞它的 committed Consumers。
稳定排序键为 `(FiberId, has_no_inverse, min_seq_or_zero, EffectId)`：`has_no_inverse=false`
先排；没有 inverse 时第三项固定为 0，因此在同一 Fiber 内排在有 inverse 的 effect 之后。

## 6. Event

Actor 在状态提交后发布事件：

~~~rust
pub struct RuntimeEvent {
    pub sequence: u64,
    pub kind: RuntimeEventKind,
}
~~~

精确事件 payload：

~~~rust
pub enum RuntimeEventKind {
    RuntimeShuttingDown,
    RuntimeStopped,

    FiberInserted {
        fiber: FiberId,
        parent: Option<FiberId>,
        component: ComponentId,
    },
    FiberRetired { fiber: FiberId },
    FiberRemoved { fiber: FiberId },

    LBegin {
        fiber: FiberId,
        episode: EpisodeId,
        key: ActivationKeySnapshot,
    },
    LIter {
        fiber: FiberId,
        episode: EpisodeId,
        completed_step: StepId,
    },
    LFinish {
        fiber: FiberId,
        episode: EpisodeId,
        completed_step: StepId,
    },
    LDivert {
        fiber: FiberId,
        episode: EpisodeId,
        completed_step: StepId,
        committed: ActivationKeySnapshot,
        latest: Option<ActivationKeySnapshot>,
    },
    LRaise {
        fiber: FiberId,
        episode: EpisodeId,
        completed_step: StepId,
        error: ActivationFailure,
    },
    LLeave {
        fiber: FiberId,
        episode: EpisodeId,
        committed: ActivationKeySnapshot,
        latest: Option<ActivationKeySnapshot>,
        removed_bindings: Vec<BindingId>,
    },
    LUnloadStarted { fiber: FiberId, episode: EpisodeId },
    LUnloadFinished { fiber: FiberId, episode: EpisodeId },

    ActivationYielded {
        fiber: FiberId,
        episode: EpisodeId,
        step: StepId,
        seq: u64,
        has_next: bool,
    },

    BindingInstalled { binding: BindingEventRef },
    BindingVisible { binding: BindingEventRef },
    BindingRemoved { binding: BindingEventRef },

    EffectStarted {
        fiber: FiberId,
        episode: EpisodeId,
        effect: EffectId,
        label: Arc<str>,
    },
    EffectYielded {
        effect: EffectId,
        step: StepId,
        seq: u64,
        has_next: bool,
    },
    EffectForwardCompleted {
        effect: EffectId,
        outcome: EffectForwardOutcome,
    },
    EffectForwardFailed { effect: EffectId, error: EffectError },
    EffectDisposeRequested { effect: EffectId },
    InverseStarted {
        fiber: FiberId,
        episode: EpisodeId,
        seq: u64,
        owner: CleanupOwner,
        label: Option<Arc<str>>,
    },
    InverseFinished {
        fiber: FiberId,
        episode: EpisodeId,
        seq: u64,
        owner: CleanupOwner,
        label: Option<Arc<str>>,
        error: Option<CleanupError>,
    },
}

pub struct BindingEventRef {
    pub id: BindingId,
    pub service: ServiceId,
    pub realm: RealmId,
    pub provider: FiberId,
    pub owner_effect: EffectId,
}
~~~

sequence 表示 Actor 提交顺序，不表示墙钟时间。
RuntimeBuilder 的初始提交发送 synthetic Root 的 FiberInserted，parent 为 None；Root 直接
构造为 Active，不发送 LBegin/LFinish。
一个 reducer 提交产生多个事件时，Actor 按动作顺序连续分配 sequence：activation yield
先于 LIter/LFinish，final EffectYielded 先于 EffectForwardCompleted，LFinish 先于
BindingVisible，Provider LLeave 先于其 BindingRemoved，
LDivert/LRaise 也先于它们删除的 BindingRemoved；BindingRemoved 先于由 watcher 触发的
Consumer LLeave/LDivert，LUnloadStarted 先于该次
Fiber-wide cleanup 的第一个 InverseStarted，最后一个 InverseFinished 先于
LUnloadFinished。Sender 的发送顺序与 sequence 相同。

第一次 shutdown 提交中 RuntimeShuttingDown 是首个事件，随后才是 Root 的 lifecycle/
Binding 事件；全部非 Root FiberRemoved 且 Root LUnloadFinished 后，RuntimeStopped 是关闭
过程的最后一个事件。Root 不发送 FiberRemoved。

## 7. 事件内容

`LRaise.error` 表示 activation failure；带 error 的 InverseFinished 表示 cleanup failure。
BindingRecord 退出解析时由 BindingRemoved 表示。
ActivationYielded/EffectYielded 的 `has_next` 来自同一 completion：true 表示返回 Actor 的
driver 可继续，false 表示这是 final Yield；Done 不发送 Yielded。

Core-owned provide/mount 没有用户 driver 或 StepId，因此不发送 EffectYielded。它们的成功
提交顺序固定为：

~~~text
provide: EffectStarted < BindingInstalled < [BindingVisible] < EffectForwardCompleted
mount:   EffectStarted < FiberInserted < EffectForwardCompleted < [child LBegin]
~~~

方括号事件只在对应条件成立时出现；Core effect 的 inverse seq 可从 Effect/Inverse snapshot
及其后续 InverseStarted 观察。

每个 EffectId 在第一次进入 Disposing 时恰好发送一次 EffectDisposeRequested，包括 manual
dispose、Fiber-wide unload 和 forward failure 触发的自动 cleanup。forward failure 的顺序为
EffectForwardFailed < EffectDisposeRequested；L-Divert/L-Raise/L-Leave 路径先发送对应
lifecycle event，再发送该 Fiber 的 BindingRemoved，随后按 EffectId 发送
EffectDisposeRequested，最后才出现 watcher 引发的 Consumer lifecycle event。已经
Disposing 或已移除的 effect 不重复发送。手动 provide cleanup 若实际删除 Binding，则按
BindingRemoved、InverseStarted 的顺序发送。

FiberHandle::dispose 第一次赢得 O-Retire 竞争时发送 FiberRetired；若对应 mount effect 此前
尚未进入 Disposing，则先发送它唯一的 EffectDisposeRequested。该 Host orchestration 已建立
RetireFiber action 的结果，精确匹配的 accumulator entry 被直接取出，因此这条路径没有
InverseStarted/InverseFinished；若父级进入 Unloading 时已经发送 request，Host 不重复发送。
重复 dispose 或 child 已 retired 后也不重复发送 retire。父 accumulator 实际选择
RetireFiber 时，顺序为 InverseStarted、FiberRetired、InverseFinished。

## 8. Trace 偏序

Provider Fiber-wide withdrawal 的事件流必须能验证：

~~~text
BindingRemoved
    < Consumer LLeave or LDivert
    < Consumer LUnloadFinished
    < Provider InverseStarted
~~~

以及：

~~~text
EffectYielded(seq N)
    < InverseStarted(seq N)
    < InverseFinished(seq N)
    < InverseStarted(next selected lower seq)
~~~

## 9. Metrics

Host 从 RuntimeEvent 和 RuntimeSnapshot 派生以下 metrics；Core 0.1 不内置 metrics registry。

Gauge：

- runtime_fibers；
- fibers_by_state；
- registered_bindings；
- visible_bindings；
- running_activation_steps；
- running_effect_steps；
- running_inverse_tasks；
- committed_dependency_edges。

Counter：

- activations_started；
- activations_completed；
- activations_diverted；
- activation_failures；
- effect_forward_failures；
- inverse_calls；
- cleanup_failures；
- revision_driven_reactivations。

Histogram：

- activation_step_duration；
- episode_loading_duration；
- consumer_drain_duration；
- inverse_duration；
- shutdown_duration。

`revision_driven_reactivations` 统计 committed/latest key 的 revision 不同而发生的
LLeave/LDivert，不声称区分 restart、config update 和 implementation replace。Event 不携带
墙钟；Histogram 由 Receiver 在收到边界事件时读取 Host monotonic clock，因此表示 Host
观察时长。

## 10. Quiescence

Snapshot.quiescent 按转换协议统一计算：

- Actor state 已到 fixed point；
- running activation/effect step 空；
- running inverse 空；
- 没有 Reloading 或 Unloading Fiber；
- Active key 与 latest key 一致；
- Pending 无 key；
- Failed 即 `Inactive { failure: Some(_) }`，视为 settled；
- registry 中没有 retired 但尚未移除的 Fiber；
- 没有可立即执行的 O-Remove。

Failed Fiber 可以存在于 quiescent Runtime。

## 11. 日志

日志由 event Sender 的 Receiver 侧处理。Actor 只构造结构化 RuntimeEvent 并发送，不调用
用户日志 callback。

错误日志使用叶子错误的 FailureKind/message 和事件中的定位字段；普通状态事件不需要
error-level 日志。

## 12. 排序

Snapshot 输出采用稳定排序：

- Fiber 按 FiberId；
- Binding 按 ServiceId、RealmId、BindingId；
- Effect 按 FiberId、`has_no_inverse`、`min_seq_or_zero`、EffectId；
- Episode/Effect 内的 InverseSnapshot 按 seq；
- Consumer 按 FiberId；
- Error 按实际 inverse 执行顺序。

稳定排序便于测试、diff 和问题复现。
