use std::collections::{BTreeMap, BTreeSet};

use linorun_core::{
    BindingId, CleanupOwner, EffectId, EffectSnapshotState, FiberId, FiberState,
    InverseSnapshotState, RuntimeSnapshot,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedSnapshot {
    pub fibers: Vec<NormalizedFiber>,
    pub bindings: Vec<NormalizedBinding>,
    pub effects: Vec<NormalizedEffect>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedFiber {
    pub id: FiberId,
    pub parent: Option<FiberId>,
    pub state: FiberState,
    pub retired: bool,
    pub providers: Vec<FiberId>,
    pub children: Vec<FiberId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedBinding {
    pub id: BindingId,
    pub provider: FiberId,
    pub visible: bool,
    pub consumers: Vec<FiberId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedEffect {
    pub id: EffectId,
    pub fiber: FiberId,
    pub inverse_sequences: Vec<u64>,
}

#[must_use]
pub fn normalize_snapshot(snapshot: &RuntimeSnapshot) -> NormalizedSnapshot {
    NormalizedSnapshot {
        fibers: snapshot
            .fibers
            .iter()
            .map(|fiber| NormalizedFiber {
                id: fiber.id,
                parent: fiber.parent,
                state: fiber.state,
                retired: fiber.retired,
                providers: fiber
                    .episode
                    .as_ref()
                    .map_or_else(Vec::new, |episode| episode.committed_key.providers.clone()),
                children: fiber.children.clone(),
            })
            .collect(),
        bindings: snapshot
            .bindings
            .iter()
            .map(|binding| NormalizedBinding {
                id: binding.id,
                provider: binding.provider,
                visible: binding.visible,
                consumers: binding.committed_consumers.clone(),
            })
            .collect(),
        effects: snapshot
            .effects
            .iter()
            .map(|effect| NormalizedEffect {
                id: effect.id,
                fiber: effect.fiber,
                inverse_sequences: effect.inverses.iter().map(|inverse| inverse.seq).collect(),
            })
            .collect(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotViolation(pub String);

impl std::fmt::Display for SnapshotViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SnapshotViolation {}

pub fn check_snapshot(snapshot: &RuntimeSnapshot) -> Result<(), SnapshotViolation> {
    let fibers: BTreeMap<_, _> = snapshot
        .fibers
        .iter()
        .map(|fiber| (fiber.id, fiber))
        .collect();
    if fibers.len() != snapshot.fibers.len() {
        return Err(SnapshotViolation("snapshot repeats a fiber id".to_owned()));
    }
    let roots: Vec<_> = snapshot
        .fibers
        .iter()
        .filter(|fiber| fiber.parent.is_none())
        .collect();
    if roots.len() != 1 || roots[0].mount_effect.is_some() {
        return Err(SnapshotViolation(
            "snapshot must contain exactly one root without a mount effect".to_owned(),
        ));
    }
    let effects: BTreeMap<_, _> = snapshot
        .effects
        .iter()
        .map(|effect| (effect.id, effect))
        .collect();
    if effects.len() != snapshot.effects.len() {
        return Err(SnapshotViolation(
            "snapshot repeats an effect id".to_owned(),
        ));
    }

    for fiber in &snapshot.fibers {
        if let Some(parent_id) = fiber.parent {
            let parent = fibers.get(&parent_id).ok_or_else(|| {
                SnapshotViolation(format!(
                    "fiber {:?} has missing parent {parent_id:?}",
                    fiber.id
                ))
            })?;
            if !parent.children.contains(&fiber.id) {
                return Err(SnapshotViolation(format!(
                    "fiber {:?} is absent from parent {parent_id:?} children",
                    fiber.id
                )));
            }
            if !fiber.retired {
                let mount_effect = fiber.mount_effect.ok_or_else(|| {
                    SnapshotViolation(format!("live child {:?} has no mount effect", fiber.id))
                })?;
                let effect = effects.get(&mount_effect).ok_or_else(|| {
                    SnapshotViolation(format!(
                        "fiber {:?} has missing mount effect {mount_effect:?}",
                        fiber.id
                    ))
                })?;
                if effect.fiber != parent_id {
                    return Err(SnapshotViolation(format!(
                        "fiber {:?} mount effect belongs to {:?}, not parent {parent_id:?}",
                        fiber.id, effect.fiber,
                    )));
                }
            }
        }
        for child in &fiber.children {
            let child_record = fibers.get(child).ok_or_else(|| {
                SnapshotViolation(format!("fiber {:?} has missing child {child:?}", fiber.id))
            })?;
            if child_record.parent != Some(fiber.id) {
                return Err(SnapshotViolation(format!(
                    "child {child:?} does not point back to {:?}",
                    fiber.id
                )));
            }
        }
        let mut inverse_sequences = BTreeSet::new();
        let mut running = 0;
        if let Some(episode) = &fiber.episode {
            for provider in &episode.committed_key.providers {
                if !fibers.contains_key(provider) {
                    return Err(SnapshotViolation(format!(
                        "fiber {:?} references missing provider {provider:?}",
                        fiber.id
                    )));
                }
                if *provider != fiber.id
                    && !fibers
                        .get(provider)
                        .expect("provider existence checked above")
                        .relied_by
                        .contains(&fiber.id)
                {
                    return Err(SnapshotViolation(format!(
                        "provider {provider:?} omits committed consumer {:?}",
                        fiber.id
                    )));
                }
            }
            for inverse in &episode.accumulator {
                if !inverse_sequences.insert(inverse.seq) {
                    return Err(SnapshotViolation(format!(
                        "fiber {:?} repeats inverse sequence {}",
                        fiber.id, inverse.seq
                    )));
                }
                running += usize::from(inverse.state == InverseSnapshotState::Running);
                if let CleanupOwner::Effect(effect) = inverse.owner {
                    let effect = effects.get(&effect).ok_or_else(|| {
                        SnapshotViolation(format!(
                            "fiber {:?} inverse {} references a missing effect",
                            fiber.id, inverse.seq
                        ))
                    })?;
                    if effect.fiber != fiber.id || effect.episode != episode.id {
                        return Err(SnapshotViolation(format!(
                            "fiber {:?} inverse {} has a foreign effect owner",
                            fiber.id, inverse.seq
                        )));
                    }
                }
            }
        }
        if !fiber.relied_by.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(SnapshotViolation(format!(
                "fiber {:?} relied_by is not unique and sorted",
                fiber.id
            )));
        }
        for consumer in &fiber.relied_by {
            let consumer = fibers.get(consumer).ok_or_else(|| {
                SnapshotViolation(format!(
                    "fiber {:?} has missing committed consumer {consumer:?}",
                    fiber.id
                ))
            })?;
            if !consumer
                .episode
                .as_ref()
                .is_some_and(|episode| episode.committed_key.providers.contains(&fiber.id))
            {
                return Err(SnapshotViolation(format!(
                    "fiber {:?} lists a consumer without a committed edge",
                    fiber.id
                )));
            }
        }
        if running > 1 {
            return Err(SnapshotViolation(format!(
                "fiber {:?} has {running} running inverses",
                fiber.id
            )));
        }
    }

    let mut binding_ids = BTreeSet::new();
    for binding in &snapshot.bindings {
        if !binding_ids.insert(binding.id) {
            return Err(SnapshotViolation(
                "snapshot repeats a binding id".to_owned(),
            ));
        }
        let provider = fibers.get(&binding.provider).ok_or_else(|| {
            SnapshotViolation(format!("binding {:?} has missing provider", binding.id))
        })?;
        let expected_visible = provider.state == FiberState::Active && !provider.retired;
        if binding.visible != expected_visible {
            return Err(SnapshotViolation(format!(
                "binding {:?} visibility is {}, expected {expected_visible}",
                binding.id, binding.visible
            )));
        }
        let observed_consumers: BTreeSet<_> = binding.committed_consumers.iter().copied().collect();
        if observed_consumers.len() != binding.committed_consumers.len() {
            return Err(SnapshotViolation(format!(
                "binding {:?} repeats a committed consumer",
                binding.id
            )));
        }
        let expected_consumers: BTreeSet<_> = snapshot
            .fibers
            .iter()
            .filter(|fiber| {
                fiber
                    .episode
                    .as_ref()
                    .is_some_and(|episode| episode.committed_bindings.contains(&binding.id))
            })
            .map(|fiber| fiber.id)
            .collect();
        if observed_consumers != expected_consumers {
            return Err(SnapshotViolation(format!(
                "binding {:?} committed consumer index is inconsistent",
                binding.id
            )));
        }
        let owner = effects.get(&binding.owner_effect).ok_or_else(|| {
            SnapshotViolation(format!(
                "binding {:?} has missing owner effect {:?}",
                binding.id, binding.owner_effect
            ))
        })?;
        if owner.fiber != binding.provider {
            return Err(SnapshotViolation(format!(
                "binding {:?} owner effect belongs to another fiber",
                binding.id
            )));
        }
    }

    for effect in &snapshot.effects {
        let fiber = fibers.get(&effect.fiber).ok_or_else(|| {
            SnapshotViolation(format!("effect {:?} has missing fiber", effect.id))
        })?;
        let episode = fiber.episode.as_ref().ok_or_else(|| {
            SnapshotViolation(format!("effect {:?} owner has no episode", effect.id))
        })?;
        if effect.episode != episode.id {
            return Err(SnapshotViolation(format!(
                "effect {:?} references a stale episode",
                effect.id
            )));
        }
        match effect.state {
            EffectSnapshotState::Applying if effect.running_step.is_none() => {
                return Err(SnapshotViolation(format!(
                    "applying effect {:?} has no running step",
                    effect.id
                )));
            }
            EffectSnapshotState::Armed if effect.running_step.is_some() => {
                return Err(SnapshotViolation(format!(
                    "armed effect {:?} still has a running step",
                    effect.id
                )));
            }
            _ => {}
        }
        let expected_inverses = episode
            .accumulator
            .iter()
            .filter(|inverse| inverse.owner == CleanupOwner::Effect(effect.id));
        if !effect.inverses.iter().eq(expected_inverses) {
            return Err(SnapshotViolation(format!(
                "effect {:?} has an inconsistent inverse projection",
                effect.id
            )));
        }
    }
    Ok(())
}
