//! Explicit experimental MCP 2025-11-25 stdio proxy for one isolated native worker.
//!
//! Verified against the versioned official lifecycle, transports and server/tools specs:
//! <https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle>
//! <https://modelcontextprotocol.io/specification/2025-11-25/basic/transports>
//! <https://modelcontextprotocol.io/specification/2025-11-25/server/tools>
//! This is deliberately the legacy initialize handshake, not latest-version support.
//! No desktop work occurs here; all native operations stay in the child krill-app.

mod retention;
mod schema;
#[cfg(test)]
mod tests;
mod transport;
mod view;

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::mpsc::{RecvTimeoutError, TrySendError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::CliError;
use retention::{
    MAX_CACHE_BYTES, MAX_INPUT_BYTES, MAX_REQUESTS, MAX_SEQUENCES, Recorded, Retention, Sequence,
};
use schema::{Hello, VERSION, fields, full_format, must_close, tool_result, unknown};
use transport::{Event, STALL, Transport, encode, worker_path};

const MAX_PENDING: usize = 16;

pub(super) fn run(args: &[String]) -> Result<(), CliError> {
    let path = options(args).map_err(CliError::Usage)?;
    let (io, hello) = Transport::spawn(&path).map_err(CliError::Runtime)?;
    let hello = Hello::parse(hello).map_err(CliError::Runtime)?;
    let mut session = Session::new(io, hello);
    let result = session.serve();
    // Termination is not cancellation of posted input. Pending calls retain their original seq.
    session.finish(
        result
            .as_ref()
            .err()
            .map_or("client closed input", String::as_str),
    );
    result.map_err(CliError::Runtime)
}

fn options(args: &[String]) -> Result<std::path::PathBuf, String> {
    let mut experimental = false;
    let mut worker = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--experimental" if !experimental => experimental = true,
            "--worker" if worker.is_none() => {
                worker = Some(iter.next().ok_or("missing --worker path")?);
            }
            _ => return Err(
                "usage: krill computer serve --experimental [--worker <absolute krill-app path>]"
                    .into(),
            ),
        }
    }
    if !experimental {
        return Err("native persistent mode requires explicit --experimental".into());
    }
    worker_path(worker.map(Path::new))
}

#[derive(Debug)]
struct Control {
    id: Option<String>,
    until: Instant,
}

type Controls = BTreeMap<u64, VecDeque<Control>>;

struct Session {
    io: Transport,
    hello: Hello,
    initialized: bool,
    ready: bool,
    retention: Retention,
    statuses: Controls,
    cancels: Controls,
    queued_output: usize,
    output_progress: Instant,
}

impl Session {
    fn new(io: Transport, hello: Hello) -> Self {
        Self {
            io,
            hello,
            initialized: false,
            ready: false,
            retention: Retention::default(),
            statuses: BTreeMap::new(),
            cancels: BTreeMap::new(),
            queued_output: 0,
            output_progress: Instant::now(),
        }
    }

