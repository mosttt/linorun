# Normative scenario catalog

[简体中文](../../zh/testing/scenario-catalog.md) | English

Every scenario is required behavior for Core 0.1.

## E: Effects

### E01 Idempotent manual disposal

- An effect returns an inverse.
- dispose is called twice consecutively.
- The inverse runs only once.
- Both waiters receive the same result.

### E02 Idempotent Fiber unload

- An Active Fiber owns an effect.
- The Fiber is disposed twice.
- The inverse runs only once.
- The Fiber eventually becomes Disposed.

### E03 LIFO

- Inverses receive sequences 1, 2, and 3 in order.
- Unload calls them in order 3, 2, 1.
- At most one inverse is Running at any moment.

### E04 Manual disposal followed by Fiber unload

- Sequence 2 is manually reversed.
- The Fiber unloads.
- Sequence 2 is not called again.
- Remaining inverses run in descending sequence order.

### E05 Disposal during an asynchronous single step

- A forward Future is in flight.
- Disposal is requested.
- The Future still completes.
- Its inverse is collected.
- wait_forward returns Disposed without waiting for the inverse.
- The inverse executes afterward.
- EffectDisposeReport completes and the record is removed.

### E06 Stopping an asynchronous iterator

- Iteration 1 yields inverse 1.
- Iteration 2 is in flight.
- Disposal is requested.
- Iteration 2 lands and yields inverse 2.
- Iteration 3 never starts.
- Cleanup runs in order 2, 1.

### E07 Activation iterator failure

- Step 1 of Activation::Steps yields inverse 1.
- Step 2 returns ActivationFailure.
- Inverse 1 runs.
- Activation reaches Failed.

### E08 Inverse failure

- The inverse at sequence 3 returns an error.
- Sequences 2 and 1 still run.
- The report contains the sequence 3 error.

### E09 Nested effect order

- An outer effect produces sequence 1.
- An inner effect produces sequence 2.
- The outer effect produces sequence 3.
- Unload runs them in order 3, 2, 1.

### E10 provide is an effect

- provide creates a Binding and EffectRecord.
- Its EffectHandle is disposed.
- BindingRecord is removed from the store first.
- Dependent Consumers complete L-Unload.
- EffectHandle disposal completes.

### E11 mount is an effect

- A parent mounts a child and lets it reach Active.
- FiberHandle::dispose immediately commits O-Retire and takes the equivalent unstarted mount entry.
- While child cleanup remains pending, retire the parent. Earlier parent inverses may run while the child is Unloading.
- FiberHandle continues waiting for child cleanup and removal.
- The child is removed from parent.children.
- The mount inverse was taken from the accumulator; repeated disposal creates no second retirement path.
- The host path does not invoke the RetireFiber action and cannot take any nonmatching inverse.
- Cleanup failures in the child subtree appear in that FiberHandle's DisposeReport.

### E12 Independent Effect forward failure

- Step 1 of a Scope effect yields inverse 1.
- Step 2 returns EffectError.
- Inverse 1 runs.
- wait_forward returns EffectError, and the EffectHandle disposal report retains the forward error.
- The Fiber lifecycle remains unchanged.

### E13 Inverse panic

- An inverse panics under unwind.
- Exactly one CleanupFailure is recorded for that inverse.
- Earlier inverses continue executing.
- The report includes owner, sequence, optional label, and panic cause.

### E14 Zero-inverse effect

- The effect forward returns Done without producing an inverse.
- wait_forward returns Completed.
- Manual disposal completes and removes EffectRecord directly.
- No placeholder InverseEntry or InverseStarted/InverseFinished event is created.
- Whole-Fiber cleanup also converges for such effects.

### E15 Core-owned effect trace

- provide/mount create neither an EffectDriver nor a StepId and emit no EffectYielded.
- provide emits EffectStarted, BindingInstalled, conditional BindingVisible, and EffectForwardCompleted in order.
- mount emits EffectStarted, FiberInserted, and EffectForwardCompleted in order. Child LBegin is not earlier than forward completion.
- Each still contributes a Core-owned inverse to the Episode accumulator.

### E16 Effect forward panic

- The effect driver panics while polling its current step.
- The task converts the panic to EffectError and returns the driver with its completion.
- Collected inverses clean up normally; wait_forward returns Err.
- An independent effect panic does not change the Fiber lifecycle.

## F: Fiber lifecycle

### F01 Pending waits for Requirements

- A service is missing when the Consumer is mounted.
- Its state is Pending.
- Once the Provider is Active, the Consumer becomes Loading.
- After activation finishes, it becomes Active.

### F02 Loading Providers are invisible

