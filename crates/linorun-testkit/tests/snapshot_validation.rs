use std::sync::Arc;

use linorun_core::{
    CallContext, CleanupOwner, InverseSnapshotState, effect, service_contract, service_impl,
};
use linorun_testkit::{Scenario, check_snapshot, controlled};

service_contract! {
    contract Probe {
        id: "snapshot-validation/probe@1",
        methods: [async fn value() -> ();],
    }
}

struct Provider;

#[service_impl]
impl ProbeProvider for Provider {
    async fn value(&self, _: CallContext<()>) {}
}

#[test]
fn duplicate_binding_id_is_rejected_without_rejecting_runtime_snapshots() {
    let scenario = Scenario::new();
    scenario
        .run(scenario.runtime().provide::<Probe>(Arc::new(Provider)))
        .unwrap()
        .unwrap();
    let snapshot = scenario.snapshot().unwrap();
    assert_eq!(snapshot.bindings.len(), 1);
    let mut duplicated = snapshot.clone();
    duplicated.bindings.push(snapshot.bindings[0].clone());
    assert_eq!(
        check_snapshot(&duplicated).unwrap_err().0,
        "snapshot repeats a binding id"
    );
    check_snapshot(&snapshot).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
    scenario.snapshot().unwrap();
}

#[test]
fn effect_projection_must_include_every_armed_and_running_inverse_exactly_once() {
    let scenario = Scenario::new();
    let (release, gate) = controlled();
    let effect = scenario
        .run(scenario.runtime().effect(
            "two inverses",
            effect::iter(move || {
                [
                    Ok(effect::inverse(|| async { Ok(()) })),
                    Ok(effect::inverse(move || async move {
                        gate.await;
                        Ok(())
                    })),
                ]
            }),
        ))
        .unwrap()
        .unwrap();
    scenario.run(effect.wait_forward()).unwrap().unwrap();
    let snapshot = scenario.snapshot().unwrap();
    assert_eq!(snapshot.effects.len(), 1);
    assert_eq!(snapshot.effects[0].inverses.len(), 2);

    for alteration in ["missing", "duplicate", "foreign owner", "reordered"] {
        let mut altered = snapshot.clone();
        let inverses = &mut altered.effects[0].inverses;
        match alteration {
            "missing" => {
                inverses.pop();
            }
            "duplicate" => inverses.push(inverses[0].clone()),
            "foreign owner" => inverses[0].owner = CleanupOwner::Activation,
            "reordered" => inverses.reverse(),
            _ => unreachable!(),
        }
        assert!(
            check_snapshot(&altered)
                .unwrap_err()
                .0
                .contains("inconsistent inverse projection"),
            "{alteration}"
        );
    }

    assert!(scenario.run(effect.dispose()).is_err());
    let running = scenario.snapshot().unwrap();
    assert!(
        running.effects[0]
            .inverses
            .iter()
            .any(|inverse| inverse.state == InverseSnapshotState::Running)
    );
    let mut missing_running = running.clone();
    missing_running.effects[0]
        .inverses
        .retain(|inverse| inverse.state != InverseSnapshotState::Running);
    assert!(check_snapshot(&missing_running).is_err());
    check_snapshot(&running).unwrap();

    release.complete(()).unwrap();
    scenario.run(effect.dispose()).unwrap();
    assert!(scenario.snapshot().unwrap().effects.is_empty());
    scenario.run(scenario.runtime().shutdown()).unwrap();
    scenario.snapshot().unwrap();
}
