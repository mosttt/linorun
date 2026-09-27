# 规范场景目录

简体中文 | [English](../../en/testing/scenario-catalog.md)

所有场景都是 Core 0.1 必测行为。

## E：Effect

### E01 手动 dispose 幂等

- effect 返回 inverse；
- 连续调用 dispose 两次；
- inverse 只调用一次；
- 两个 waiter 得到同一结果。

### E02 Fiber unload 幂等

- Active Fiber 有一个 effect；
- dispose Fiber 两次；
- inverse 只调用一次；
- Fiber 最终 Disposed。

### E03 LIFO

- 依次产生 seq 1、2、3；
- unload 调用顺序为 3、2、1；
- 任意时刻只有一个 inverse Running。

### E04 手动 dispose 后 Fiber unload

- 手动撤销 seq 2；
- Fiber unload；
- seq 2 不再调用；
- 其余 inverse 按降序执行。

### E05 异步单步中 dispose

- forward Future in flight；
- 请求 dispose；
- Future 仍完成；
- 收集 inverse；
- wait_forward 返回 Disposed，不等待 inverse；
- inverse 随后执行；
- EffectDisposeReport 完成并移除 record。

### E06 异步 iterator 停止

- iteration 1 yield inverse 1；
- iteration 2 in flight；
- 请求 dispose；
- iteration 2 落地并 yield inverse 2；
- iteration 3 不启动；
- cleanup 顺序 2、1。

### E07 Activation iterator failure

- Activation::Steps 的 step 1 yield inverse 1；
- step 2 返回 ActivationFailure；
- inverse 1 执行；
- activation 进入 Failed。

### E08 Inverse failure

- seq 3 inverse 返回 error；
- seq 2、1 仍执行；
- report 包含 seq 3 error。

### E09 嵌套 effect 顺序

- outer 产生 seq 1；
- inner 产生 seq 2；
- outer 再产生 seq 3；
- unload 顺序 3、2、1。

### E10 provide 是 effect

- provide 创建 Binding 和 EffectRecord；
- dispose EffectHandle；
- BindingRecord 先从 store 删除；
- dependent Consumer 完成 L-Unload；
- EffectHandle dispose 完成。

### E11 mount 是 effect

- parent mount child，并让 child 到达 Active；
- FiberHandle::dispose 立即提交 O-Retire，并取出未运行的等价 mount entry；
- 保持 child cleanup pending，再 retire parent；parent 的更早 inverse 可以在 child
  Unloading 时运行；
- FiberHandle 继续等待 child cleanup、remove；
- parent.children 清除；
- mount inverse 已从 accumulator 取出，重复 dispose 不创建第二条 retire 路径；
- Host 路径不调用 RetireFiber action，也不能取出任何非匹配 inverse；
- child subtree 的 cleanup failure 出现在该 FiberHandle 的 DisposeReport。

### E12 独立 Effect forward failure

- Scope effect 的 step 1 yield inverse 1；
- step 2 返回 EffectError；
- inverse 1 执行；
- wait_forward 返回 EffectError，EffectHandle dispose report 保存 forward error；
- Fiber lifecycle 保持原状态。

### E13 Inverse panic

- 一个 inverse 在 unwind 中 panic；
- 该 inverse 只记录一个 CleanupFailure；
- 更早 inverse 继续执行；
- report 包含 owner、seq、可选 label 和 panic cause。

### E14 零 inverse effect

- effect forward 直接 Done，未产生 inverse；
- wait_forward 返回 Completed；
- 手动 dispose 直接完成并移除 EffectRecord；
- 不创建占位 InverseEntry，不发送 InverseStarted/InverseFinished；
- Fiber-wide cleanup 对同类 effect 也能收敛。

### E15 Core-owned effect trace

- provide/mount 不创建 EffectDriver 或 StepId，也不发送 EffectYielded；
- provide 依次发送 EffectStarted、BindingInstalled、条件性的 BindingVisible、
  EffectForwardCompleted；
- mount 依次发送 EffectStarted、FiberInserted、EffectForwardCompleted，child LBegin 不早于
  forward completion；
