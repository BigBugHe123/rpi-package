//! Offline RPC peer for native ACP plugin tests. No model or tool executes.
use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn send(message: Value) {
    println!("{message}");
    std::io::stdout().flush().unwrap();
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let argument = |name| {
        args.windows(2)
            .find(|args| args[0] == name)
            .map(|args| args[1].clone())
    };
    let session_id = argument("--session-id").unwrap_or_else(|| {
        let history = std::fs::read_to_string(argument("--session").unwrap()).unwrap();
        serde_json::from_str::<Value>(history.lines().next().unwrap()).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    });
    if let Some(id) = argument("--session-id") {
        let directory =
            std::path::PathBuf::from(argument("--session-dir").unwrap()).join("fixture");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("session.jsonl"),
            format!("{}\n", json!({"kind":"header","version":4,"id":id})),
        )
        .unwrap();
    }
    for line in std::io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line.unwrap()).unwrap();
        if message["type"] == "get_state" {
            send(json!({
                "type": "response",
                "id": message["id"],
                "status": "ok",
                "result": {"leafId": null, "sessionId": session_id}
            }));
        }
    }
}
