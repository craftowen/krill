//! 모바일 확인 요청. 내부 질문은 지휘자에게 남기고, 끝난 작업의 요청은 표시·응답하지 않는다.
use krill_proto::orchestration::{
    DecisionGate, DispatchStatus, GateStatus, MessageType, OrchMessage, OrchestrationStore,
    RunStatus, TaskStatus,
};
use serde_json::{Value, json};

fn run_active(store: &OrchestrationStore, id: &str) -> bool {
    store
        .runs
        .iter()
        .any(|r| r.id == id && r.status == RunStatus::Active)
}

fn context_active(store: &OrchestrationStore, run: Option<&str>, task: Option<&str>) -> bool {
    run.is_none_or(|id| run_active(store, id))
        && task.is_none_or(|id| {
            store.tasks.iter().any(|t| {
                t.id == id
                    && t.status != TaskStatus::Completed
                    && run.is_none_or(|id| t.run_id == id)
                    && run_active(store, &t.run_id)
            })
        })
}

pub(super) fn question_pending(store: &OrchestrationStore, question: &OrchMessage) -> bool {
    // `ask` defaults to coordinator. Only explicitly user-addressed questions belong on the phone.
    question.msg_type == MessageType::Question
        && matches!(question.to.as_str(), "user" | "mobile")
        && !question.acked
        && context_active(
            store,
            question.run_id.as_deref(),
            question.task_id.as_deref(),
        )
        && question.dispatch_id.as_deref().is_none_or(|id| {
            store.dispatches.iter().any(|d| {
                d.id == id
                    && d.status == DispatchStatus::Active
                    && context_active(store, Some(&d.run_id), d.task_id.as_deref())
            })
        })
}

pub(super) fn gate_pending(store: &OrchestrationStore, gate: &DecisionGate) -> bool {
    gate.status == GateStatus::Pending
        && context_active(store, gate.run_id.as_deref(), gate.task_id.as_deref())
}

pub(super) fn snapshot(store: &OrchestrationStore) -> Value {
    let questions: Vec<_> = store
        .messages
        .iter()
        .filter(|m| question_pending(store, m))
        .collect();
    let gates: Vec<_> = store
        .gates
        .iter()
        .filter(|g| gate_pending(store, g))
        .collect();
    let runs: Vec<_> = store
        .runs
        .iter()
        .filter(|r| r.status == RunStatus::Active)
        .collect();
    let tasks: Vec<_> = store
        .tasks
        .iter()
        .filter(|t| run_active(store, &t.run_id))
        .collect();
    json!({"runs":runs,"tasks":tasks,"gates":gates,"questions":questions})
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::orchestration::SendParams;

    #[test]
    fn mobile_requests_exclude_internal_resolved_and_finished_work() {
        let mut store = OrchestrationStore::default();
        let run = store.create_run("Prepare release", None).unwrap();
        let task = store.create_task(&run.id, "Review", "", vec![]).unwrap();
        for to in ["coordinator", "other-worker", "user", "mobile"] {
            store
                .send_message(&SendParams {
                    from: "reviewer".into(),
                    to: to.into(),
                    msg_type: MessageType::Question,
                    summary: "Proceed?".into(),
                    body: String::new(),
                    run_id: Some(run.id.clone()),
                    task_id: Some(task.id.clone()),
                    dispatch_id: None,
                    outcome: None,
                    reply_to: None,
                })
                .unwrap();
        }
        let gate = store
            .create_gate(
                "coordinator",
                "Publish?",
                vec![],
                Some(run.id.clone()),
                Some(task.id.clone()),
            )
            .unwrap();
        let data = snapshot(&store);
        assert_eq!(data["questions"].as_array().unwrap().len(), 2);
        assert_eq!(data["gates"].as_array().unwrap().len(), 1);
        assert!(
            !store
                .messages
                .iter()
                .take(2)
                .any(|m| question_pending(&store, m))
        );
        let question = store
            .messages
            .iter()
            .find(|m| m.to == "user")
            .unwrap()
            .clone();
        store
            .reply_message(&question.id, "mobile", "Proceed")
            .unwrap();
        assert_eq!(snapshot(&store)["questions"].as_array().unwrap().len(), 1);
        assert!(!question_pending(
            &store,
            store.messages.iter().find(|m| m.id == question.id).unwrap()
        ));
        store.tasks.first_mut().unwrap().status = TaskStatus::Completed;
        assert!(snapshot(&store)["questions"].as_array().unwrap().is_empty());
        assert!(!gate_pending(&store, &gate));
        store.tasks.first_mut().unwrap().status = TaskStatus::Blocked;
        assert!(
            gate_pending(&store, &gate),
            "blocked work can still need a decision"
        );
        for status in [RunStatus::Completed, RunStatus::Archived] {
            store.runs.first_mut().unwrap().status = status;
            let data = snapshot(&store);
            for key in ["runs", "tasks", "questions", "gates"] {
                assert!(data[key].as_array().unwrap().is_empty(), "{key}");
            }
        }
        // A missing owner is stale too; do not acknowledge or delete stored history.
        store.runs.clear();
        assert!(!gate_pending(&store, &gate));
        assert!(
            store
                .messages
                .iter()
                .any(|m| m.to == "coordinator" && !m.acked)
        );
    }
}