- 两者仍各有一个进入 Episode accumulator 的 Core-owned inverse。

### E16 Effect forward panic

- effect driver 在当前 step poll 中 panic；
- task 把 panic 转成 EffectError，并把 driver 随 completion 归还；
- 已收集 inverse 正常回收，wait_forward 返回 Err；
- Fiber lifecycle 不因独立 effect panic 改变。

## F：Fiber lifecycle

### F01 Pending 等待 Requirement

- Consumer mount 时 service 缺失；
- state Pending；
- Provider Active 后 Consumer Loading；
- activation 完成后 Active。

### F02 Loading Provider 不可见

- Provider 在 Reloading 中已调用 provide；
- Consumer 仍 Pending；
- Provider L-Finish 后 Consumer 才 L-Begin。

### F03 Active Target 丢失

- Consumer Active(P)；
- P L-Leave；
- Consumer L-Leave；
- cleanup 后 Pending。

### F04 Loading Target 丢失

- Consumer Loading(P)，step in flight；
- P 消失且未恢复；
- step 落地；
- Consumer L-Divert；
- cleanup 后 Pending。

### F05 Loading Target 恢复

- Consumer Loading(P)，CommittedView 捕获 Binding B1；
- P 手动 dispose provide effect，Core 删除 B1，Target 暂时为 None；
- 同一 P provide Binding B2，并在 step 落地前恢复 Target；
- completion 时 key 相同；
- Consumer 继续并进入 Active；
- CommittedView 仍为 B1 endpoint；
- B1 WithdrawBinding 完成时不得删除 Slot 中的 B2；
- B1 provide dispose 完成后，Consumer 持有的 endpoint Arc 仍可用。

### F06 Loading Provider 切换

- Consumer Loading(P)；
- Target 变为 Q；
- 当前 step 落地；
- P episode cleanup；
- 新 L-Begin(Q)。

### F07 Restart

- Consumer Active(P, revision 1)；
- restart；
- 完整 L-Leave/L-Unload；
- 新 episode key 为 P, revision 2。

### F08 Config snapshot

- episode E1 捕获 config A；
- Loading 时 update 为 B；
- E1 cleanup 仍持有 A；
- E2 捕获 B。

### F09 Activation failure

- activation 在产生一个 inverse 后失败；
- inverse 执行；
- state Failed；
- sibling Active。

### F10 Failed restart

- Fiber Failed；
- Provider 抖动不触发 activation；
- restart 清除 failure；
- Fiber 重新 Loading。

### F11 Dispose Loading

- Fiber Loading，step in flight；
- O-Retire；
- step 落地；
- L-Divert；
- cleanup 后 O-Remove。

### F12 Dispose Active

- Active -> L-Leave -> L-Unload；
- BindingRecord 在 first inverse 前已从 store 删除；
- O-Retire 提交后 restart/update/replace 返回 FiberDisposed；
- child 清空后 Disposed。

### F13 Active config update

- Active Fiber 收到 update_config；
- 当前 Episode 执行 L-Leave/L-Unload；
- 新 Episode 捕获新 config；
- 旧 Episode 的 CommittedView 和 config 不变；
- 错误 Config TypeId 返回 DescriptorMismatch，旧 config/revision 不变。

### F14 Active implementation replace

- Active Fiber 收到复用同一 `'static` descriptor 的 replace；
- 完整 Leave/Unload/Begin；
- descriptor 实例或 Config TypeId 不同的 replace 返回 DescriptorMismatch，旧 Fiber 不变。

### F15 Failed dispose

- Fiber 已 Failed；
- dispose 仍执行 O-Retire/O-Remove；
- 不重新执行 activation；
- 最终 Disposed。

### F16 Pending dispose

- Requirement 缺失的 Fiber 为 Pending；
- dispose 直接 retire；
- 不创建 Episode；
- children 清空后 Disposed。

### F17 无 Requirement leaf

- Component 没有 Requirement；
- O-Insert 后立即获得空 Target；
- activation 完成后 Active；
- 不注册 dependency watcher。

