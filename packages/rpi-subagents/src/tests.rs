use super::*;
use std::collections::VecDeque;
use std::sync::Mutex;

fn request() -> Request {
    parse_request(&json!({"task":"inspect"})).unwrap()
}
fn completed() -> Value {
    json!({"status":"done","result":{"status":"completed","text":"report","turns":1,"turnLimited":false}})
}

fn scripted(replies: Vec<Result<Value, String>>) -> (Box<Action>, Arc<Mutex<Vec<Value>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let saved = calls.clone();
    let replies = Mutex::new(VecDeque::from(replies));
    let action = Box::new(move |value: Value| {
        saved.lock().unwrap().push(value.clone());
        if value["op"] == "dispose" || value["op"] == "cancel" {
            return Ok(json!({}));
        }
        replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected action")
    });
    (action, calls)
}

#[test]
fn validates_and_clamps_limits_while_accepting_host_context() {
    let r = parse_request(
        &json!({"task":"inspect","maxTurns":999,"timeoutSeconds":1,"__rpi":{"cwd":"D:/project"}}),
    )
    .unwrap();
    assert_eq!(r.max_turns, 32);
    assert_eq!(r.timeout_seconds, 10);
    assert_eq!(request().max_turns, 8);
    assert_eq!(request().timeout_seconds, 300);
    for value in [
        json!({}),
        json!({"task":" "}),
        json!({"task":"x".repeat(20001)}),
        json!({"task":"x","role":"x".repeat(201)}),
    ] {
        assert!(parse_request(&value).is_err());
    }
}

#[test]
fn forwards_deltas_and_returns_delegation_metadata_then_disposes() {
    let (action, calls) = scripted(vec![
        Ok(json!({"childId":"child-1","sessionId":"child-1"})),
        Ok(json!({"status":"running","deltas":["hello"]})),
        Ok(completed()),
    ]);
    let (tx, rx) = mpsc::channel();
    run_request(
        request(),
        "tool-1".into(),
        &*action,
        tx,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert!(matches!(rx.recv().unwrap(),JobMessage::Partial(text) if text == "hello"));
    let JobMessage::Done(text) = rx.recv().unwrap() else {
        panic!("missing final result")
    };
    let result: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(result["kind"], "rpi-delegation");
    assert_eq!(result["report"], "report");
    assert_eq!(result["sessionId"], "child-1");
    let calls = calls.lock().unwrap();
    assert_eq!(calls[0]["toolCallId"], "tool-1");
    assert_eq!(calls.last().unwrap()["op"], "dispose");
}

#[test]
fn unsupported_host_is_explicit_and_never_spawns_a_process() {
    let (action, calls) = scripted(vec![Err("unknown runtime action id 24".into())]);
    let (tx, _) = mpsc::channel();
    let error = run_request(
        request(),
        "t".into(),
        &*action,
        tx,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap_err();
    assert!(error.contains("requires a host supporting ChildAgent"));
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[test]
fn cancellation_before_start_has_no_host_side_effects() {
    let (action, calls) = scripted(vec![]);
    let (tx, _) = mpsc::channel();
    assert!(run_request(
        request(),
        "t".into(),
        &*action,
        tx,
        Arc::new(AtomicBool::new(true))
    )
    .is_err());
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn cancellation_after_start_cancels_and_disposes() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let token = cancelled.clone();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let saved = calls.clone();
    let action = move |value: Value| {
        saved.lock().unwrap().push(value.clone());
        match value["op"].as_str().unwrap() {
            "start" => Ok(json!({"childId":"c"})),
            "poll" => {
                token.store(true, Ordering::Release);
                Ok(json!({"status":"running"}))
            }
            _ => Ok(json!({})),
        }
    };
    let (tx, _rx) = mpsc::channel();
    assert!(run_request(request(), "t".into(), &action, tx, cancelled)
        .unwrap_err()
        .contains("cancelled"));
    let ops: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|v| v["op"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ops, vec!["start", "poll", "cancel", "dispose"]);
}

#[test]
fn errors_and_malformed_replies_always_dispose() {
    for reply in [
        Err("lost bridge".into()),
        Ok(json!({"status":"failed","error":"permission denied"})),
        Ok(json!({"status":"invalid"})),
        Ok(json!({"status":"done"})),
        Ok(json!({"status":"done","result":{"status":"failed","error":"provider failed"}})),
    ] {
        let (action, calls) = scripted(vec![Ok(json!({"childId":"c"})), reply]);
        let (tx, _rx) = mpsc::channel();
        assert!(run_request(
            request(),
            "t".into(),
            &*action,
            tx,
            Arc::new(AtomicBool::new(false))
        )
        .is_err());
        assert_eq!(calls.lock().unwrap().last().unwrap()["op"], "dispose");
    }
}

#[test]
fn streamed_and_final_text_share_a_budget() {
    let (action, calls) = scripted(vec![
        Ok(json!({"childId":"c"})),
        Ok(json!({"status":"running","deltas":["a".repeat(MAX_OUTPUT_BYTES)]})),
        Ok(completed()),
    ]);
    let (tx, _rx) = mpsc::channel();
    assert!(run_request(
        request(),
        "t".into(),
        &*action,
        tx,
        Arc::new(AtomicBool::new(false))
    )
    .unwrap_err()
    .contains("output limit"));
    assert_eq!(calls.lock().unwrap().last().unwrap()["op"], "dispose");
}

#[test]
fn partial_count_is_bounded_independently_of_bytes() {
    let (action, _) = scripted(vec![
        Ok(json!({"childId":"c"})),
        Ok(json!({"status":"running","deltas":vec!["";MAX_PARTIAL_EVENTS+1]})),
    ]);
    let (tx, _rx) = mpsc::channel();
    assert!(run_request(
        request(),
        "t".into(),
        &*action,
        tx,
        Arc::new(AtomicBool::new(false))
    )
    .unwrap_err()
    .contains("output limit"));
}

#[test]
fn destroy_joins_the_worker_and_retires_the_handle() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let token = cancelled.clone();
    let exited = Arc::new(AtomicBool::new(false));
    let done = exited.clone();
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _tx = tx;
        while !token.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(1));
        }
        done.store(true, Ordering::Release);
    });
    let handle = JOBS.insert_with_cancel(
        Job {
            receiver: rx,
            completed: false,
            worker: Some(worker),
        },
        cancelled,
    );
    destroy(handle);
    assert!(exited.load(Ordering::Acquire));
    assert!(JOBS.get(handle).is_none());
    destroy(handle);
}

#[test]
fn registration_context_must_match_the_calling_session_or_project() {
    let actual = json!({"sessionId":"s1","cwd":"D:/project"});
    assert!(validate_context(&json!({"sessionId":"s1"}), &actual).is_ok());
    assert!(validate_context(&json!({"cwd":"D:/project"}), &actual).is_ok());
    assert!(validate_context(&json!({"sessionId":"other","cwd":"D:/project"}), &actual).is_err());
    assert!(validate_context(&json!({"cwd":"D:/other"}), &actual).is_err());
    assert!(validate_context(&Value::Null, &actual).is_err());
}
