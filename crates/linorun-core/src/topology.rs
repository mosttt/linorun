use std::{any::TypeId, collections::BTreeMap, sync::Arc};

use crate::component::{ComponentDescriptor, Requirement};
use crate::route::ContextRoute;
use crate::service::{ContractVTable, ErasedEndpoint, InterceptValue};
use crate::{
    BindingId, EffectId, FiberId, RealmId, ServiceId, ServiceTypeChannel, ServiceTypeMismatch,
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct BindingSlot {
    service: ServiceId,
    realm: RealmId,
}

impl BindingSlot {
    pub(crate) fn new(service: ServiceId, realm: RealmId) -> Self {
        Self { service, realm }
    }

    pub(crate) fn service(&self) -> &ServiceId {
        &self.service
    }

    pub(crate) fn realm(&self) -> &RealmId {
        &self.realm
    }
}

#[derive(Clone)]
pub(crate) struct BindingRecord {
    id: BindingId,
    slot: BindingSlot,
    provider: FiberId,
    contract_type: TypeId,
    endpoint: Arc<dyn ErasedEndpoint>,
    owner_effect: EffectId,
}

impl BindingRecord {
    pub(crate) fn new(
        id: BindingId,
        slot: BindingSlot,
        provider: FiberId,
        contract_type: TypeId,
        endpoint: Arc<dyn ErasedEndpoint>,
        owner_effect: EffectId,
    ) -> Self {
        Self {
            id,
            slot,
            provider,
            contract_type,
            endpoint,
            owner_effect,
        }
    }

    pub(crate) const fn id(&self) -> BindingId {
        self.id
    }

    pub(crate) fn slot(&self) -> &BindingSlot {
        &self.slot
    }

    pub(crate) const fn provider(&self) -> FiberId {
        self.provider
    }

    pub(crate) const fn owner_effect(&self) -> EffectId {
        self.owner_effect
    }
}

pub(crate) struct BindingTable {
    records: BTreeMap<BindingSlot, BindingRecord>,
}

impl BindingTable {
    pub(crate) fn new() -> Self {
        Self {
            records: BTreeMap::new(),
        }
    }

    pub(crate) fn insert(&mut self, record: BindingRecord) -> Result<(), BindingRecord> {
        if self.records.contains_key(&record.slot) {
            return Err(record);
        }
        self.records.insert(record.slot.clone(), record);
        Ok(())
    }

    pub(crate) fn remove_if_matches(
        &mut self,
        slot: &BindingSlot,
        binding: BindingId,
    ) -> Option<BindingRecord> {
        let current = self.records.get(slot)?;
        if current.id != binding {
            return None;
        }
        self.records.remove(slot)
    }

    pub(crate) fn contains(&self, slot: &BindingSlot) -> bool {
        self.records.contains_key(slot)
    }

    pub(crate) fn remove_provider(&mut self, provider: FiberId) -> Vec<BindingRecord> {
        let slots: Vec<_> = self
            .records
            .iter()
            .filter_map(|(slot, record)| (record.provider == provider).then_some(slot.clone()))
            .collect();
        slots
            .into_iter()
            .filter_map(|slot| self.records.remove(&slot))
            .collect()
    }

    pub(crate) fn provider_has_bindings(&self, provider: FiberId) -> bool {
        self.records
            .values()
            .any(|record| record.provider == provider)
    }

    pub(crate) fn records(&self) -> impl Iterator<Item = &BindingRecord> {
        self.records.values()
    }

    fn get(&self, slot: &BindingSlot) -> Option<&BindingRecord> {
        self.records.get(slot)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Target {
    providers: Vec<FiberId>,
}

impl Target {
    pub(crate) fn new(providers: Vec<FiberId>) -> Self {
        Self { providers }
    }

    pub(crate) fn providers(&self) -> &[FiberId] {
        &self.providers
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActivationKey {
    target: Target,
    revision: u64,
}

impl ActivationKey {
    pub(crate) fn new(target: Target, revision: u64) -> Self {
        Self { target, revision }
    }

    pub(crate) fn target(&self) -> &Target {
        &self.target
    }

    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }
}

#[derive(Clone)]
pub(crate) struct CommittedView {
    key: ActivationKey,
    entries: Vec<CommittedEntry>,
}

impl CommittedView {
    pub(crate) fn new(key: ActivationKey, entries: Vec<CommittedEntry>) -> Self {
        Self { key, entries }
    }

    pub(crate) fn key(&self) -> &ActivationKey {
        &self.key
    }

    pub(crate) fn entries(&self) -> &[CommittedEntry] {
        &self.entries
    }

    pub(crate) fn entry_for(&self, service: &ServiceId) -> Option<&CommittedEntry> {
        self.entries
            .iter()
            .find(|entry| entry.slot.service() == service)
    }
}

#[derive(Clone)]
pub(crate) struct CommittedEntry {
    binding: BindingId,
    slot: BindingSlot,
    provider: FiberId,
    contract_type: TypeId,
    endpoint: Arc<dyn ErasedEndpoint>,
    declared_intercept: Option<InterceptValue>,
}

impl CommittedEntry {
    pub(crate) const fn binding(&self) -> BindingId {
        self.binding
    }

    pub(crate) const fn provider(&self) -> FiberId {
        self.provider
    }

    pub(crate) const fn contract_type(&self) -> TypeId {
        self.contract_type
    }

    pub(crate) fn endpoint(&self) -> Arc<dyn ErasedEndpoint> {
        Arc::clone(&self.endpoint)
    }

    pub(crate) fn declared_intercept(&self) -> Option<&InterceptValue> {
        self.declared_intercept.as_ref()
    }
}

#[derive(Default)]
pub(crate) struct ContractRegistry {
    contracts: BTreeMap<ServiceId, ContractVTable>,
}

impl ContractRegistry {
    pub(crate) fn validate_descriptor(
        &self,
        descriptor: &ComponentDescriptor,
    ) -> Result<Vec<(ServiceId, ContractVTable)>, ServiceTypeMismatch> {
        let mut staged = Vec::new();
        for requirement in descriptor.requirements() {
            self.validate_mapping(requirement.service(), requirement.contract())?;
            if !self.contracts.contains_key(requirement.service()) {
                staged.push((requirement.service().clone(), requirement.contract()));
            }
        }
        Ok(staged)
    }

    pub(crate) fn validate_erased_contract(
        &self,
        service: ServiceId,
        contract: ContractVTable,
    ) -> Result<Option<(ServiceId, ContractVTable)>, ServiceTypeMismatch> {
        self.validate_mapping(&service, contract)?;
        Ok((!self.contracts.contains_key(&service)).then_some((service, contract)))
    }

    pub(crate) fn commit<I>(&mut self, staged: I)
    where
        I: IntoIterator<Item = (ServiceId, ContractVTable)>,
    {
        for (service, contract) in staged {
            self.contracts.entry(service).or_insert(contract);
        }
    }

    fn validate_mapping(
        &self,
        service: &ServiceId,
        contract: ContractVTable,
    ) -> Result<(), ServiceTypeMismatch> {
        let Some(current) = self.contracts.get(service).copied() else {
            return Ok(());
        };
        if current.contract_type() != contract.contract_type() {
            return Err(ServiceTypeMismatch {
                service: service.clone(),
                channel: ServiceTypeChannel::Contract,
                expected: current.contract_type(),
                actual: contract.contract_type(),
            });
        }
        if current.intercept_type() != contract.intercept_type() {
            return Err(ServiceTypeMismatch {
                service: service.clone(),
                channel: ServiceTypeChannel::Intercept,
                expected: current.intercept_type(),
                actual: contract.intercept_type(),
            });
        }
        Ok(())
    }
}

pub(crate) fn validate_descriptor_route(
    descriptor: &ComponentDescriptor,
    route: &ContextRoute,
) -> Result<(), ServiceTypeMismatch> {
    for requirement in descriptor.requirements() {
        route.effective_intercept(
            requirement.service(),
            requirement.contract(),
            requirement.declared_intercept(),
        )?;
    }
    Ok(())
}

pub(crate) fn resolve_dependencies(
    descriptor: &ComponentDescriptor,
    route: &ContextRoute,
    default_realm: &RealmId,
    bindings: &BindingTable,
    revision: u64,
    mut provider_is_active: impl FnMut(FiberId) -> bool,
) -> Option<CommittedView> {
    let mut providers = Vec::with_capacity(descriptor.requirements().len());
    let mut entries = Vec::with_capacity(descriptor.requirements().len());

    for requirement in descriptor.requirements() {
        let slot = BindingSlot::new(
            requirement.service().clone(),
            route.realm_for(requirement.service(), default_realm),
        );
        let binding = bindings.get(&slot)?;
        if binding.contract_type != requirement.contract().contract_type()
            || !provider_is_active(binding.provider)
        {
            return None;
        }

        providers.push(binding.provider);
        entries.push(committed_entry(requirement, binding));
    }

    Some(CommittedView::new(
        ActivationKey::new(Target::new(providers), revision),
        entries,
    ))
}

fn committed_entry(requirement: &Requirement, binding: &BindingRecord) -> CommittedEntry {
    CommittedEntry {
        binding: binding.id,
        slot: binding.slot.clone(),
        provider: binding.provider,
        contract_type: binding.contract_type,
        endpoint: Arc::clone(&binding.endpoint),
        declared_intercept: requirement.declared_intercept().cloned(),
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;

    use crate::id::RuntimeTag;
    use crate::{Requirement, ServiceContract, ServiceRef};

    use super::*;

    struct Contract;
    struct OtherContract;

    #[derive(Debug)]
    struct Endpoint(u64);

    impl ErasedEndpoint for Endpoint {
        fn as_any(&self) -> &(dyn Any + Send + Sync) {
            self
        }
    }

    macro_rules! contract {
        ($name:ty, $id:literal) => {
            impl ServiceContract for $name {
                const ID: &'static str = $id;

                type Provider = u64;
                type Intercept = ();
                type Client = ServiceRef<Self>;

                fn identity_intercept() -> Self::Intercept {}

                fn merge_intercept(_: &Self::Intercept, _: &Self::Intercept) -> Self::Intercept {}

                fn erase(provider: Self::Provider) -> Arc<dyn ErasedEndpoint> {
                    Arc::new(Endpoint(provider))
                }

                fn client(reference: ServiceRef<Self>) -> Self::Client {
                    reference
                }
            }
        };
    }

    contract!(Contract, "test/topology@1");
    contract!(OtherContract, "test/topology@1");

    fn binding(id: u64, slot: BindingSlot, provider: FiberId, endpoint: u64) -> BindingRecord {
        BindingRecord {
            id: BindingId::new(id),
            slot,
            provider,
            contract_type: TypeId::of::<Contract>(),
            endpoint: Arc::new(Endpoint(endpoint)),
            owner_effect: EffectId::new(id),
        }
    }

    #[test]
    fn provider_visibility_controls_target_satisfaction() {
        let runtime = RuntimeTag::allocate();
        let default_realm = RealmId::new(runtime, 0);
        let route = ContextRoute::root();
        let descriptor = ComponentDescriptor::new::<()>(
            "test/consumer@1",
            vec![Requirement::new::<Contract>(None)],
            Vec::new(),
        );
        let slot = BindingSlot::new(ServiceId::new(Contract::ID), default_realm.clone());
        let mut bindings = BindingTable::new();
        assert!(
            bindings
                .insert(binding(1, slot, FiberId::new(9), 11))
                .is_ok()
        );

        assert!(
            resolve_dependencies(&descriptor, &route, &default_realm, &bindings, 0, |_| false,)
                .is_none()
        );
        assert!(
            resolve_dependencies(&descriptor, &route, &default_realm, &bindings, 0, |_| true,)
                .is_some()
        );
    }

    #[test]
    fn same_provider_replacement_keeps_target_but_commits_old_endpoint() {
        let runtime = RuntimeTag::allocate();
        let default_realm = RealmId::new(runtime, 0);
        let route = ContextRoute::root();
        let descriptor = ComponentDescriptor::new::<()>(
            "test/consumer@1",
            vec![Requirement::new::<Contract>(None)],
            Vec::new(),
        );
        let slot = BindingSlot::new(ServiceId::new(Contract::ID), default_realm.clone());
        let provider = FiberId::new(5);
        let mut bindings = BindingTable::new();
        assert!(
            bindings
                .insert(binding(1, slot.clone(), provider, 10))
                .is_ok()
        );
        let first =
            resolve_dependencies(&descriptor, &route, &default_realm, &bindings, 0, |_| true)
                .unwrap();

        bindings.remove_if_matches(&slot, BindingId::new(1));
        assert!(bindings.insert(binding(2, slot, provider, 20)).is_ok());
        let second =
            resolve_dependencies(&descriptor, &route, &default_realm, &bindings, 0, |_| true)
                .unwrap();

        assert_eq!(first.key.target, second.key.target);
        assert_ne!(first.entries[0].binding, second.entries[0].binding);
        assert_eq!(
            first.entries[0]
                .endpoint
                .as_any()
                .downcast_ref::<Endpoint>()
                .unwrap()
                .0,
            10
        );
    }

    #[test]
    fn old_withdraw_action_does_not_remove_replacement() {
        let runtime = RuntimeTag::allocate();
        let realm = RealmId::new(runtime, 0);
        let slot = BindingSlot::new(ServiceId::new(Contract::ID), realm);
        let provider = FiberId::new(5);
        let mut bindings = BindingTable::new();
        assert!(
            bindings
                .insert(binding(2, slot.clone(), provider, 20))
                .is_ok()
        );

        assert!(
            bindings
                .remove_if_matches(&slot, BindingId::new(1))
                .is_none()
        );
        assert_eq!(bindings.get(&slot).unwrap().id, BindingId::new(2));
    }

    #[test]
    fn contract_registry_rejects_same_id_with_another_rust_contract() {
        let mut registry = ContractRegistry::default();
        registry.commit(
            registry
                .validate_erased_contract(
                    ServiceId::new(Contract::ID),
                    ContractVTable::of::<Contract>(),
                )
                .unwrap(),
        );

        let error = match registry.validate_erased_contract(
            ServiceId::new(OtherContract::ID),
            ContractVTable::of::<OtherContract>(),
        ) {
            Ok(_) => panic!("same service id with another contract must fail"),
            Err(error) => error,
        };

        assert_eq!(error.channel, ServiceTypeChannel::Contract);
    }
}
