[简体中文](../../zh/reference/invariants.md) | [English](invariants.md)

# Invariant quick reference

This document supports implementation review, debug assertions, and testing.

## I1: At-most-once inverse invocation

~~~text
forall inverse:
    call_count(inverse) <= 1
~~~

Implementation conditions:

- An accumulator entry directly and exclusively owns InverseAction; its User variant owns FnOnce.
- The scheduler takes an entry with Vec::remove; afterward, exactly one RuntimeTask owns a User action.
- An entry/action is not returned to the accumulator after completion.
- All inverse ownership is represented by Fiber.cleanup_running, RuntimeTask, and the Episode accumulator.
- Host O-Retire through FiberHandle may take only the exactly matching `RetireFiber(child)` entry in the same reducer; it cannot satisfy another action. If the parent scheduler already selected it, the Host only waits for FiberCompletion.

## I2: Strict reverse order and serial cleanup

~~~text
forall fiber:
    running_inverse_count <= 1

when starting inverse x:
    x.seq == max(armed_requested_seq)
~~~

Implementation conditions:

- A Fiber has only one `cleanup_running`.
- Fiber-wide cleanup waits for all effect drivers to reach their boundaries.
- No next inverse is selected before completion.
- Error completion also clears `cleanup_running` and continues.

## I3: Leave before Provider cleanup

~~~text
Fiber-wide Provider inverse starts
    =>
Provider lifecycle == Unloading
and no registered BindingRecord belongs to Provider
~~~

Implementation conditions:

- The reducer entering Unloading first writes lifecycle state, removes BindingRecords, and refreshes watchers.
- Fiber-wide inverses start only after the withdrawal guard opens.

For manual provide disposal, after selecting the inverse but before publishing InverseStarted,
the Actor removes its BindingRecord and refreshes watchers.

## I4: Consumer before Provider

~~~text
Provider Fiber-wide accumulator starts its first inverse
    =>
forall installed consumer:
    consumer.committed does not reference provider Fiber
~~~

Implementation conditions:

- L-Begin adds committed dependencies.
- L-Unload removes committed dependencies.
- is_relied_on is a cleanup precondition.
- While the Fiber is Unloading, Disposing effects that have not started are subject to the same precondition. A manual inverse already running completes first.

## I5: Fixed CommittedView

~~~text
during one Episode:
    committed.key is constant
    committed.entries are constant
~~~

Implementation conditions:

- CommittedView uses Arc.
- The Episode's committed fields remain immutable after construction.
- Service lookup reads only Episode.committed.

## I6: Target compares only Provider identity

~~~text
Target = ordered Vec<FiberId>
~~~

Implementation conditions:

- The target builder reads only Requirements, Binding visibility, and providers.
- BindingId, endpoint, value, and metadata do not enter Target.
- FiberIds are never reused during the Runtime's lifetime.

## I7: Transitions have inertia

~~~text
forall fiber:
    running_lifecycle_step <= 1
    episode_count <= 1
~~~

Implementation conditions:

- Reloading stores one StepId; an effect stores only one `running_step`.
- The Actor stores only identity while one RuntimeTask exclusively owns the driver, returning ownership to the Actor with step completion. Only a has_next Yield permits another poll.
- StepId and RuntimeTask ownership are established in the same Actor commit, which constitutes step start. There is no additional cancellable state before the first executor poll.
- Target changes modify only the latest intent read by the next fixed-point pass.
- The next state is decided after in-flight step completion.
- No new Episode is created until Unloading completes.

`Activation::run` retains single-step inertia for its whole body; `on_cleanup` creates no
activation boundary. As an existing step finishes after shutdown/retirement, `RegisterCleanup`
may still register cleanup in the same Episode before unload cleanup starts while a step remains
in flight. This does not permit new external work or revalidate expired Scopes.

## Structural invariants

### ComponentDescriptor

- ServiceIds in requirements are unique and have stable order.
- Provides is a ServiceId set.
- ComponentId remains fixed for the descriptor's lifetime.

### Fiber tree

~~~text
fiber.parent == P
    => P.children contains fiber

P.children contains C
    => C.parent == P
~~~

Every parent is in the registry. Children must be empty before O-Remove.
The synthetic Root and retired Fibers have no mount_effect. For every ordinary non-retired Fiber,
mount_effect points to its unique mount EffectRecord in the parent's current Episode. That record
is Armed or Disposing, and its InverseEntry is Armed or Running.

### Binding

~~~text
one BindingSlot -> at most one registered BindingRecord
binding.provider exists
binding.owner_effect exists
binding.contract_type == contracts[binding.slot.service].contract_type
~~~

WithdrawBinding removes a record only if the Slot's current BindingId matches the action.
An old action cannot remove a replacement Binding registered later in that Slot.

visibility:

~~~text
visible(binding)
    iff binding is registered
        and provider.state == Active
        and !provider.retired
