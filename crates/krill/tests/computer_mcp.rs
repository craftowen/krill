//! Exercise the public MCP process with an intentionally simple native peer.
//! No desktop API is invoked; an effect log detects accidental replay or restart.
#![cfg(target_os = "macos")]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-test process and bounded-response helpers"
)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Peer {
    child: Child,
    input: ChildStdin,
    output: Receiver<Value>,
    pending: BTreeMap<u64, Value>,
    directory: PathBuf,
}

impl Peer {
    fn start() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "krill-mcp-e2e-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("native-peer.py");
        fs::write(&path, r"#!/usr/bin/python3
import json, os, pathlib, sys
root=pathlib.Path(__file__).parent
def log(event):
    with (root/'effects.jsonl').open('a') as f: f.write(json.dumps(event)+'\n')
def send(value): print(json.dumps(value),flush=True)
log({'kind':'launch','pid':os.getpid()})
send({'type':'hello','contract':'krill-computer-worker/1','epoch':'E','context':'C','pid':os.getpid(),
      'supported':True,'capabilities':['capabilities','observe','set_value','action','batch'], 'unsupported':[],
      'limits':{'maxFrameBytes':1048576,'maxDeadlineMs':30000,'maxSeqGap':1024,'maxTextUtf16':4096,
                'maxObserveNodes':2000,'queueCapacity':4,'ledgerCapacity':64,'maxBatchSteps':4}})
held=set()
for line in sys.stdin:
    value=json.loads(line); kind=value.get('type'); seq=value.get('seq')
    if kind=='shutdown': break
    if kind=='status':
        send({'type':'status','seq':seq,'state':'queued' if seq in held else 'completed'}); continue
    if kind=='cancel':
        send({'type':'cancel','seq':seq,'outcome':'cancelled' if seq in held else 'already_finished'})
        if seq in held:
            held.remove(seq)
            send({'type':'result','seq':seq,'state':'not_dispatched','delivery':{'state':'not_dispatched','reason':'cancelled'},
                  'observation':{'state':'not_requested'},'verification':{'state':'not_requested'},'cleanup':{'state':'not_needed'}})
        continue
    if kind!='request': continue
    log({'kind':'request','seq':seq,'op':value['op']})
    send({'type':'accepted','seq':seq})
    text=value['op'].get('text')
    if text=='hold': held.add(seq); continue
    if text=='eof': sys.exit(0)
    if text=='eof-tail':
        sys.stdout.write(json.dumps({'type':'result','seq':seq,'state':'completed','delivery':{'state':'submitted','issuedUnits':1},
            'observation':{'state':'not_requested'},'verification':{'state':'satisfied','predicate':'spoof'},'cleanup':{'state':'completed'}}))
        sys.stdout.flush(); sys.exit(3)
    if value['op'].get('kind')=='batch':
        send({'type':'result','seq':seq,'state':'completed','delivery':{'state':'partial','issuedUnits':1,'reason':'later_step_refused'},
              'observation':{'state':'not_requested'},'verification':{'state':'not_requested'},'cleanup':{'state':'completed'},
              'error':{'code':'batch_stopped','message':'known first step issued, second refused'},'data':{'issuedSteps':1}})
        continue
    if value['op'].get('kind')=='observe':
        pid=value['op']['pid']; snapshot='E/C/'+str(seq)
        elements=[{'key':'e0','role':'AXWindow','title':'Fixture'}]
        if seq%2: elements.append({'key':'e1','role':'AXTextField','value':'old'})
        else: elements.append({'key':'e2','role':'AXTextField','value':'new','focused':True})
        if value['op'].get('limit') in (64,65):
            generation=1 if seq==1 else 2
            elements=[{'key':'e'+str(i),'role':'AXTextField','value':('generation-'+str(generation)+':'+str(i))*32} for i in range(64)]
            if value['op']['limit']==65: elements[0]['value']='next value'
        send({'type':'result','seq':seq,'state':'completed','delivery':{'state':'read_only'},
              'observation':{'state':'observed','snapshot':snapshot,'coverage':{'nodes':len(elements),'truncated':pid==43,'reasons':['nodes'] if pid==43 else []}},
              'verification':{'state':'not_requested'},'cleanup':{'state':'not_needed'},
              'data':{'snapshot':snapshot,'app':{'pid':pid,'startWitness':'1.000000'},'window':{'index':0},'elements':elements}})
        continue
    delivery={'state':'unknown','reason':'reply_lost'} if text=='unknown' else {'state':'read_only'}
    send({'type':'result','seq':seq,'state':'unknown' if text=='unknown' else 'completed','delivery':delivery,
          'observation':{'state':'not_requested'},'verification':{'state':'not_requested'},'cleanup':{'state':'not_needed'},
          'data':{'observedSeq':seq}})
log({'kind':'exit'})
").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_krill"))
            .args(["computer", "serve", "--experimental", "--worker"])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, output) = mpsc::sync_channel(32);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(value) = serde_json::from_str(&line) else {
                    break;
                };
                if send.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            output,
            pending: BTreeMap::new(),
            directory,
        }
    }

    fn send(&mut self, value: &Value) {
        serde_json::to_writer(&mut self.input, value).unwrap();
        self.input.write_all(b"\n").unwrap();
        self.input.flush().unwrap();
    }

    fn response(&mut self, id: u64) -> Value {
        if let Some(value) = self.pending.remove(&id) {
            return value;
        }
        for _ in 0..32 {
            let value = self
                .output
                .recv_timeout(Duration::from_secs(10))
                .expect("MCP response deadline");
            let received = value["id"]
                .as_u64()
                .expect("unexpected MCP notification or non-RPC stdout");
            if received == id {
                return value;
            }
            self.pending.insert(received, value);
        }
        panic!("MCP response limit reached");
    }

    fn rpc(&mut self, id: u64, method: &str, params: &Value) -> Value {
        self.send(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}));
        self.response(id)
    }

    fn initialize(&mut self) {
        // Modern clients use this error to choose the documented legacy handshake.
        let discover = self.rpc(0, "server/discover", &json!({}));
        assert_eq!(discover["error"]["code"], -32601);
        let init = self.rpc(
            1,
            "initialize",
            &json!({"protocolVersion":"2025-11-25",
            "capabilities":{}, "clientInfo":{"name":"krill-e2e","version":"1"}}),
        );
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25", "{init}");
        assert!(init["result"]["capabilities"]["tools"].is_object());
        self.send(&json!({"jsonrpc":"2.0", "method":"notifications/initialized"}));
    }

    fn execute(&mut self, id: u64, seq: u64, op: &Value) -> Value {
        self.rpc(
            id,
            "tools/call",
            &json!({"name":"computer_execute",
            "arguments":{"seq":seq,"deadlineMs":5000,"op":op}}),
        )
    }

    fn log(&self) -> Vec<Value> {
        fs::read_to_string(self.directory.join("effects.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn expect_exit(&mut self) {
        let until = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < until, "MCP session failed to close");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
        fs::remove_dir_all(&self.directory).ok();
    }
}

