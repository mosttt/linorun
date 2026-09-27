[简体中文](../../zh/semantics/context-isolation-interception.md) | [English](context-isolation-interception.md)

# Context, isolate, and intercept

## 1. Responsibilities of Context

Context is a component's read-only view of its current Episode:

- Access declared services through CommittedView.
- Derive isolation routes.
- Derive interception metadata.
- Carry the current Fiber/Episode identity for calls.

Mutable operations belong to Scope: effect, provide, and mount. Context and Scope together form
ComponentContext, while service access always remains constrained by Requirements.

ComponentContext's convenience methods delegate to these existing entry points. `on_cleanup`
registers an inverse for an acquired resource. It retains Episode identity checks and permits
an in-flight step of the same Episode to finish necessary registration after shutdown or
retirement begins; see [Effect API constraints](effects.md#12-effect-api-constraints) for the
conditions. Deriving or cloning Context neither revalidates an expired Scope nor changes
activation step boundaries.

## 2. Immutable derivation

~~~rust
struct ContextRoute {
    parent: Option<Arc<ContextRoute>>,
    isolate_delta: Map<ServiceId, RealmId>,
    intercept_delta: Map<ServiceId, InterceptValue>,
}
~~~

Context::isolate and Context::intercept both return a new Context:

~~~rust
let tenant = ctx.isolate::<Database>(tenant_realm)?;
let traced = tenant.intercept::<Http>(trace_metadata);
~~~

The original Context is unchanged. A route's parent is used only to resolve isolation and intercept
metadata; it is not a service capability inheritance chain.

## 3. Realm resolution

For ServiceId S:

1. Search from the current route toward its parents for the nearest `isolate_delta[S]`.
2. If no override exists, use the Runtime's default Realm.
3. Construct `BindingSlot(S, Realm)`.

Services with the same name do not conflict across different Realms. Synchronous
RootContext::create_realm allocates RealmId atomically through RuntimeHandle and associates it
with one Runtime. Context::isolate accepts only a RealmId from that Runtime.
RootContext/Context::default_realm returns that Runtime's RuntimeTag/local 0. After an outer
isolate, passing this ID to an inner isolate resets subsequent routing for a ServiceId to the
default. Allocating or reading these RealmIds does not change Fiber/Binding state.

## 4. mount

When mounting a child through a derived Context:

- The Context must derive from the current ComponentContext.
- The child stores that route.
- The child's requirements resolve through that route.
- Watchers register against the resolved Slots.
- The route remains fixed for the child's Fiber lifetime.

A Fiber's parent records only mount/removal ownership. Creating a newly derived Context does
not change the Target of an existing Fiber.

The current Fiber's `Context::service` still reads the entry committed for that Episode.
Deriving an isolate from the same Context later does not redirect this lookup to another Realm.
Access to another Realm must occur through a child Fiber mounted under that route, with the
dependency committed through that child's own Requirement.

## 5. provide

Scope::provide explicitly accepts a Context derived from the current ComponentContext and uses its route:

~~~text
root.provide(DB)                         -> default realm
scope.provide(&tenant_ctx, DB)           -> tenant realm
~~~

The descriptor's provides list declares only ServiceIds; the call's route determines the actual BindingSlot.

## 6. declared service lookup

When the current Fiber declares the ServiceId:

1. Locate its entry in CommittedView by Requirement index.
2. Construct an opaque ServiceRef for entry.endpoint and wrap it in the contract-local `C::Client`.
3. Do not query the latest Binding store.
4. Continue returning the same entry during Unloading.

If the service is absent from requirements, lookup returns
`ServiceAccessError::UndeclaredDependency`. Context retains the CommittedView captured at its
creation. Components must respect the Episode lifetime; Core does not query the Actor's current
state during synchronous lookup. Core also does not implicitly search parent Fibers or the mount
Context for undeclared services.

A component may read only declared service dependencies. A service that must participate in
activation, reload, or cleanup ordering must be a Requirement.

## 7. Interception

Interception is service-specific metadata:

~~~rust
pub trait ServiceContract: Sized + Send + Sync + 'static {
    type Intercept: Clone + Send + Sync + 'static;

    fn identity_intercept() -> Self::Intercept;
    fn merge_intercept(
        base: &Self::Intercept,
        overlay: &Self::Intercept,
    ) -> Self::Intercept;
}
~~~

When `Requirement.declared_intercept == None`, merging starts from `identity_intercept()`.
Effective metadata is ordered as follows:

1. Identity.
2. Declared metadata.
3. Route overlays from outermost to innermost.

merge_intercept must be associative, with identity_intercept as both its left and right identity.
These laws allow layered metadata to compose in a defined order.

Core's erased metadata carries both contract_type and intercept_type. Mount validates the
declared/route metadata for the child's Requirements. Provide validates only the contract
identity of its generic `C`; it does not read unrelated route intercepts. Missing
`ServiceId -> Contract TypeId/vtable` mappings and the operation result are written in the same
Actor commit; failed validation leaves RuntimeCore unchanged. `Context::service` also validates
overlays derived from that Context later and constructs Client/ServiceRef in the order above.

Example:

~~~text
HTTP identity: timeout = none
Requirement: timeout = 30s
outer route: trace_id = abc
inner route: timeout = 5s
effective: trace_id = abc, timeout = 5s
~~~

## 8. Intercept does not change topology

Intercept does not participate in BindingSlot selection, Requirement satisfaction, Target equality,
watcher registration, or reload decisions. Deriving traced_ctx from ctx therefore does not restart
an existing Fiber.

## 9. Typed Client and ServiceRef

ServiceRef stores the committed endpoint, merged interception metadata, and caller/Episode
diagnostic fields. Its fields are private. ServiceContract wraps it in a contract-local
`C::Client`, whose calls use the generated adapter.

A Client/ServiceRef belongs to the Episode that created it. Components must include asynchronous
calls, streams, and cached handles in that Episode's effects and cleanup, and stop using them
when the Episode finishes.

## 10. RootContext

RootContext is the Host interface for:

- Creating top-level effects.
- Mounting top-level Components.
- Providing Host-owned services.
- Obtaining the default Realm, creating Realms, and deriving routes.

Root effects enter the same Episode accumulator. RootContext is not an administrative bypass
around effect tracking.

## 11. Isolation boundary

A Realm defines an in-process service resolution domain, determining the BindingSlots used by
Requirements and provide. It does not change a Component's static Requirement set or introduce
another tenant authorization system. Higher-level Runtime/Realm management policies are
responsible for multi-tenant isolation.
