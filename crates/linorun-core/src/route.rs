use std::{collections::BTreeMap, sync::Arc};

use crate::id::RuntimeTag;
use crate::service::{ContractVTable, InterceptValue};
use crate::{RealmId, RealmMismatch, ServiceContract, ServiceId, ServiceTypeMismatch};

#[derive(Clone)]
pub(crate) struct ContextRoute {
    parent: Option<Arc<Self>>,
    isolate_delta: BTreeMap<ServiceId, RealmId>,
    intercept_delta: BTreeMap<ServiceId, InterceptValue>,
}

impl ContextRoute {
    pub(crate) fn root() -> Arc<Self> {
        Arc::new(Self {
            parent: None,
            isolate_delta: BTreeMap::new(),
            intercept_delta: BTreeMap::new(),
        })
    }

    pub(crate) fn isolate<C: ServiceContract>(
        self: &Arc<Self>,
        runtime: &Arc<RuntimeTag>,
        realm: RealmId,
    ) -> Result<Arc<Self>, RealmMismatch> {
        if !realm.belongs_to(runtime) {
            return Err(RealmMismatch::new(realm));
        }

        Ok(Arc::new(Self {
            parent: Some(Arc::clone(self)),
            isolate_delta: BTreeMap::from([(ServiceId::new(C::ID), realm)]),
            intercept_delta: BTreeMap::new(),
        }))
    }

    pub(crate) fn intercept<C: ServiceContract>(
        self: &Arc<Self>,
        value: C::Intercept,
    ) -> Arc<Self> {
        Arc::new(Self {
            parent: Some(Arc::clone(self)),
            isolate_delta: BTreeMap::new(),
            intercept_delta: BTreeMap::from([(
                ServiceId::new(C::ID),
                InterceptValue::new::<C>(value),
            )]),
        })
    }

    pub(crate) fn realm_for(&self, service: &ServiceId, default: &RealmId) -> RealmId {
        let mut route = Some(self);
        while let Some(current) = route {
            if let Some(realm) = current.isolate_delta.get(service) {
                return realm.clone();
            }
            route = current.parent.as_deref();
        }
        default.clone()
    }

    pub(crate) fn effective_intercept(
        &self,
        service: &ServiceId,
        contract: ContractVTable,
        declared: Option<&InterceptValue>,
    ) -> Result<InterceptValue, ServiceTypeMismatch> {
        let mut overlays = Vec::new();
        let mut route = Some(self);
        while let Some(current) = route {
            if let Some(value) = current.intercept_delta.get(service) {
                overlays.push(value);
            }
            route = current.parent.as_deref();
        }

        let mut effective = declared.cloned().unwrap_or_else(|| contract.identity());
        for overlay in overlays.into_iter().rev() {
            effective = contract.merge(service, &effective, overlay)?;
        }
        Ok(effective)
    }
}

#[cfg(test)]
mod tests {
    use std::{any::Any, sync::Arc};

    use crate::{ErasedEndpoint, ServiceRef};

    use super::*;

    struct Contract;
    struct Endpoint;

    impl ErasedEndpoint for Endpoint {
        fn as_any(&self) -> &(dyn Any + Send + Sync) {
            self
        }
    }

    impl ServiceContract for Contract {
        const ID: &'static str = "test/route@1";

        type Provider = ();
        type Intercept = Vec<&'static str>;
        type Client = ServiceRef<Self>;

        fn identity_intercept() -> Self::Intercept {
            Vec::new()
        }

        fn merge_intercept(base: &Self::Intercept, overlay: &Self::Intercept) -> Self::Intercept {
            base.iter().chain(overlay).copied().collect()
        }

        fn erase(_: Self::Provider) -> Arc<dyn ErasedEndpoint> {
            Arc::new(Endpoint)
        }

        fn client(reference: ServiceRef<Self>) -> Self::Client {
            reference
        }
    }

    #[test]
    fn nearest_isolate_wins_and_default_can_be_restored() {
        let runtime = RuntimeTag::allocate();
        let default = RealmId::new(Arc::clone(&runtime), 0);
        let tenant = RealmId::new(Arc::clone(&runtime), 1);
        let service = ServiceId::new(Contract::ID);
        let root = ContextRoute::root();
        let isolated = root.isolate::<Contract>(&runtime, tenant.clone()).unwrap();
        let restored = isolated
            .isolate::<Contract>(&runtime, default.clone())
            .unwrap();

        assert_eq!(isolated.realm_for(&service, &default), tenant);
        assert_eq!(restored.realm_for(&service, &default), default);
    }

    #[test]
    fn intercept_fold_is_declared_then_outer_to_inner() {
        let service = ServiceId::new(Contract::ID);
        let route = ContextRoute::root()
            .intercept::<Contract>(vec!["outer"])
            .intercept::<Contract>(vec!["inner"]);
        let declared = InterceptValue::new::<Contract>(vec!["declared"]);
        let effective = route
            .effective_intercept(&service, ContractVTable::of::<Contract>(), Some(&declared))
            .unwrap();

        assert_eq!(
            effective.downcast_ref::<Vec<&'static str>>().unwrap(),
            &["declared", "outer", "inner"]
        );
    }

    #[test]
    fn foreign_realm_is_rejected_without_deriving_a_route() {
        let runtime = RuntimeTag::allocate();
        let foreign = RealmId::new(RuntimeTag::allocate(), 7);
        let route = ContextRoute::root();

        assert!(route.isolate::<Contract>(&runtime, foreign).is_err());
    }
}