- A Provider has called provide during Reloading.
- The Consumer remains Pending.
- The Consumer can only L-Begin after the Provider's L-Finish.

### F03 Losing Target while Active

- The Consumer is Active(P).
- P performs L-Leave.
- The Consumer performs L-Leave.
- After cleanup, the Consumer is Pending.

### F04 Losing Target while Loading

- The Consumer is Loading(P) with a step in flight.
- P disappears and does not return.
- The step lands.
- The Consumer performs L-Divert.
- After cleanup, it is Pending.

### F05 Restoring Target while Loading

- The Consumer is Loading(P); its CommittedView captured Binding B1.
- P manually disposes its provide effect. Core removes B1, and Target temporarily becomes None.
- The same P provides Binding B2, restoring Target before the step lands.
- The keys are equal at completion.
- The Consumer continues and becomes Active.
- Its CommittedView still holds the B1 endpoint.
- B1's WithdrawBinding completion must not delete B2 from the Slot.
- The Consumer's endpoint Arc remains usable after B1 provide disposal completes.

### F06 Switching Providers while Loading

- The Consumer is Loading(P).
- Target changes to Q.
- The current step lands.
- The episode committed to P cleans up.
- A new L-Begin(Q) occurs.

### F07 Restart

- The Consumer is Active(P, revision 1).
- restart is requested.
- A complete L-Leave/L-Unload occurs.
- The new episode key is P, revision 2.

### F08 Config snapshot

- Episode E1 captures config A.
- While Loading, config is updated to B.
- E1 cleanup still retains A.
- E2 captures B.

### F09 Activation failure

- Activation fails after producing one inverse.
- The inverse runs.
- State becomes Failed.
- A sibling remains Active.

### F10 Restarting Failed

- The Fiber is Failed.
- Provider churn does not trigger activation.
- restart clears the failure.
- The Fiber becomes Loading again.

### F11 Disposing Loading

- The Fiber is Loading with a step in flight.
- O-Retire is applied.
- The step lands.
- L-Divert occurs.
- O-Remove follows cleanup.

### F12 Disposing Active

- Active proceeds through L-Leave and L-Unload.
- BindingRecord has left the store before the first inverse.
- restart/update/replace return FiberDisposed after O-Retire commits.
- The Fiber becomes Disposed after its children are gone.

### F13 Updating config while Active

- An Active Fiber receives update_config.
- The current Episode performs L-Leave/L-Unload.
- A new Episode captures the new config.
- The old Episode's CommittedView and config remain unchanged.
- An incorrect Config TypeId returns DescriptorMismatch without changing the old config/revision.

### F14 Replacing the implementation while Active

- An Active Fiber receives replace reusing the same `'static` descriptor.
- A full Leave/Unload/Begin occurs.
- A different descriptor instance or Config TypeId returns DescriptorMismatch and leaves the old Fiber unchanged.

### F15 Disposing Failed

- The Fiber is already Failed.
- dispose still performs O-Retire/O-Remove.
- Activation does not run again.
- The Fiber eventually becomes Disposed.

### F16 Disposing Pending

- A Fiber with a missing Requirement is Pending.
- dispose directly retires it.
- No Episode is created.
- It becomes Disposed after its children are gone.

### F17 Leaf with no Requirements

- The Component has no Requirements.
- O-Insert immediately yields an empty Target.
- The Fiber becomes Active after activation finishes.
- No dependency watcher is registered.

### F18 Synchronous activate panic

- L-Begin first commits StepId and RuntimeTask ownership for an Init driver.
- The first RuntimeTask encounters a synchronous panic in `Component::activate`.
- The task returns ActivationFailure and a valid Exhausted driver.
- The Fiber reaches Failed through L-Raise/cleanup. The Actor neither loses the driver nor holds duplicate ownership.

## D: Dependencies and Bindings

### D01 Target uses Provider identity

- Provider P changes its value from 1 to 2.
- Target is unchanged.
- The Consumer does not reload.

### D02 Equal values from different Providers

- P and Q have equal endpoint values.
- The Consumer switches from P to Q.
- Target changes.
- The Consumer reloads.

### D03 Fixed CommittedView

- The Consumer is Active(P).
- P's BindingRecord is removed; Q registers in the same Slot and becomes visible.
- Consumer cleanup still calls P.
- Only the new episode calls Q.

### D04 Consumers before Providers

- Consumer cleanup uses the Provider.
- The Provider performs L-Leave.
- Consumer cleanup finishes.
- The Consumer completes L-Unload.
- Only then may the Provider's first inverse start.

### D05 Dependency chain