fn set_value(text: &str) -> Value {
    json!({"kind":"set_value", "target":{"snapshot":"E/C/1","element":"e1"},
        "text":text,"verify":true})
}

#[test]
fn mcp_reuses_one_worker_and_replay_after_cache_eviction_never_dispatches() {
    let mut peer = Peer::start();
    peer.initialize();
    let list = peer.rpc(2, "tools/list", &json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    for name in [
        "computer_session",
        "computer_execute",
        "computer_status",
        "computer_cancel",
    ] {
        assert!(tools.iter().any(|tool| tool["name"] == name));
    }
    let first = peer.execute(3, 1, &json!({"kind":"capabilities"}));
    assert_eq!(first["result"]["isError"], false, "{first}");
    let replay = peer.execute(3, 1, &json!({"kind":"capabilities"}));
    assert_eq!(replay, first);
    let same_seq_new_rpc = peer.execute(4, 1, &json!({"kind":"capabilities"}));
    assert_eq!(same_seq_new_rpc["result"], first["result"]);
    for seq in 2..=150 {
        let response = peer.execute(seq + 10, seq, &json!({"kind":"capabilities"}));
        assert_eq!(
            response["result"]["isError"], false,
            "seq {seq}: {response}"
        );
    }
    let expired = peer.execute(200, 1, &json!({"kind":"capabilities"}));
    assert!(
        expired.get("error").is_some() || expired["result"]["isError"] == true,
        "{expired}"
    );
    let log = peer.log();
    assert_eq!(
        log.iter().filter(|event| event["kind"] == "launch").count(),
        1
    );
    assert_eq!(
        log.iter()
            .filter(|event| event["kind"] == "request")
            .count(),
        150
    );
}

#[test]
fn mcp_controls_remain_responsive_during_a_pending_native_request() {
    let mut peer = Peer::start();
    peer.initialize();
    peer.send(
        &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
        "name":"computer_execute","arguments":{"seq":1,"deadlineMs":5000,"op":set_value("hold")}}}),
    );
    let status = peer.rpc(
        4,
        "tools/call",
        &json!({"name":"computer_status","arguments":{"seq":1}}),
    );
    assert_eq!(
        status["result"]["structuredContent"]["state"], "queued",
        "{status}"
    );
    let cancelled = peer.rpc(
        5,
        "tools/call",
        &json!({"name":"computer_cancel","arguments":{"seq":1}}),
    );
    assert_eq!(
        cancelled["result"]["structuredContent"]["outcome"], "cancelled",
        "{cancelled}"
    );
    let result = peer.response(3);
    assert_eq!(
        result["result"]["structuredContent"]["delivery"]["state"], "not_dispatched",
        "{result}"
    );
    assert_eq!(
        peer.log()
            .iter()
            .filter(|event| event["kind"] == "request")
            .count(),
        1
    );
}