### F18 activate 同步 panic

- L-Begin 先提交 Init driver 对应的 StepId 与 RuntimeTask ownership；
- 第一 RuntimeTask 调用 `Component::activate` 时同步 panic；
- task 返回 ActivationFailure 和有效的 Exhausted driver；
- Fiber 经 L-Raise/cleanup 到 Failed，Actor 不遗失或重复持有 driver。

## D：Dependency 与 Binding

### D01 Target 使用 ProviderId

- Provider P value 从 1 改为 2；
- Target 不变；
- Consumer 不 reload。

### D02 相等值不同 Provider

- P 和 Q endpoint value 相等；
- Consumer 从 P 切到 Q；
- Target 改变；
- Consumer reload。

### D03 CommittedView 固定

- Consumer Active(P)；
- P 的 BindingRecord 删除，Q 在同 Slot 注册并变为 visible；
- Consumer cleanup 仍调用 P；
- 新 episode 才调用 Q。

### D04 Consumer 先于 Provider

- Consumer cleanup 使用 Provider；
- Provider L-Leave；
- Consumer cleanup 完成；
- Consumer L-Unload；
- Provider first inverse 才开始。

### D05 依赖链

- A provides a；
- B requires a, provides b；
- C requires b；
- A Leave；
- cleanup 顺序 C、B、A。

### D06 依赖分支

- B、C 都依赖 A；
- B/C cleanup 顺序任意；
- A 等二者 L-Unload 后 cleanup。

### D07 手动 Binding withdrawal

- Provider Active 并提供 a、b；
- dispose a；
- 只依赖 a 的 Consumer reload；
- b 保持 visible；
- Provider Fiber 保持 Active。

### D08 Slot 独占

- Slot 已有 Binding；
- 第二次 provide 返回 DuplicateBinding；
- 原 Binding 和 Consumer 不变化。

### D09 Slot 撤销后复用

- 旧 BindingRecord 在 withdrawal 开始时删除；
- 旧 Consumer 仍持有 committed endpoint 并执行 cleanup；
- 新 provide 同时占用已释放的 Slot；
- 旧 WithdrawBinding 完成时按 BindingId 匹配，不删除新 Binding；
- 新 Consumer 解析新 Binding。
- 同 Provider 在 Loading 边界前重新发布同 Slot，原 Episode 与旧 CommittedView 保留；
  原 provide 撤回仍可在该 Episode 恢复 Active 后完成。

### D10 Watcher 不等于依赖

- Pending Fiber watch Slot；
- Provider withdrawal guard 不等待该 Pending Fiber；
- 只有 installed committed Consumer 阻塞 guard。
- 手动撤回只捕获 CommittedView 持有该 BindingId 的 Episode；仅 watch 同 Slot 的
  Loading/Unloading Fiber 也不构成该 Binding 的依赖。

### D11 汇合依赖

- Consumer C 同时 requires A 和 B；
- A 或 B 单独 withdrawal 都使 C 离开；
- C cleanup 完成后对应 Provider 才允许 cleanup；
- 两个 Provider 之间没有额外总顺序。

### D12 互需依赖

- A requires b、provides a，B requires a、provides b；
- A、B 都保持 Pending；
- 不创建 committed dependency edge；
- Runtime 不尝试打破或自动重写该环。

### D13 中间 Provider restart

- A provides a，B requires a and provides b，C requires b；
- B restart；
- C 先离开并释放旧 CommittedView；
- B cleanup 完成后重新 Active，C 再捕获新 B；
- 不出现 Provider 先于 Consumer cleanup。

## C：Context

### C01 不同 Realm

- 两个派生 Context 对同 ServiceId 使用不同 Realm；
- 各自 provide；
- child 分别解析到对应 Provider。

### C02 共享 Realm

- 两个 Context 使用同一 RealmId；
- 它们解析到同一 Binding。

### C03 Intercept 不 reload

- Active Fiber 的派生 Context 增加 metadata；
- Target 和 Episode 不变化；
- 新构造的 typed Client call 使用 overlay。

### C04 内层 intercept 优先