- A provides a.
- B requires a and provides b.
- C requires b.
- A leaves.
- Cleanup order is C, B, A.

### D06 Dependency branches

- B and C both depend on A.
- B/C may clean up in either order.
- A waits for both to complete L-Unload before cleaning up.

### D07 Manual Binding withdrawal

- An Active Provider provides a and b.
- a is disposed.
- Consumers depending only on a reload.
- b remains visible.
- The Provider Fiber remains Active.

### D08 Exclusive Slot

- A Slot already contains a Binding.
- A second provide returns DuplicateBinding.
- The original Binding and Consumer remain unchanged.

### D09 Reusing a withdrawn Slot

- The old BindingRecord is removed when withdrawal starts.
- Old Consumers still hold the committed endpoint during cleanup.
- A new provide occupies the now-free Slot concurrently.
- Completion of the old WithdrawBinding matches BindingId and does not remove the new Binding.
- New Consumers resolve the new Binding.
- If the same Provider republishes the same Slot before the Loading boundary, the original Episode and CommittedView remain. Withdrawal of the original provide can still complete when that Episode returns to Active.

### D10 Watching is not dependency

- A Pending Fiber watches a Slot.
- Provider withdrawal does not wait for this Pending Fiber.
- Only installed committed Consumers block the guard.
- Manual withdrawal captures only Episodes whose CommittedView holds that BindingId. Loading/Unloading Fibers merely watching the same Slot do not depend on that Binding either.

### D11 Joining dependencies

- Consumer C requires both A and B.
- Withdrawal of either A or B makes C leave.
- The corresponding Provider may only clean up after C finishes cleanup.
- No additional total order is imposed between the two Providers.

### D12 Mutually required dependencies

- A requires b and provides a; B requires a and provides b.
- Both A and B remain Pending.
- No committed dependency edge is created.
- Runtime does not try to break or rewrite the cycle automatically.

### D13 Restarting a middle Provider

- A provides a; B requires a and provides b; C requires b.
- B restarts.
- C leaves first and releases its old CommittedView.
- B becomes Active after cleanup; C then captures the new B episode's binding.
- Provider cleanup never precedes Consumer cleanup.

## C: Context

### C01 Different Realms

- Two derived Contexts use different Realms for the same ServiceId.
- Each provides the service.
- Their children resolve the corresponding Providers.

### C02 Shared Realm

- Two Contexts use the same RealmId.
- They resolve the same Binding.

### C03 Intercept does not reload

- A Context derived from an Active Fiber adds metadata.
- Target and Episode do not change.
- Calls through newly constructed typed Clients use the overlay.

### C04 Inner intercept precedence

- Requirement metadata, an outer overlay, and an inner overlay are present.
- Contract merge order is fixed.
- The endpoint receives the expected effective metadata.

### C05 Declared lookup

- The Binding store subsequently changes.
- Clients returned by the current Context::service still use its CommittedEntry.

### C06 Undeclared services are rejected

- A child has not declared a service.
- Even if its mount Context came from the parent, lookup returns UndeclaredDependency.
- The child creates no hidden dependency edge.

### C07 The parent tree grants no service capability

- The Fiber parent relationship affects removal only.
- Services must be obtained through Requirements and CommittedView.
- Provider withdrawal waits only for declared Consumers.

### C08 Declared dependency ordering

- The child declares the service as a Requirement.
- Provider withdrawal waits for child cleanup.
- Context::service constructs a typed Client from the child's own CommittedView.

### C09 Realm Runtime ownership

- Runtime A creates a RealmId.
- A Context in Runtime B attempts to isolate into that Realm.
- RealmMismatch is returned.
- error.realm equals the supplied RealmId.
- The Context route remains unchanged.

### C10 Contract type conflict

- Runtime has registered ServiceId S as contract type A.
- mount/provide attempts to use the same S for contract type B.
- The command returns ServiceTypeMismatch before changing state.
- The payload contains service S, the Contract channel, and expected/actual TypeIds A/B.
- The Fiber registry, Binding store, and route remain unchanged.

### C11 Intercept contract laws

- Generate a, b, and c from representative metadata.
- Verify that identity is a left and right identity.
- Verify merge associativity.
- Verify a round trip through Core's typed metadata erasure and decoding.
- Folding declared, outer, and inner values follows the contract's overlay rules.

### C12 Contract-local typed Client

- A downstream fixture crate defines a Logger ServiceContract.
- The macro generates a LoggerClient newtype and typed methods in that crate.
- `Context::service::<Logger>()` returns LoggerClient and calls the committed endpoint.
- The implementation neither relies on nor generates an invalid downstream inherent `impl ServiceRef<Logger>` block.

