use std::{collections::BTreeSet, sync::Arc};

use linorun_core::{
    ActivationFailure, BindingId, CallContext, ComponentContext, EffectId, EpisodeId, FiberId,
    component, service_contract, service_impl,
};
use linorun_testkit::{ModelBinding, ModelCommand, ModelLifecycle, ReferenceModel, Scenario};

service_contract! {
    contract Probe {
        id: "model-boundaries/probe@1",
        methods: [async fn value() -> ();],
    }
}

struct Provider;

#[service_impl]
impl ProbeProvider for Provider {
    async fn value(&self, _: CallContext<()>) {}
}

#[component(id = "model-boundaries/consumer@1")]
async fn consumer(_context: ComponentContext) -> Result<(), ActivationFailure> {
    Ok(())
}

struct Ids {
    provider: FiberId,
    consumer: FiberId,
    episode: EpisodeId,
    binding: BindingId,
    effect: EffectId,
}

impl Ids {
    fn new() -> Self {
        let scenario = Scenario::new();
        let provided = scenario
            .run(scenario.runtime().provide::<Probe>(Arc::new(Provider)))
            .unwrap()
            .unwrap();
        let child = scenario
            .run(scenario.runtime().mount(consumer(), ()))
            .unwrap()
            .unwrap();
        scenario.run(child.wait_settled()).unwrap();
        let snapshot = scenario.snapshot().unwrap();
        let root = snapshot
            .fibers
            .iter()
            .find(|fiber| fiber.parent.is_none())
            .unwrap();
        let ids = Self {
            provider: root.id,
            consumer: child.id(),
            episode: root.episode.as_ref().unwrap().id,
            binding: snapshot.bindings[0].id,
            effect: provided.id(),
        };
        scenario.run(scenario.runtime().shutdown()).unwrap();
        ids
    }

    fn model(&self) -> ReferenceModel {
        let mut model = ReferenceModel::new();
        model
            .apply(ModelCommand::InsertFiber {
                fiber: self.provider,
                parent: None,
            })
            .unwrap();
        model
            .apply(ModelCommand::InsertFiber {
                fiber: self.consumer,
                parent: Some(self.provider),
            })
            .unwrap();
        model
    }
}

fn reject_unchanged(model: &mut ReferenceModel, command: ModelCommand) {
    let before = model.snapshot();
    assert!(model.apply(command).is_err());
    assert_eq!(model.snapshot(), before);
}

#[test]
fn retirement_cannot_remove_an_installed_episode() {
    let ids = Ids::new();
    let fiber = ids.consumer;
    for phase in ["reloading", "active", "unloading"] {
        let mut model = ids.model();
        model
            .apply(ModelCommand::Begin {
                fiber,
                episode: ids.episode,
            })
            .unwrap();
        if phase != "reloading" {
            model.apply(ModelCommand::Finish { fiber }).unwrap();
        }
        if phase == "unloading" {
            model.apply(ModelCommand::Leave { fiber }).unwrap();
        }
        model.apply(ModelCommand::Retire { fiber }).unwrap();
        reject_unchanged(&mut model, ModelCommand::Remove { fiber });
        match phase {
            "reloading" => model.apply(ModelCommand::Divert { fiber }).unwrap(),
            "active" => model.apply(ModelCommand::Leave { fiber }).unwrap(),
            _ => {}
        }
        model.apply(ModelCommand::FinishUnload { fiber }).unwrap();
        model.apply(ModelCommand::Remove { fiber }).unwrap();
        assert!(model.snapshot().fibers[0].children.is_empty());
    }

    for failed in [false, true] {
        let mut model = ids.model();
        if failed {
            model
                .apply(ModelCommand::Begin {
                    fiber,
                    episode: ids.episode,
                })
                .unwrap();
            model.apply(ModelCommand::Raise { fiber }).unwrap();
            model.apply(ModelCommand::FinishUnload { fiber }).unwrap();
            assert_eq!(model.snapshot().fibers[1].lifecycle, ModelLifecycle::Failed);
        }
        model.apply(ModelCommand::Retire { fiber }).unwrap();
        model.apply(ModelCommand::Remove { fiber }).unwrap();
        assert_eq!(model.snapshot().fibers.len(), 1);
    }
}

#[test]
fn unload_waits_for_inverse_completion_and_effect_removal() {
    let ids = Ids::new();
    let fiber = ids.consumer;
    let effect = ids.effect;
    let mut model = ids.model();
    for command in [
        ModelCommand::Begin {
            fiber,
            episode: ids.episode,
        },
        ModelCommand::Finish { fiber },
        ModelCommand::StartEffect { effect, fiber },
        ModelCommand::YieldInverse { effect, seq: 1 },
        ModelCommand::RequestEffectDispose { effect },
        ModelCommand::Leave { fiber },
    ] {
        model.apply(command).unwrap();
    }
    reject_unchanged(&mut model, ModelCommand::FinishUnload { fiber });
    model
        .apply(ModelCommand::StartInverse { effect, seq: 1 })
        .unwrap();
    reject_unchanged(&mut model, ModelCommand::FinishUnload { fiber });
    model
        .apply(ModelCommand::FinishInverse { effect, seq: 1 })
        .unwrap();
    reject_unchanged(&mut model, ModelCommand::FinishUnload { fiber });
    model.apply(ModelCommand::FinishEffect { effect }).unwrap();
    model.apply(ModelCommand::FinishUnload { fiber }).unwrap();
    assert_eq!(
        model.snapshot().fibers[1].lifecycle,
        ModelLifecycle::Inactive
    );
}

#[test]
fn unload_requires_explicit_binding_and_consumer_edge_removal() {
    let ids = Ids::new();
    let mut model = ids.model();
    for fiber in [ids.provider, ids.consumer] {
        model
            .apply(ModelCommand::Begin {
                fiber,
                episode: ids.episode,
            })
            .unwrap();
        model.apply(ModelCommand::Finish { fiber }).unwrap();
    }
    model
        .apply(ModelCommand::InstallBinding(ModelBinding {
            id: ids.binding,
            provider: ids.provider,
            consumers: BTreeSet::from([ids.consumer]),
        }))
        .unwrap();
    for fiber in [ids.provider, ids.consumer] {
        model.apply(ModelCommand::Leave { fiber }).unwrap();
        reject_unchanged(&mut model, ModelCommand::FinishUnload { fiber });
    }
    model
        .apply(ModelCommand::RemoveConsumer {
            binding: ids.binding,
            consumer: ids.consumer,
        })
        .unwrap();
    model
        .apply(ModelCommand::FinishUnload {
            fiber: ids.consumer,
        })
        .unwrap();
    reject_unchanged(
        &mut model,
        ModelCommand::FinishUnload {
            fiber: ids.provider,
        },
    );
    model
        .apply(ModelCommand::RemoveBinding {
            binding: ids.binding,
        })
        .unwrap();
    model
        .apply(ModelCommand::FinishUnload {
            fiber: ids.provider,
        })
        .unwrap();
}