    fn serve(&mut self) -> Result<(), String> {
        loop {
            self.watchdog()?;
            match self.io.events.recv_timeout(Duration::from_millis(20)) {
                Ok(Event::Client(value)) => self.client(value)?,
                Ok(Event::Native(value)) => self.native(value)?,
                Ok(Event::ClientEnd) => return Ok(()),
                Ok(Event::NativeEnd) => {
                    return Err("native worker EOF; session closed without replay".into());
                }
                Ok(Event::InvalidClient) => {
                    self.emit(&rpc_error(
                        Value::Null,
                        -32700,
                        "invalid UTF-8 JSON line or frame limit exceeded",
                    ))?;
                    return Err("invalid MCP input".into());
                }
                Ok(Event::InvalidNative) => return Err("invalid native worker output".into()),
                Ok(Event::Written) => self.written(),
                Ok(Event::OutputFailed) => {
                    return Err("MCP output failed; posted effects may remain".into());
                }
                Ok(Event::NativeWriteFailed) => {
                    return Err(
                        "native request pipe failed; submitted sequences are unknown".into(),
                    );
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err("transport disconnected".into()),
            }
        }
    }

    fn watchdog(&self) -> Result<(), String> {
        let now = Instant::now();
        if self.queued_output > 0 && now.duration_since(self.output_progress) >= STALL {
            return Err("MCP output stalled; session closed".into());
        }
        if self
            .retention
            .sequences
            .values()
            .any(|s| s.result.is_none() && now >= s.until)
        {
            return Err(
                "native execute deadline elapsed after submission; original seq is unknown".into(),
            );
        }
        if self
            .statuses
            .values()
            .chain(self.cancels.values())
            .flatten()
            .any(|c| now >= c.until)
        {
            return Err("native control deadline elapsed; session closed".into());
        }
        Ok(())
    }

    fn client(&mut self, value: Value) -> Result<(), String> {
        let Some(method) = value
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            self.emit(&rpc_error(
                Value::Null,
                -32600,
                "expected JSON-RPC request or notification",
            ))?;
            return Ok(());
        };
        if value["jsonrpc"] != "2.0" || value.get("params").is_some_and(|v| !v.is_object()) {
            self.emit(&rpc_error(Value::Null, -32600, "invalid JSON-RPC envelope"))?;
            return Ok(());
        }
        let Some(id) = value.get("id").cloned() else {
            return self.notification(&method, &value["params"]);
        };
        let Some(key) = request_key(&id) else {
            return self.emit(&rpc_error(
                Value::Null,
                -32600,
                "id must be a bounded string or integer",
            ));
        };
        if let Some(old) = self.retention.requests.get(&key) {
            if old.input != value {
                return self.emit(&rpc_error(
                    id,
                    -32600,
                    "request id reused with different payload; original request retained",
                ));
            }
            // A repeated pending transport frame does not create another waiter or native request.
            if let Some(response) = old.response.clone() {
                self.emit(&response)?;
            }
            return Ok(());
        }
        let bytes = serde_json::to_vec(&value)
            .map_err(|_| "cannot encode input")?
            .len();
        if !self.retention.room(bytes, 0, 1, 0) {
            return self.emit(&rpc_error(
                id,
                -32000,
                "session request/dedup limit reached; no dispatch",
            ));
        }
        let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
        self.retention.input_bytes += bytes;
        let order = self.retention.next_order();
        self.retention.requests.insert(
            key.clone(),
            Recorded {
                input: value,
                response: None,
                queued: false,
                input_bytes: bytes,
                response_bytes: 0,
                order,
            },
        );
        match method.as_str() {
            // Explicit legacy fallback for clients probing modern discovery. Never claim modern support.
            "server/discover" => self.error(
                &key,
                -32601,
                "method not found; supports legacy MCP 2025-11-25 initialize",
            ),
            "initialize" => self.initialize(&key, &params),
            "ping" => self.reply(&key, json!({})),
            _ if !self.ready => self.error(
                &key,
                -32002,
                "initialize and notifications/initialized are required",
            ),
            "tools/list" => {
                if fields(&params, &["cursor", "_meta"]).is_err() || params.get("cursor").is_some()
                {
                    self.error(&key, -32602, "this bounded tool list has no cursor")
                } else {
                    self.reply(&key, self.hello.tools())
                }
            }
            "tools/call" => self.call(&key, &params),
            _ => self.error(&key, -32601, "method not found"),
        }
    }