~~~

### Watcher

The Slot resolved for every Fiber Requirement contains that Fiber. A removed Fiber no longer
appears in watchers.

### Episode

- Inactive has no Episode.
- Reloading, Active, and Unloading each have one Episode.
- Episode.id matches EffectRecord.episode.
- Episode implementation/config snapshots remain fixed within the episode.
- `cleanup_running` and cleanup report collectors reside permanently on FiberRecord.

### Effect

- Inverse seq values are unique within a Fiber.
- Episode.accumulator exclusively owns InverseAction; a User action exclusively owns InverseFn.
- A Running inverse has been taken from the accumulator; its action exists only on the task/cleanup_running path.
- Removing an Effect record means all its inverses have completed.
- A requested Effect at its forward boundary with no inverse in accumulator/cleanup_running must complete directly. Zero-inverse effects do not create placeholder inverses.
- A registered Binding.owner_effect points to an EffectRecord in the same Fiber/Episode.

### Committed dependency

- CommittedEntry stores full BindingId, provider, slot, and endpoint snapshots.
- CommittedEntry.contract_type agrees with Requirement.contract and the Runtime contract map.
- If the current store still contains the same BindingId, its provider and slot agree with the entry.
- Requirement indices are unique and have stable order.

## Transition preconditions and postconditions

### L-Begin

Before:

    Inactive { failure: None }
    cleanup_running == None
    latest key exists

After:

    Reloading
    committed.key == captured latest key
    accumulator empty

### L-Iter

Before:

    Reloading
    completion matches EpisodeId
    latest key == episode.key

After:

    new inverse Armed
    seq increases
    returned driver moved into exactly one next RuntimeTask

### L-Finish

Before:

    Reloading
    activation done
    latest key == episode.key

After:

    Active
    registered Provider Binding becomes visible

### L-Divert

Before:

    Reloading at boundary
    latest key != episode.key

After:

    Unloading
    owned BindingRecord removed
    never transiently Active

### L-Raise

Before:

    Reloading step error

After:

    Unloading with ActivationFailed outcome carrying the failure revision
    owned BindingRecord removed
    accumulated inverse retained

### L-Leave

Before:

    Active
    latest key != episode.key

After:

    Unloading
    owned BindingRecord removed
    watcher observes the next fixed-point pass

### L-Unload

Before starting:

    Unloading
    relied == false
    all forward drivers at boundary

After starting:

    lifecycle remains Unloading
    scheduler takes accumulator entries by descending seq

Before completing:

    accumulator empty
    cleanup_running == None
    no live EffectRecord for Episode

After completing:

    Inactive
    no registered BindingRecord for Episode
    cleanup_running == None
    Episode dropped
    committed dependency removed

## Derived graph constraints

Service visibility, Episode ownership, and withdrawal ordering imply two observable results:

- Mutually requiring Components remain Pending and create no committed edge when there is no visible Provider outside the cycle.
- Committed dependency edges do not form cycles, so a dependency cycle cannot structurally deadlock the withdrawal guard. A user Future that never completes can still keep it waiting.

These are consequences of I1–I7, not additional Core invariants. They do not require runtime
cycle detection or a cycle-breaking mechanism.

The implementation's temporal argument is that L-Begin captures only already-Active Providers,
so every committed edge points to a Provider Episode that began earlier. While the Consumer
remains installed, the withdrawal guard prevents that Provider Episode from finishing and being
replaced by a new Episode. Following committed edges therefore strictly decreases Episode start
order and cannot return to the starting point. This argument does not cover arbitrary watcher
waits, parent removal, or waits introduced by user Futures. It cannot establish freedom from
deadlocks involving arbitrary user awaits.

Accordingly, manual provide withdrawal notifies every relevant watcher but waits only for
Consumer Episodes committed to the exact BindingId, capturing `(FiberId, EpisodeId)`. Watcher
membership alone is not a dependency and must not add a wait edge. The wait ends when the captured
Episode ends or that same Episode is Active with a current key. Later Episodes do not extend it.
Manual waits therefore follow real committed edges rather than creating purely Core-owned cleanup
wait cycles outside that graph. They wait for neither public settled state nor child O-Remove,
and do not change I4's installed-Consumer guard for Fiber-wide cleanup.

## Automated checks

Core reducer unit tests directly inspect crate-private lifecycle/topology records. Deterministic
scenarios call `Scenario::snapshot()` at selected boundaries. It first obtains an Actor-consistent
RuntimeSnapshot, then `linorun_testkit::check_snapshot` validates:

- Fiber tree and mount effect ownership.
- Binding visibility, owner effect, and committed consumer index.
- Bidirectional consistency between `relied_by` and committed Provider edges.
- Episode/Effect identity and inverse owner projections.
- Unique Fiber-local seq values and at most one Running inverse.

The checker only reads DTOs; it neither changes nor repairs Runtime state.