- Requirement metadata、outer overlay、inner overlay；
- contract merge 顺序固定；
- endpoint 收到预期 effective metadata。

### C05 Declared lookup

- Binding store 后来变化；
- 当前 Context::service 返回的 Client 仍使用该 CommittedEntry。

### C06 未声明 service 被拒绝

- child 未声明 service；
- 即使 mount Context 来自 parent，lookup 返回 UndeclaredDependency；
- child 不创建隐藏 dependency edge。

### C07 parent tree 不提供 service capability

- child 与 parent 的 Fiber parent 关系只影响 removal；
- service 必须通过 Requirement 和 CommittedView 获得；
- Provider withdrawal 只等待 declared Consumer。

### C08 Declared dependency ordering

- child 把 service 声明为 Requirement；
- Provider withdrawal 等 child cleanup；
- Context::service 从 child 自己的 CommittedView 构造 typed Client。

### C09 Realm Runtime 归属

- Runtime A 创建 RealmId；
- Runtime B 的 Context 尝试 isolate 到该 Realm；
- 返回 RealmMismatch；
- error.realm 等于传入的 RealmId；
- Context route 不变化。

### C10 Contract type conflict

- Runtime 已把 ServiceId S 注册为 contract type A；
- mount/provide 尝试用同一 S 的 contract type B；
- 命令在产生状态变更前返回 ServiceTypeMismatch；
- payload 为 service S、Contract channel 和 A/B expected/actual TypeId；
- Fiber registry、Binding store 和 route 不变化。

### C11 Intercept contract laws

- 对代表性 metadata 生成 a、b、c；
- 验证 identity 的左右单位元；
- 验证 merge 结合律；
- 验证 Core typed metadata erase/decode 往返；
- declared、outer、inner 的 fold 结果符合 contract 覆盖规则。

### C12 Contract-local typed client

- 在下游 fixture crate 定义 Logger ServiceContract；
- macro 在该 crate 生成 LoggerClient newtype 和 typed methods；
- `Context::service::<Logger>()` 的返回类型为 LoggerClient，调用到 committed endpoint；
- 实现不依赖、也不生成非法的下游 `impl ServiceRef<Logger>` inherent block。

### C13 Isolate 不重选当前 CommittedView

- Consumer 已在 Realm A committed 到 Provider P；
- 从当前 Context 派生 Realm B isolate 后再次 `service::<C>()`，仍得到 P 的 client；
- 使用派生 Context mount child，child 的 Requirement 解析 Realm B 的 Provider Q；
- 当前 Consumer 不 reload，也不创建到 Q 的隐藏 dependency edge。

### C14 显式重设为 default Realm

- Context 外层把 service C isolate 到 Realm B；
- `default_realm()` 返回同 RuntimeTag/local 0；
- 内层 `isolate::<C>(default_realm)` 后 mount child；
- child 的 Requirement 解析 default Realm，而不是外层 Realm B；
- default_realm/create_realm 在 ShuttingDown/Stopped 仍是纯 identity 操作。

## P：Parent/child

### P01 Parent mount child

- parent effect 创建 child；
- parent dispose；
- child 被 retire；
- child 移除后 parent 才移除。

### P02 Parent 与 dependency 交叉

- child 依赖非 parent Provider；
- cleanup 顺序由 committed dependency 决定；
- parent tree 只控制 removal。

### P03 Nested children

- root -> A -> B -> C；
- retire A；
- C、B、A 均最终移除；
- parent.children 双向一致。

## R：并发与 inertia

### R01 Effect dispose 与 step completion

- 两种 Actor 顺序都执行；
- inverse 只收集一次、调用一次；
- 不启动下一 iteration。

### R02 Target 高频抖动

- Loading step in flight；
- Target P/None/P/Q 多次变化；
- 始终只有一个 Episode；
- completion 按落地时 latest key 决策。

### R03 Restart 与 Provider change

- 同一 Loading 期间同时 restart 和 Provider change；
- revision 与 Target 都进入 latest key；
- 旧 episode cleanup 后只启动一个新 episode。

### R04 多 Consumer withdrawal