    fn initialize(&mut self, key: &str, params: &Value) -> Result<(), String> {
        if self.initialized {
            return self.error(key, -32600, "session already initialized");
        }
        if params["protocolVersion"].as_str().is_none()
            || !params["capabilities"].is_object()
            || params["clientInfo"]["name"].as_str().is_none()
            || params["clientInfo"]["version"].as_str().is_none()
        {
            return self.error(
                key,
                -32602,
                "initialize requires protocolVersion, capabilities and clientInfo",
            );
        }
        self.initialized = true;
        self.reply(key, json!({"protocolVersion":VERSION,"capabilities":{"tools":{}},
            "serverInfo":{"name":"krill-computer-experimental","version":env!("CARGO_PKG_VERSION")},
            "instructions":"Experimental native worker, MCP 2025-11-25 only. Call computer_session for actual capabilities/limits. Caller owns positive monotonic seq; never allocate a new seq to retry uncertain input. No worker restart, replay or backend fallback. Mutations require a native snapshot from this epoch/context."}))
    }

    fn notification(&mut self, method: &str, params: &Value) -> Result<(), String> {
        match method {
            "notifications/initialized" if self.initialized => self.ready = true,
            "notifications/cancelled" if self.ready => {
                let Some(key) = request_key(&params["requestId"]) else {
                    return Ok(());
                };
                let seq = self.retention.sequences.iter().find_map(|(seq, state)| {
                    (state.result.is_none() && state.waiting.contains(&key)).then_some(*seq)
                });
                if let Some(seq) = seq {
                    // A duplicate cancellation notification cannot dispatch an effect. Bound control traffic too.
                    if !self.cancels.contains_key(&seq) && self.pending() < MAX_PENDING {
                        self.control(None, seq, false)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn call(&mut self, key: &str, params: &Value) -> Result<(), String> {
        if fields(params, &["name", "arguments", "_meta"]).is_err() {
            return self.error(key, -32602, "invalid tools/call fields");
        }
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let name = params["name"].as_str().unwrap_or("");
        if name == "computer_session" {
            if fields(&arguments, &[]).is_err() {
                return self.error(key, -32602, "computer_session takes no arguments");
            }
            let mut hello = self.hello.raw.clone();
            hello["proxy"] = json!({"protocolVersion":VERSION,"experimental":true,
                "maxPending":MAX_PENDING,"retainedRpcRequests":MAX_REQUESTS,
                "retainedSequences":MAX_SEQUENCES,"maxInputBytes":MAX_INPUT_BYTES,
                "maxRetainedResultBytes":MAX_CACHE_BYTES,"outputQueueFrames":transport::OUTPUT_QUEUE,
                "maxFrameBytes":transport::MAX_FRAME,"readQueueFrames":transport::QUEUE,
                "defaultObservationFormat":"compact","observationFormats":["compact","full"],
                "cachePolicy":"rolling_completed_only","expiredSequenceReplay":"never"});
            return self.reply(key, tool_result(hello));
        }
        if !matches!(
            name,
            "computer_execute" | "computer_status" | "computer_cancel"
        ) {
            return self.error(key, -32602, "unknown tool");
        }
        let allowed: &[&str] = if name == "computer_execute" {
            &["seq", "deadlineMs", "op", "format"]
        } else {
            &["seq"]
        };
        if fields(&arguments, allowed).is_err() {
            return self.error(
                key,
                -32602,
                "invalid tool arguments; epoch/context are proxy-owned",
            );
        }
        let Some(seq) = arguments["seq"].as_u64().filter(|seq| *seq > 0) else {
            return self.error(key, -32602, "seq is required and must be a positive u64");
        };
        if self.pending() >= MAX_PENDING {
            return self.error(key, -32000, "pending request limit reached; no dispatch");
        }
        if name == "computer_execute" {
            self.execute(key, seq, arguments)
        } else {
            match self.control(Some(key.to_owned()), seq, name == "computer_status") {
                Ok(()) => Ok(()),
                Err(error) => self.error(key, -32000, &error),
            }
        }
    }

    fn execute(&mut self, key: &str, seq: u64, args: Value) -> Result<(), String> {
        if let Err(error) = full_format(&args) {
            return self.error(key, -32602, &error);
        }
        let Some(deadline) = args["deadlineMs"]
            .as_u64()
            .filter(|d| *d > 0 && *d <= self.hello.max_deadline)
        else {
            return self.error(
                key,
                -32602,
                "deadlineMs is required and exceeds native limits or is zero",
            );
        };
        if let Err(error) = self.hello.validate_op(&args["op"]) {
            return self.error(key, -32602, &error);
        }
        if let Some(old) = self.retention.sequences.get_mut(&seq) {
            if old.args != args {
                return self.error(
                    key,
                    -32602,
                    "seq reused with different payload; no dispatch",
                );
            }
            if let Some(result) = old.result.clone() {
                return self.reply(key, result);
            }
            old.waiting.push(key.to_owned());
            return Ok(());
        }
        if seq <= self.retention.highwater {
            let mut expired = unknown(
                seq,
                "retained sequence result expired; no new dispatch or replay",
            );
            expired["state"] = json!("expired");
            expired["error"]["code"] = json!("expired");
            return self.reply(key, tool_result(expired));
        }
        if seq.saturating_sub(self.retention.highwater) > self.hello.max_gap {
            return self.error(
                key,
                -32602,
                "seq must increase within native gap/session limits; no dispatch",
            );
        }
        let args_bytes = serde_json::to_vec(&args)
            .map_err(|_| "cannot encode operation")?
            .len();
        if !self.retention.room(args_bytes, 0, 0, 1) {
            return self.error(
                key,
                -32000,
                "all bounded sequence/input slots are pending; no dispatch",
            );
        }
        let frame = json!({"type":"request","seq":seq,"deadlineMs":deadline,
            "epoch":self.hello.epoch,"context":self.hello.context,"op":args["op"]});
        if let Err(error) = self.send_native(&frame) {
            return self.error(key, -32000, &error);
        }
        self.retention.highwater = seq;
        self.retention.input_bytes += args_bytes;
        let order = self.retention.next_order();
        self.retention.sequences.insert(
            seq,
            Sequence {
                args,
                until: Instant::now() + Duration::from_millis(deadline),
                waiting: vec![key.to_owned()],
                result: None,
                args_bytes,
                result_bytes: 0,
                order,
            },
        );
        Ok(())
    }

    fn control(&mut self, id: Option<String>, seq: u64, status: bool) -> Result<(), String> {
        let kind = if status { "status" } else { "cancel" };
        self.send_native(&json!({"type":kind,"seq":seq}))?;
        let controls = if status {
            &mut self.statuses
        } else {
            &mut self.cancels
        };
        controls.entry(seq).or_default().push_back(Control {
            id,
            until: Instant::now() + STALL,
        });
        Ok(())
    }

    fn send_native(&self, value: &Value) -> Result<(), String> {
        let bytes = encode(value)?;
        if bytes.len().saturating_sub(1) > self.hello.max_frame {
            return Err("native frame limit exceeded; no dispatch".into());
        }
        self.io
            .native
            .try_send(bytes)
            .map_err(|_| "native writer unavailable or bounded queue full; no dispatch".into())
    }

    fn pending(&self) -> usize {
        self.retention
            .sequences
            .values()
            .filter(|s| s.result.is_none())
            .map(|s| s.waiting.len())
            .sum::<usize>()
            + self
                .statuses
                .values()
                .chain(self.cancels.values())
                .map(VecDeque::len)
                .sum::<usize>()
    }

    fn native(&mut self, value: Value) -> Result<(), String> {
        let kind = value["type"].as_str().ok_or("native frame has no type")?;
        if kind == "protocol_error" {
            return Err("worker reported a protocol error".into());
        }
        let seq = value["seq"].as_u64().ok_or("native frame has no seq")?;
        match kind {
            "accepted" => {
                if !self.retention.sequences.contains_key(&seq) {
                    return Err("native accepted unknown seq".into());
                }
            }
            "result" | "rejected" => {
                if !self.retention.sequences.contains_key(&seq) {
                    return Err("native result has unknown seq".into());
                }
                // An unsolicited repeat cannot overwrite evidence already returned to the client.
                if self
                    .retention
                    .sequences
                    .get(&seq)
                    .is_some_and(|s| s.result.is_some())
                {
                    return Err("worker repeated a terminal result".into());
                }
                let closing = must_close(&value);
                let full = self
                    .retention
                    .sequences
                    .get(&seq)
                    .is_some_and(|state| full_format(&state.args).unwrap_or(false));
                self.complete(seq, &tool_result(view::render(value, full)))?;
                if closing {
                    return Err(
                        "native outcome unknown/quiescing; worker will be terminated, no replay"
                            .into(),
                    );
                }
            }
            "status" | "cancel" => {
                let controls = if kind == "status" {
                    &mut self.statuses
                } else {
                    &mut self.cancels
                };
                let control = controls
                    .get_mut(&seq)
                    .and_then(VecDeque::pop_front)
                    .ok_or("unsolicited native control response")?;
                if controls.get(&seq).is_some_and(VecDeque::is_empty) {
                    controls.remove(&seq);
                }
                let closing = must_close(&value);
                if let Some(id) = control.id {
                    self.reply(&id, tool_result(value))?;
                }
                if closing {
                    return Err(
                        "native status unknown; worker will be terminated, no replay".into(),
                    );
                }
            }
            _ => return Err("unexpected native frame; session closed".into()),
        }
        Ok(())
    }

    fn complete(&mut self, seq: u64, result: &Value) -> Result<(), String> {
        let retained = serde_json::to_vec(&result)
            .map_err(|_| "cannot encode native result")?
            .len();
        if !self.retention.room(0, retained, 0, 0) {
            return Err("bounded native result cache full; original seq remains uncertain".into());
        }
        self.retention.cache_bytes += retained;
        let waiting = if let Some(state) = self.retention.sequences.get_mut(&seq) {
            state.result = Some(result.clone());
            state.result_bytes = retained;
            std::mem::take(&mut state.waiting)
        } else {
            return Err("completion has unknown sequence".into());
        };
        for key in waiting {
            self.reply(&key, result.clone())?;
        }
        Ok(())
    }

    fn id(&self, key: &str) -> Value {
        self.retention
            .requests
            .get(key)
            .map_or(Value::Null, |r| r.input["id"].clone())
    }

    fn reply(&mut self, key: &str, result: Value) -> Result<(), String> {
        let mut response = json!({"jsonrpc":"2.0","id":self.id(key)});
        response["result"] = result;
        self.respond(key, response)
    }

    fn error(&mut self, key: &str, code: i64, message: &str) -> Result<(), String> {
        self.respond(key, rpc_error(self.id(key), code, message))
    }

    fn respond(&mut self, key: &str, mut response: Value) -> Result<(), String> {
        if encode(&response).is_err() {
            // Preserve delivery/verification/cleanup axes when an MCP wrapper exceeds the frame cap.
            if let Some(result) = response.get_mut("result")
                && let Some(data) = result.get_mut("structuredContent")
                && data.get("data").is_some()
            {
                if let Some(object) = data.as_object_mut() {
                    object.remove("data");
                }
                data["observation"] = json!({"state":"failed","reason":"output_too_large"});
                data["error"] = json!({"code":"output_too_large","message":"MCP frame limit exceeded; native data omitted"});
                *result = tool_result(data.clone());
            }
        }
        let bytes = encode(&response)?.len();
        if !self.retention.room(0, bytes, 0, 0) {
            return Err("bounded response cache full; session closed without replay".into());
        }
        self.retention.cache_bytes += bytes;
        if let Some(recorded) = self.retention.requests.get_mut(key) {
            recorded.response = Some(response.clone());
            recorded.response_bytes = bytes;
        }
        self.emit(&response)?;
        if let Some(recorded) = self.retention.requests.get_mut(key) {
            recorded.queued = true;
        }
        Ok(())
    }

    fn emit(&mut self, value: &Value) -> Result<(), String> {
        let bytes = encode(value)?;
        self.io
            .output
            .try_send(bytes)
            .map_err(|_| "bounded MCP output queue full or disconnected")?;
        if self.queued_output == 0 {
            self.output_progress = Instant::now();
        }
        self.queued_output += 1;
        Ok(())
    }

    fn written(&mut self) {
        self.queued_output = self.queued_output.saturating_sub(1);
        self.output_progress = Instant::now();
    }

    fn finish(&mut self, reason: &str) {
        // Stop dispatch first. Killing the worker cannot undo events already posted to the OS.
        self.io.terminate();
        let until = Instant::now() + STALL;
        let unfinished: Vec<_> = self
            .retention
            .requests
            .iter()
            .filter_map(|(key, request)| (!request.queued).then_some(key.clone()))
            .collect();
        for key in unfinished {
            let response = if let Some(response) = self
                .retention
                .requests
                .get(&key)
                .and_then(|r| r.response.clone())
            {
                response
            } else {
                let seq = self
                    .retention
                    .requests
                    .get(&key)
                    .and_then(|r| r.input["params"]["arguments"]["seq"].as_u64());
                if let Some(seq) = seq {
                    let result = self
                        .retention
                        .sequences
                        .get(&seq)
                        .and_then(|state| state.result.clone())
                        .unwrap_or_else(|| tool_result(unknown(seq, reason)));
                    json!({"jsonrpc":"2.0","id":self.id(&key),"result":result})
                } else {
                    rpc_error(self.id(&key), -32000, "session closed before response")
                }
            };
            // Final responses bypass the replay cache: the session is closing. This also preserves
            // known delivery when a result arrived just as the bounded cache filled.
            if let Err(error) = self.emit_final(&response, until) {
                eprintln!("krill computer serve: final response unavailable; {error}");
            }
        }
        // Bounded flush only; a client that stopped reading cannot hold this session alive forever.
        while self.queued_output > 0 && Instant::now() < until {
            match self.io.events.recv_timeout(Duration::from_millis(20)) {
                Ok(Event::Written) => self.written(),
                Ok(Event::OutputFailed) | Err(RecvTimeoutError::Disconnected) => break,
                _ => {}
            }
        }
        if self.queued_output > 0 {
            eprintln!(
                "krill computer serve: output incomplete; submitted sequences remain uncertain"
            );
        }
    }

    fn emit_final(&mut self, value: &Value, until: Instant) -> Result<(), String> {
        let mut bytes = encode(value)?;
        loop {
            match self.io.output.try_send(bytes) {
                Ok(()) => {
                    self.queued_output += 1;
                    return Ok(());
                }
                Err(TrySendError::Disconnected(_)) => return Err("MCP output disconnected".into()),
                Err(TrySendError::Full(frame)) => bytes = frame,
            }
            if Instant::now() >= until {
                return Err("bounded final output deadline elapsed".into());
            }
            match self.io.events.recv_timeout(Duration::from_millis(20)) {
                Ok(Event::Written) => self.written(),
                Ok(Event::OutputFailed) | Err(RecvTimeoutError::Disconnected) => {
                    return Err("MCP output failed".into());
                }
                _ => {}
            }
        }
    }
}

fn request_key(id: &Value) -> Option<String> {
    match id {
        Value::String(value) if value.len() <= 256 => Some(format!("s:{value}")),
        Value::Number(number) if number.is_i64() || number.is_u64() => Some(format!("n:{number}")),
        _ => None,
    }
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    let mut response = json!({"jsonrpc":"2.0","error":{"code":code,"message":message}});
    response["id"] = id;
    response
}