### C13 Isolate does not reselect the current CommittedView

- The Consumer is committed to Provider P in Realm A.
- Deriving an isolate for Realm B and calling `service::<C>()` again still returns P's client.
- A child mounted through the derived Context resolves Provider Q in Realm B through its Requirement.
- The current Consumer does not reload or create a hidden edge to Q.

### C14 Explicit reset to the default Realm

- An outer Context isolates service C into Realm B.
- `default_realm()` returns the same RuntimeTag with local 0.
- An inner `isolate::<C>(default_realm)` is used to mount a child.
- The child resolves its Requirement in the default Realm rather than outer Realm B.
- default_realm/create_realm remain pure identity operations during ShuttingDown/Stopped.

## P: Parent/child

### P01 Parent mounts a child

- A parent effect creates a child.
- The parent is disposed.
- The child is retired.
- The parent is removed only after the child is removed.

### P02 Crossing parent and dependency relationships

- The child depends on a Provider that is not its parent.
- Committed dependencies determine cleanup order.
- The parent tree only controls removal.

### P03 Nested children

- The tree is root -> A -> B -> C.
- A is retired.
- C, B, and A are eventually removed.
- parent.children remains bidirectionally consistent.

## R: Concurrency and inertia

### R01 Effect disposal and step completion

- Exercise both Actor orderings.
- The inverse is collected and called once.
- The next iteration does not start after disposal wins the boundary.

### R02 Frequent Target churn

- A Loading step is in flight.
- Target changes repeatedly through P/None/P/Q.
- There is always only one Episode.
- Completion decisions use the latest key at landing time.

### R03 Restart and Provider change

- restart and a Provider change occur during the same Loading phase.
- Both revision and Target enter the latest key.
- Only one new episode starts after the old episode cleans up.

### R04 Withdrawal with multiple Consumers

- Multiple Consumers depend on the same Provider.
- The Provider waits for all committed Consumers to finish L-Unload.
- Its first inverse starts only once.

### R05 Serial cleanup

- Sequences 2 and 1 are ready while inverse 3 is in flight.
- Sequence 2 does not start.
- Sequence 2 starts only after inverse 3 completes.

### R06 Consistent snapshots

- A snapshot races with L-Leave or step completion.
- It sees a complete state from before or after the turn.
- Binding visibility always agrees with Provider lifecycle.
- An in-flight effect's EffectSnapshot.running_step is its StepId and becomes None when waiting for its inverse.
- A running WithdrawBinding's InverseSnapshot.waiting_on lists exactly the captured Consumers whose Episodes remain installed without reaching Active with a key equal to the latest ActivationKey. Later Episodes of the same Fiber are excluded.

### R07 Active effect step and L-Leave

- An effect iteration in an Active Fiber is in flight.
- Target changes and L-Leave occurs.
- Scope rejects new effects.
- The current iteration lands and produces an inverse.
- Whole-Fiber cleanup waits for the other effect drivers to reach their boundaries as well.
- Cleanup still uses the final descending sequence order.

### R08 Merging manual and whole-Fiber cleanup

- Manual disposal has started an older inverse when the Fiber performs L-Leave.
- Inertia lets the running inverse land first.
- Unloading's Episode-wide request covers the remaining entries.
- The next selected entry is the maximum remaining accumulator sequence.
- If L-Leave happens first and the manual inverse has not started, the request must wait for the withdrawal guard.
- Every inverse is still called at most once.

### R09 Updating during failure-unload

- An activation failure has entered Unloading.
- update/restart is submitted before L-Unload completes.
- The new revision invalidates the old failure outcome.
- The new Episode starts with the latest config/revision.

### R10 wait_settled

- Call wait_settled for Pending, Active, Failed, and Disposed Fibers.
- Pending/Failed/Disposed complete immediately with their corresponding FiberState.
- Loading/Unloading wait for a settled boundary.
- An Active Fiber with an independent Scope effect step in flight still returns Active, although Runtime is not quiescent.
- Failed is not returned as a Rust Err.

### R11 Driver continuation ownership

- A Steps driver's current task yields an inverse and retains a continuation.
- Completion transfers driver ownership to the Actor together with the result.
- ActivationYielded/EffectYielded.has_next indicates whether the returned driver can continue.
- The Actor moves it to the next RuntimeTask only after committing the has_next Yield.
- A driver returned with final Yield, Done, Err, or a boundary won by Target change/dispose is dropped after commit and is never polled again.
- If disposal is submitted after StepId/RuntimeTask ownership commits but before the executor's first poll, that step must still be polled through completion.
- The Actor and task never hold duplicate driver ownership, and the next iteration never starts ahead of its boundary.

