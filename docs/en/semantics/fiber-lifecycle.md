[简体中文](../../zh/semantics/fiber-lifecycle.md) | [English](fiber-lifecycle.md)

# Fiber lifecycle

## 1. Component and Fiber

Component is a definition; Fiber is an instance. Each Fiber owns its:

- Parent.
- Descriptor.
- Config and implementation snapshots.
- Route.
- Lifecycle state.
- Episode accumulator.
- Child set.

Multiple Fibers may share one Component implementation while their lifecycles remain completely
independent.

Runtime also contains a synthetic Root Fiber. It has no ordinary Component activation, starts
Active, and owns all top-level effects created by RootContext. While Running, its key is fixed
to an empty Target/revision 0. In ShuttingDown/Stopped, its key is None. After cleanup, Root
remains Inactive: it neither activates again nor undergoes O-Remove.

## 2. Four internal states

~~~text
Inactive
   |
   | L-Begin
   v
Reloading
   |       |
   |       | L-Divert / L-Raise
   |       v
   |    Unloading
   |
   | L-Finish
   v
Active
   |
   | L-Leave
   v
Unloading
   |
   | L-Unload
   v
Inactive
~~~

Public states are projected in the following priority order; removed and retired conditions take
precedence over ordinary Inactive:

| Internal state | Public state |
|---|---|
| Removed from registry | Disposed |
| Inactive, retired, still in registry | Unloading |
| Inactive { failure: None } | Pending |
| Reloading | Loading |
| Active | Active |
| Unloading | Unloading |
| Inactive { failure: Some(_) } | Failed |

When `Inactive { failure: Some(_) }` is also retired, it appears as Unloading until removal.
Failed is settled and may also exist in a quiescent Runtime.

## 3. Target

A Fiber's latest ActivationKey is determined jointly by:

- retired flag;
- Requirement resolution;
- activation revision;

All three participate in determining the key.

There is no ActivationKey if the Fiber is retired or any Required service is missing.

Target itself contains only Provider FiberIds; revision is a separate field of ActivationKey.

## 4. O-Insert

To create a Fiber:

1. Allocate FiberId.
2. Set parent to an existing Fiber or Root.
3. Initialize Lifecycle revision to 0 and state to `Inactive { failure: None }`.
4. Initialize next_inverse_seq to 1 and `cleanup_running` to None.
5. Register watchers.
6. Add the Fiber to parent.children.
7. Reconcile.

The Fiber remains Pending while dependencies are unsatisfied.

## 5. L-Begin

When a Pending Fiber has an ActivationKey:

1. Capture Bindings.
2. Construct CommittedView, retaining validated declared interception metadata.
3. Capture implementation/config.
4. Allocate EpisodeId.
5. Create an empty accumulator.
6. Enter Reloading.
7. Construct an Init driver. After the Actor commits, the first RuntimeTask calls `Component::activate` and polls the first activation step.

From this moment the Fiber is installed, preventing its Provider from cleaning up too early.

## 6. Reloading

Reloading can produce three kinds of results.

### 6.1 L-Iter

If completion is `Yield { inverse, has_next: true }`, returns the driver, and the latest
ActivationKey still equals episode.key:

- Collect the inverse.
- Remain Reloading.
- The Actor moves the returned driver into the next RuntimeTask.

### 6.2 L-Finish

If completion is a final Yield or Done, and the key still matches:

- Collect the inverse.
- Enter Active.
- Make Provider Bindings visible to new Consumers.
- Refresh watchers.

### 6.3 L-Divert

If the key differs at the boundary:

- First collect any inverse carried by the current successful completion.
- Do not transfer the returned driver to another RuntimeTask.
- Enter Unloading.
- Remove BindingRecords registered by this Fiber.
- Mark every live Applying/Armed effect in this Episode Disposing.
- Drop the returned activation driver after committing state.

If the key becomes equal again before the async step completes, do not divert.

A step starts when the Actor commits StepId and transfers its driver to RuntimeTask. Even if the
executor has not polled it yet, subsequent key changes must wait for its completion.

### 6.4 L-Raise

If the step returns an error:

- Stop activation.
- Enter Unloading.
- Remove BindingRecords registered by this Fiber.
- Retain the failure in the outcome.
- Mark every live Applying/Armed effect in this Episode Disposing.
- Drop the returned activation driver after committing state.
- Clean up the accumulator.

## 7. Active

Active Fiber:

- It makes registered Bindings available to new Consumers.
- It uses a fixed CommittedView.
- It may create ordinary effects.
- It waits for the latest ActivationKey to change.

The following actions change ActivationKey:

- A change in Provider identity.
- A Requirement becoming unsatisfied.
- Config update.
- Implementation update.
- Restart.
- Retire.

## 8. L-Leave

When an Active episode.key differs from the latest key:

1. Enter Unloading.
2. Remove the Provider's registered BindingRecords, allowing immediate Slot reuse.
3. Refresh Consumers.
4. Mark all live EffectRecords in this Episode Disposing. Applying drivers finish only their current step.
5. Wait for in-flight steps and the withdrawal guard.

Leave does not run inverses. It changes only the component's topology visibility and lifecycle state.

While an already-started activation/effect step finishes, it may still register an acquired
resource's inverse through the dedicated `RegisterCleanup` command. After shutdown or retirement,
this finishing operation is allowed only in the same Episode, before unload cleanup has started,
and while a step remains in flight. The inverse enters the same accumulator and cleanup process;
this does not reopen new effects, provisions, or mounts.

## 9. Unloading

Unloading has two logical stages:

### 9.1 Waiting