- 多个 Consumer 同时依赖 Provider；
- Provider 等所有 committed Consumer L-Unload；
- first inverse 只开始一次。

### R05 Cleanup 串行

- inverse 3 in flight 时 seq 2、1 ready；
- seq 2 不启动；
- inverse 3 completion 后才启动 seq 2。

### R06 Snapshot 一致

- snapshot 与 L-Leave/step completion 竞争；
- snapshot 要么看到 turn 前，要么看到 turn 后完整状态；
- Binding visibility 始终与 Provider lifecycle 一致；
- in-flight effect 的 EffectSnapshot.running_step 为对应 StepId，进入 inverse 等待后为 None；
- Running WithdrawBinding 的 InverseSnapshot.waiting_on 精确列出捕获的 Consumer：其
  Episode 仍 installed，且尚未达到 Active、key 与最新 ActivationKey 一致的边界；不包含
  同 Fiber 的后续 Episode。

### R07 Active effect step 与 L-Leave

- Active Fiber 的一个 effect iteration in flight；
- Target 改变并 L-Leave；
- Scope 拒绝新的 effect；
- 当前 iteration 落地并产生 inverse；
- 其他 effect driver 也到达边界后才开始 Fiber cleanup；
- cleanup 仍按最终 seq 降序。

### R08 手动 cleanup 与 Fiber-wide cleanup 汇合

- 手动 dispose 已启动较早 seq 的 inverse，随后 Fiber L-Leave；
- 已运行的 inverse 按 inertia 先落地；
- Unloading 的 Episode-wide request 覆盖其余 entries；
- 随后选择剩余 accumulator entry 中最大 seq；
- 若 L-Leave 先发生、手动 inverse 尚未启动，该请求必须等待 withdrawal guard；
- 所有 inverse 仍至多调用一次。

### R09 update 与 failure-unload

- activation failure 已进入 Unloading；
- 在 L-Unload 完成前提交 update/restart；
- 新 revision 使旧 failure outcome 失效；
- 新 Episode 按最新 config/revision 启动。

### R10 wait_settled

- Pending、Active、Failed、Disposed 分别调用 wait_settled；
- Pending/Failed/Disposed 立即以对应 FiberState 完成；
- Loading/Unloading 等到 settled 边界；
- Active Fiber 即使有独立 Scope effect step in flight，wait_settled 仍返回 Active，但
  Runtime 不 quiescent；
- Failed 不通过 Rust Err 返回。

### R11 Driver continuation ownership

- 一个 Steps driver 的当前 task yield inverse，并仍有 continuation；
- completion 同时把 driver 所有权移交 Actor；
- ActivationYielded/EffectYielded.has_next 表示返回的 driver 是否可继续；
- Actor 提交 has_next Yield 后才把它移入下一 RuntimeTask；
- final Yield、Done、Err，或 Target change/dispose 赢得边界时，返回的 driver 在 post-commit
  阶段析构且不再 poll；
- StepId/RuntimeTask ownership 已提交、但 executor 尚未首次 poll 时再提交 dispose，
  当前 step 仍必须被 poll 到 completion；
- 任意时刻 Actor 与 task 不重复持有 driver，下一 iteration 不越过 boundary 提前开始。

### R12 无 committed 依赖的手动撤回

- A requires S/provides T，B requires T/provides S，各自从独立 Provider 提交依赖后停在 Loading；
- 旧 Provider 撤回后，A/B 复用释放的 Slot 并提供 T/S；新 Binding 没有实际 committed Consumer；
- 手动 dispose 新 Binding 不等待互相 watch 的 A/B；
- 所有已开始的用户 step 落地后，Core inverse 与 shutdown 均可完成。

### R13 撤回等待不跟随新 Episode

- 同一 Binding 被 C、D 的旧 Episode 提交，二者 Loading 时请求撤回；
- 新 Provider 占用释放的 Slot，C 先退出旧 Episode 并在新 Provider 上进入长 Loading；
- D 仍在旧 Episode，旧 WithdrawBinding.waiting_on 此时只包含 D；
- D 退出旧 Episode 后，旧 Binding 撤回完成，无需等待 C 的新 activation；
- 捕获的 EpisodeId 不参与 Target equality，同 Provider 重新发布仍遵守 D09 的惯性规则。

