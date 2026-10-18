use super::render_task;
use crate::proto::tasks;

fn task(in_flight: u64) -> tasks::TaskStatus {
    tasks::TaskStatus {
        name: "example-task".into(),
        placements: vec![tasks::TaskPlacementStatus {
            lifecycle: tasks::TaskLifecycle::Inactive as i32,
            in_flight,
            last_execution: Some(tasks::TaskExecutionOutcome {
                result: tasks::TaskExecutionResult::Succeeded as i32,
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn completed_execution_remains_visible_without_running_instances() {
    let report = render_task(task(0));
    assert!(report.contains("running=0"));
    assert!(report.contains("no running instances"));
    assert!(report.contains("last=succeeded"));
    assert!(report.contains("controller=inactive"));
}

#[test]
fn current_execution_and_previous_outcome_are_distinct() {
    let report = render_task(task(1));
    assert!(report.contains("running=1"));
    assert!(report.contains("instance=1/1 | running"));
    assert!(report.contains("last=succeeded"));
    assert!(!report.contains("no running instances"));
}

#[test]
fn coordinated_idle_controllers_distinguish_slot_owners_from_waiters() {
    for dispatch in [tasks::TaskDispatch::One, tasks::TaskDispatch::Pool] {
        let mut idle = task(0);
        idle.dispatch = dispatch as i32;
        idle.placements[0].lifecycle = tasks::TaskLifecycle::Active as i32;
        assert!(
            render_task(idle.clone())
                .contains("execution=waiting | owned_slots=0")
        );
        idle.placements[0].owned_slots = 1;
        assert!(render_task(idle).contains("execution=armed | owned_slots=1"));
    }
}