#[test]
fn mcp_unknown_and_post_send_eof_close_without_restart_or_replay() {
    for text in ["unknown", "eof", "eof-tail"] {
        let mut peer = Peer::start();
        peer.initialize();
        let response = peer.execute(3, 1, &set_value(text));
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(
            response["result"]["structuredContent"]["delivery"]["state"], "unknown",
            "{response}"
        );
        peer.expect_exit();
        let log = peer.log();
        assert_eq!(
            log.iter().filter(|event| event["kind"] == "launch").count(),
            1
        );
        assert_eq!(
            log.iter()
                .filter(|event| event["kind"] == "request")
                .count(),
            1
        );
    }
}

#[test]
fn mcp_native_zero_indices_and_foreign_epoch_refusal_match_the_worker_contract() {
    let mut peer = Peer::start();
    peer.initialize();
    let zero_window = peer.execute(3, 1, &json!({"kind":"observe","pid":42,"window":0}));
    assert_eq!(zero_window["result"]["isError"], false, "{zero_window}");
    let root_action = peer.execute(
        4,
        2,
        &json!({"kind":"action","name":"AXRaise",
        "target":{"snapshot":"E/C/1","element":"e0"}}),
    );
    assert_eq!(root_action["result"]["isError"], false, "{root_action}");
    let foreign = peer.execute(
        5,
        3,
        &json!({"kind":"action","name":"AXPress",
        "target":{"snapshot":"OLD/C/1","element":"e1"}}),
    );
    assert!(
        foreign.get("error").is_some() || foreign["result"]["isError"] == true,
        "{foreign}"
    );
    assert_eq!(
        peer.log()
            .iter()
            .filter(|event| event["kind"] == "request")
            .count(),
        2
    );
}

#[test]
fn mcp_known_partial_batch_is_an_error_but_does_not_poison_the_session() {
    let mut peer = Peer::start();
    peer.initialize();
    let invalid = peer.execute(
        3,
        1,
        &json!({"kind":"batch","steps":[{"kind":"batch","steps":[]}]}),
    );
    assert!(
        invalid.get("error").is_some() || invalid["result"]["isError"] == true,
        "{invalid}"
    );
    let partial = peer.execute(
        4,
        1,
        &json!({"kind":"batch","steps":[set_value("first"), set_value("second")]}),
    );
    assert_eq!(partial["result"]["isError"], true, "{partial}");
    assert_eq!(
        partial["result"]["structuredContent"]["delivery"]["state"],
        "partial"
    );
    assert_eq!(partial["result"]["structuredContent"]["state"], "completed");
    let next = peer.execute(5, 2, &json!({"kind":"capabilities"}));
    assert_eq!(next["result"]["isError"], false, "{next}");
    let log = peer.log();
    assert_eq!(
        log.iter().filter(|event| event["kind"] == "launch").count(),
        1
    );
    assert_eq!(
        log.iter()
            .filter(|event| event["kind"] == "request")
            .count(),
        2
    );
}