### R12 Manual withdrawal without committed dependencies

- A requires S/provides T and B requires T/provides S. Each commits dependencies from independent Providers and pauses in Loading.
- After those Providers withdraw, A/B reuse the freed Slots to provide T/S. The new Bindings have no actual committed Consumers.
- Manually disposing the new Bindings does not wait for A/B merely watching each other's Slots.
- Once all started user steps return, Core inverses and shutdown can complete.

### R13 Withdrawal does not follow new Episodes

- C and D commit the same Binding in their old Episodes. Withdrawal is requested while both are Loading.
- A new Provider occupies the freed Slot. C leaves its old Episode first and enters a long Loading Episode on the new Provider.
- D remains in its old Episode, and the old WithdrawBinding.waiting_on lists only D.
- Once D leaves its old Episode, withdrawal completes without waiting for C's new activation.
- Captured EpisodeId does not participate in Target equality; same-provider republication still follows D09's inertia rule.

## S: Shutdown

### S01 Empty Runtime

- The first build event is Root FiberInserted(parent None), with no Root LBegin/LFinish.
- shutdown is requested.
- The synthetic Root finishes its empty accumulator.
- Root's latest key becomes None. After cleanup it remains Inactive without another L-Begin.
- Runtime goes Running -> ShuttingDown -> Stopped.
- RuntimeShuttingDown is the first event of the shutdown commit; RuntimeStopped is the last. Root has no FiberRemoved event.
- The report is empty.

### S02 Shutdown of a dependency chain

- A, B, and C are Active and form a dependency chain.
- shutdown makes Root Unloading. Root mount inverses retire top-level Fibers without waiting for child lifecycles.
- Cleanup preserves C, B, A order.
- Runtime becomes Stopped.

### S03 Shutdown cleanup errors

- A previous normal reload has already published a cleanup error before shutdown.
- One of multiple shutdown inverses fails.
- The others continue.
- Runtime becomes Stopped.
- The report contains only failures from ShuttingDown and does not duplicate the old episode error.

### S04 Repeated shutdown

- shutdown is called concurrently.
- Cleanup runs once.
- All waiters receive the same report.

### S05 Root provide shutdown

- Root provide creates a host-owned Binding.
- A top-level Fiber uses it through a declared Requirement.
- shutdown withdraws the Consumer before executing the Root provide inverse.
- Runtime becomes Stopped after the Root Episode finishes.

### S06 DisposeReport collection window

- A child reaches O-Retire first and lets its newer inverse fail while the parent is still Active.
- Keep the child's earlier inverse pending, then retire the parent.
- Release that earlier inverse so it fails after parent retirement.
- The child's DisposeReport contains both errors.
- The parent's DisposeReport contains only the error occurring after its own O-Retire.

### S07 Subtree error ordering

- A parent has two children being cleaned up.
- Controlled completion order is child A's newer inverse, child B's inverse, then child A's older inverse.
- The parent's DisposeReport preserves those three errors in the same InverseFinished commit order.
- Child removal order must not reorder the errors.

## A: Convenient component entry points

### A01 Cleanup registration and early disposal

- Consecutively await several on_cleanup calls on the same ComponentContext.
- Dispose one handle early; repeated disposal executes its callback only once.
- Dispose the Fiber. Remaining callbacks execute in reverse registration order.
- Older callbacks do not start while a newer asynchronous callback is incomplete.
- Disposing every handle again after Fiber cleanup does not repeat callbacks.
- A Context from the stopped component rejects new registrations.

### A02 Derived Contexts and resource ownership

- A Parent is committed to a host service in the default Realm.
- The Parent derives isolate/intercept. Its current service still uses the old CommittedView, while new Clients combine the metadata.
- The derived Context provides into a tenant Realm and mounts children in tenant/default Realms.
- Children resolve the corresponding endpoints with metadata in declared, outer, inner order.
- Binding owners and child parents remain the original Fiber.
- Parent disposal removes both children. Tenant Consumer cleanup precedes Provider cleanup.
- Child cleanup can still use its committed endpoint. The Host Binding is unaffected.

### A03 Registration racing with shutdown

- Runtime::new uses a task submission closure backed by a deterministic executor.
- A Host effect has finished forward execution.
- Submit shutdown after the on_cleanup command is submitted but before its registration driver is first polled.
- The current registration step lands and its callback is cleaned up before the earlier Host effect.
- The registration call returns a handle already carrying a Disposed outcome. Repeated disposal does not repeat the callback.
- New registrations after shutdown return RuntimeShuttingDown. Runtime eventually becomes Stopped.
