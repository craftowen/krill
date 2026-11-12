//! 모바일 확인 요청. 내부 질문은 지휘자에게 남기고, 끝난 작업의 요청은 표시·응답하지 않는다.
use krill_proto::orchestration::{
    DecisionGate, DispatchStatus, GateStatus, MessageType, OrchMessage, OrchestrationStore,
    RunStatus, TaskStatus,
};
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(serde::Serialize)]
struct MobileRunSummary<'a> {
    id: &'a str,
    title: &'a str,
    status: RunStatus,
}

#[derive(serde::Serialize)]
struct MobileTaskSummary<'a> {
    id: &'a str,
    run_id: &'a str,
    title: &'a str,
    status: TaskStatus,
}

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
    let active_runs: HashSet<&str> = store
        .runs
        .iter()
        .filter(|r| r.status == RunStatus::Active)
        .map(|r| r.id.as_str())
        .collect();
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
        .map(|r| MobileRunSummary {
            id: &r.id,
            title: &r.title,
            status: r.status,
        })
        .collect();
    let tasks: Vec<_> = store
        .tasks
        .iter()
        .filter(|t| active_runs.contains(t.run_id.as_str()))
        .map(|t| MobileTaskSummary {
            id: &t.id,
            run_id: &t.run_id,
            title: &t.title,
            status: t.status,
        })
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

    #[test]
    fn mobile_requests_snapshot_projects_run_and_task_summaries() {
        let mut store = OrchestrationStore::default();
        let active_first = store.create_run("First active", Some(7)).unwrap();
        let finished = store.create_run("Finished run", Some(8)).unwrap();
        let active_second = store.create_run("Second active", None).unwrap();

        let task_a = store
            .create_task(
                &active_first.id,
                "Ship mobile",
                "large task description omit me",
                vec![],
            )
            .unwrap();
        let task_b = store
            .create_task(
                &active_first.id,
                "Follow-up check",
                "pending task description omit me",
                vec![],
            )
            .unwrap();
        let old_task = store
            .create_task(
                &finished.id,
                "Old task",
                "finished run task omit me",
                vec![],
            )
            .unwrap();
        let task_c = store
            .create_task(
                &active_second.id,
                "Verify mobile",
                "second task description omit me",
                vec![],
            )
            .unwrap();
        store
            .update_task(
                &task_a.id,
                TaskStatus::Completed,
                Some("large result summary omit me".into()),
            )
            .unwrap();
        store
            .update_task(
                &old_task.id,
                TaskStatus::Completed,
                Some("finished summary omit me".into()),
            )
            .unwrap();

        let gate = store
            .create_gate(
                "coordinator",
                "Approve deploy?",
                vec!["Approve".into(), "Hold".into()],
                Some(active_first.id.clone()),
                Some(task_b.id.clone()),
            )
            .unwrap();
        let question = store
            .send_message(&SendParams {
                from: "worker".into(),
                to: "mobile".into(),
                msg_type: MessageType::Question,
                summary: "Use staging?".into(),
                body: "Detailed question body".into(),
                run_id: Some(active_first.id.clone()),
                task_id: Some(task_b.id.clone()),
                dispatch_id: None,
                outcome: None,
                reply_to: None,
            })
            .unwrap()
            .into_iter()
            .next()
            .unwrap();

        let data = snapshot(&store);
        assert_eq!(
            data["runs"],
            json!([
                {"id": active_first.id, "title": "First active", "status": "active"},
                {"id": active_second.id, "title": "Second active", "status": "active"},
            ])
        );
        assert_eq!(
            data["tasks"],
            json!([
                {"id": task_a.id, "run_id": active_first.id, "title": "Ship mobile", "status": "completed"},
                {"id": task_b.id, "run_id": active_first.id, "title": "Follow-up check", "status": "ready"},
                {"id": task_c.id, "run_id": active_second.id, "title": "Verify mobile", "status": "ready"},
            ])
        );
        assert_eq!(data["gates"], json!([gate]));
        assert_eq!(data["questions"], json!([question]));

        let raw = serde_json::to_string(&data).unwrap();
        assert!(!raw.contains("description"));
        assert!(!raw.contains("result_summary"));
        assert!(!raw.contains("omit me"));
    }
}