## S：Shutdown

### S01 空 Runtime

- build 的首个事件是 Root FiberInserted(parent None)，且没有 Root LBegin/LFinish；
- shutdown；
- synthetic Root 完成空 accumulator；
- Root latest key 变为 None，cleanup 后保持 Inactive，不重新 L-Begin；
- Running -> ShuttingDown -> Stopped；
- RuntimeShuttingDown 是关闭提交的首事件，RuntimeStopped 是最后事件，Root 不 FiberRemoved；
- report 为空。

### S02 Dependency chain shutdown

- A、B、C Active 且形成依赖链；
- shutdown 使 Root 进入 Unloading，并由 Root mount inverse retire 顶层；mount inverse 不
  等待 child lifecycle；
- cleanup 保持 C、B、A 顺序；
- Runtime Stopped。

### S03 Shutdown cleanup error

- shutdown 前一次正常 reload 已发布 cleanup error；
- 多个 inverse 中一个失败；
- 其余继续；
- Runtime Stopped；
- report 只包含 ShuttingDown 期间的失败，不重复包含旧 episode error。

### S04 重复 shutdown

- 并发调用 shutdown；
- 只执行一次回收；
- 全部 waiter 得到同一 report。

### S05 Root provide shutdown

- Root provide 创建 Host-owned Binding；
- 顶层 Fiber 通过 declared Requirement 使用该 Binding；
- shutdown 先撤回 Consumer，再执行 Root provide inverse；
- Root Episode 完成后 Runtime Stopped。

### S06 DisposeReport 窗口

- child 先 O-Retire，并让较新 inverse 在 parent 仍 Active 时失败；
- 保持 child 的更早 inverse pending，再 O-Retire parent；
- 释放更早 inverse，使它在 parent retire 后失败；
- child DisposeReport 包含两条错误；
- parent DisposeReport 只包含其 O-Retire 之后发生的错误。

### S07 子树错误顺序

- parent 同时拥有两个正在回收的 child；
- 受控完成顺序为 child A 的新 inverse、child B inverse、child A 的旧 inverse；
- parent DisposeReport 按相同的 InverseFinished 提交顺序保存三条错误；
- child 的移除先后不得重排错误。

## A：组件便捷入口

### A01 Cleanup 登记与提前回收

- 同一 ComponentContext 连续 await 多次 on_cleanup；
- 提前 dispose 其中一个 handle，多次 dispose 只执行一次；
- dispose Fiber，剩余 callback 按登记逆序执行；
- 较新的异步 callback 未完成时，较早 callback 不启动；
- Fiber 清理完成后再次 dispose 所有 handle，不重复 callback；
- 已退出组件的 Context 拒绝新的登记。

### A02 派生 Context 与资源归属

- Parent 已 committed 到 default Realm 的 Host service；
- Parent 派生 isolate/intercept，当前 service 仍使用原 CommittedView，新 Client 合成 metadata；
- 派生 Context provide 到 tenant Realm，并分别在 tenant/default Realm mount child；
- child 解析对应 endpoint，metadata 顺序为 declared、outer、inner；
- Binding 与 child 的 owner/parent 仍为原 Fiber；
- Parent dispose 后两个 child 都移除，tenant Consumer cleanup 先于 Provider cleanup；
- child cleanup 仍能使用 committed endpoint，Host Binding 不受影响。

### A03 登记与 shutdown 竞争

- Runtime::new 使用确定性 executor 的任务提交闭包；
- Host effect 已完成 forward；
- on_cleanup 命令已提交、登记 driver 尚未首次 poll 时提交 shutdown；
- 当前登记 step 落地并回收 callback，再回收更早的 Host effect；
- 等待登记调用得到的 handle 已处于 Disposed outcome，重复 dispose 不重复 callback；
- shutdown 后的新登记返回 RuntimeShuttingDown，Runtime 最终 Stopped。