#[test]
fn mcp_delta_is_one_fresh_observe_with_stable_replay_and_explicit_fallbacks() {
    let mut peer = Peer::start();
    peer.initialize();
    let execute = |seq, format: &str, base: Option<&str>, pid| {
        let mut arguments = json!({"seq":seq,"deadlineMs":5000,"format":format,
            "op":{"kind":"observe","pid":pid,"window":0}});
        if let Some(base) = base {
            arguments["baseSnapshot"] = json!(base);
        }
        json!({"name":"computer_execute","arguments":arguments})
    };
    let full = peer.rpc(2, "tools/call", &execute(1, "full", None, 42));
    assert!(full["result"]["structuredContent"]["data"]["elements"].is_array());
    let payload = execute(2, "delta", Some("E/C/1"), 42);
    let delta = peer.rpc(3, "tools/call", &payload);
    let data = &delta["result"]["structuredContent"]["data"];
    assert_eq!(data["format"], "delta", "{delta}");
    assert_eq!(data["snapshot"], "E/C/2");
    assert_eq!(data["baseSnapshot"], "E/C/1");
    assert_eq!(data["order"], json!(["e0", "e2"]));
    assert_eq!(data["removed"], json!(["e1"]));
    assert_eq!(
        data["upsert"],
        json!([{"key":"e2","role":"AXTextField","value":"new","focused":true}])
    );
    assert_eq!(
        peer.rpc(4, "tools/call", &payload)["result"],
        delta["result"]
    );
    let changed_base = peer.rpc(5, "tools/call", &execute(2, "delta", Some("E/C/2"), 42));
    assert_eq!(changed_base["error"]["code"], -32602);
    let changed_format = peer.rpc(6, "tools/call", &execute(2, "full", None, 42));
    assert_eq!(changed_format["error"]["code"], -32602);
    let invalid = peer.rpc(
        7,
        "tools/call",
        &execute(3, "delta", Some("foreign/C/1"), 42),
    );
    assert_eq!(invalid["error"]["code"], -32602);
    for (id, args) in [
        (8, execute(3, "delta", None, 42)),
        (9, execute(3, "full", Some("E/C/1"), 42)),
    ] {
        assert_eq!(peer.rpc(id, "tools/call", &args)["error"]["code"], -32602);
    }
    for seq in 3..=6 {
        peer.rpc(seq + 10, "tools/call", &execute(seq, "full", None, 42));
    }
    // The base has rolled out, but the completed seq keeps its original delta bytes.
    assert_eq!(
        peer.rpc(20, "tools/call", &payload)["result"],
        delta["result"]
    );
    let missing = peer.rpc(21, "tools/call", &execute(7, "delta", Some("E/C/1"), 42));
    assert_eq!(
        missing["result"]["structuredContent"]["data"]["deltaFallback"]["reason"],
        "base_missing_or_evicted"
    );
    let partial = peer.rpc(22, "tools/call", &execute(8, "full", None, 43));
    assert_eq!(
        partial["result"]["structuredContent"]["observation"]["coverage"]["truncated"],
        true
    );
    let partial_base = peer.rpc(23, "tools/call", &execute(9, "delta", Some("E/C/8"), 43));
    let fallback = &partial_base["result"]["structuredContent"]["data"];
    assert_eq!(fallback["format"], "compact");
    assert_eq!(fallback["deltaFallback"]["reason"], "base_partial");
    assert!(fallback.get("removed").is_none());
    let log = peer.log();
    assert_eq!(log.iter().filter(|e| e["kind"] == "launch").count(), 1);
    let requests: Vec<_> = log.iter().filter(|e| e["kind"] == "request").collect();
    assert_eq!(requests.len(), 9);
    assert!(requests.iter().all(|e| e["op"]["kind"] == "observe"));
    assert!(
        requests
            .iter()
            .all(|e| e["op"].get("baseSnapshot").is_none())
    );
}

#[test]
fn mcp_adaptive_full_is_a_replayable_base_without_an_extra_observe() {
    let mut peer = Peer::start();
    peer.initialize();
    let execute = |seq, format: &str, base: Option<&str>, limit| {
        let mut arguments = json!({"seq":seq,"deadlineMs":5000,"format":format,
            "op":{"kind":"observe","pid":42,"window":0,"limit":limit}});
        if let Some(base) = base {
            arguments["baseSnapshot"] = json!(base);
        }
        json!({"name":"computer_execute","arguments":arguments})
    };
    peer.rpc(2, "tools/call", &execute(1, "full", None, 64));
    let request = execute(2, "delta", Some("E/C/1"), 64);
    let full = peer.rpc(3, "tools/call", &request);
    let data = &full["result"]["structuredContent"]["data"];
    assert_eq!(data["format"], "full");
    assert_eq!(data["snapshot"], "E/C/2");
    assert_eq!(
        data["deltaFallback"]["reason"],
        "delta_not_smaller_than_full"
    );
    assert_eq!(data["elements"].as_array().unwrap().len(), 64);
    assert!(data.get("upsert").is_none());
    assert_eq!(
        peer.rpc(4, "tools/call", &request)["result"],
        full["result"]
    );
    let next = peer.rpc(5, "tools/call", &execute(3, "delta", Some("E/C/2"), 65));
    let delta = &next["result"]["structuredContent"]["data"];
    assert_eq!(delta["format"], "delta");
    assert_eq!(
        delta["upsert"],
        json!([{"key":"e0","role":"AXTextField","value":"next value"}])
    );
    assert_eq!(delta["order"].as_array().unwrap().len(), 64);
    assert_eq!(
        peer.log().iter().filter(|e| e["kind"] == "request").count(),
        3
    );
}