Wait for every installed Consumer whose CommittedView points to this Provider to finish L-Unload.

Each Consumer still retains its old CommittedView.

### 9.2 Recovering

After the guard opens:

- Wait for every effect driver in this Episode to reach a boundary.
- Execute inverses in descending seq order.
- Directly finish requested EffectRecords with no entries in accumulator/cleanup_running. Zero-inverse effects do not produce artificial inverses.
- Record cleanup errors.
- Clear live EffectRecords.
- Drop CommittedView.
- Enter Inactive.

LifecycleState::Unloading, `cleanup_running`, and effect states jointly represent these two stages.

## 10. After Unloading

### Normal exit

- Retired: wait for children to clear, then perform O-Remove.
- Latest key is None: remain Pending.
- Latest key is Some: immediately L-Begin a new episode.

### Activation failure

Enter `Inactive { failure: Some(error) }`, exposed publicly as Failed.

Failed does not retry automatically. Restart, config update, or implementation replace clears failure.

### Cleanup error

Record the error through InverseFinished and the applicable Effect/Fiber/shutdown collectors,
then still reach the normal final outcome.

## 11. O-Retire and O-Remove

Disposing a Fiber first performs O-Retire:

    retired = true
    latest key = None

An Active Fiber performs normal L-Leave. A Reloading Fiber performs L-Divert at its step boundary.

A Fiber undergoes O-Remove only when all of the following hold:

- It is retired.
- It is Inactive.
- Effects have finished.
- Bindings have been removed.
- Its children set is empty.

After O-Remove, FiberHandle reports Disposed.

A public disposal request directly performs O-Retire. It may take from the parent's accumulator
only a mount entry that has not started and whose action exactly matches `RetireFiber(child)`;
it cannot satisfy other inverses. FiberHandle separately waits for FiberCompletion. If the
parent scheduler already selected the entry, the public request only waits. Repeated disposal
shares the child's completion and leaves no second Armed retirement path.

Once the Actor commits O-Retire, restart/update/replace and new Scope mutations return
FiberDisposed/InactiveScope. State, wait_settled, and repeated disposal continue to work.
The finishing cleanup registration described above remains available to an in-flight step of
the same Episode; it does not apply to an expired Scope.

## 12. Settled and quiescent

A Fiber's `wait_settled` completes after immediately runnable lifecycle reconciliation finishes
and one of these boundaries is reached:

- Inactive, without failure or ActivationKey (Pending).
- Active, with episode.key equal to the latest ActivationKey.
- Inactive, with failure (Failed).
- Removed from the registry (Disposed).

Unloading, Reloading, and retired Fibers still waiting for children are not settled.
Runtime's `wait_quiescent` additionally requires every Fiber to satisfy the conditions above,
with no activation/effect forward step, inverse, reconcile work, or pending O-Remove.

Settled describes only Fiber lifecycle. An independent Scope effect's forward or manual cleanup
may still be running on an Active Fiber. Its `wait_settled` may complete while Runtime remains
non-quiescent.

Manual provide withdrawal notifies every relevant watcher but waits only for Consumer Episodes
committed to the exact withdrawn BindingId. The wait ends when the captured Episode ends
(including internally Inactive, a later Episode, or removal); later Episodes do not extend it.
The same Episode also qualifies if it becomes Active with a current key. This uses neither the
public settled condition above nor child O-Remove. A parent watcher that never committed this
Binding cannot block its child's manual withdrawal.
See [Service withdrawal](services-and-coeffects.md#8-withdrawal) for the full conditions.

## 13. Parent/child

Mounting a child is an effect of the parent Fiber. Parent unload selects its inverse by
accumulator seq and retires the child. The Host may also O-Retire it earlier through FiberHandle
and take the exactly matching entry. In either case, the parent may continue with its earlier
inverses. Only parent O-Remove waits for the child to leave the registry.

The parent tree and dependency graph represent different relationships:

- Parent expresses who created whom.
- A committed dependency expresses who must clean up before whom.

A parent relationship alone does not automatically establish service cleanup ordering.
Use a Requirement when ordering is needed.

## 14. Restart

restart:

1. Clear activation failure.
2. Increment activation_revision.
3. If currently Unloading, invalidate any failure outcome not yet committed.
4. Reconcile.

An Active Fiber performs a full Leave/Unload/Begin; it does not call activate again in place.

A Loading Fiber does not start another activation in parallel. It diverts after the current step
completes, then Begins with the new revision.

## 15. Config and implementation updates

An update writes the Fiber's latest snapshot and increments revision.

The current Episode retains its old snapshot until L-Unload finishes. The new Episode captures
the new snapshot.

If failure is waiting in Unloading, L-Unload writes it into Inactive only when its failure
revision still equals the latest activation_revision. An old failure therefore cannot overwrite
an update.

Descriptor and route are fixed for a Fiber's lifetime. `replace` requires the same `'static`
descriptor; `update_config` requires the input TypeId to equal descriptor.config_type.
When Requirements, provides, Config type, or route changes, retire the old Fiber and mount a new one.

## 16. Inertia examples

### Target disappears and does not return

    t0 Loading(P)
    t1 P disappears
    t2 current step lands
    t3 L-Divert
    t4 cleanup
    t5 Pending

### The same Provider returns before completion

    t0 Loading(P)
    t1 P disappears
    t2 P reappears
    t3 current step lands
    t4 key still equals P
    t5 Active

### Provider changes to Q

    t0 Loading(P)
    t1 target becomes Q
    t2 current step lands
    t3 L-Divert and cleanup P episode
    t4 L-Begin(Q)

Every trace contains only one transition at a time.
