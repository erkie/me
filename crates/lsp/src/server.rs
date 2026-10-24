//! One language server: a process speaking JSON-RPC on its stdin and
//! stdout. A reader thread routes responses to their requests and answers
//! the server's own requests; a writer thread does the writing, so a big
//! buffer sent to a busy server never blocks the caller.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::future::Future;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use futures::channel::{mpsc, oneshot};
use lsp_types as lsp;
use ropey::Rope;
use serde_json::{Value, json};

use crate::{Encoding, uri_from_path};

/// What a server tells the app, as it happens.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Status(Status),
    Diagnostics(lsp::PublishDiagnosticsParams),
}

/// What a server is doing, for the status bar.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Between spawning and the answer to `initialize`.
    Starting,
    /// Working on something it reported (indexing, building): a message.
    Busy(String),
    Ready,
    /// The process exited; the last lines it printed, if any.
    Stopped(String),
}

type Reply = oneshot::Sender<anyhow::Result<Value>>;

/// A message for the writer thread.
enum Outgoing {
    Message(Value),
    /// A message with a buffer's text spliced in at a JSON pointer, so the
    /// (possibly large) string is built off the caller's thread.
    WithText {
        message: Value,
        pointer: &'static str,
        text: Rope,
    },
    /// A `didChange` from `old` to `new`: only the changed range when
    /// `incremental` (worked out on the writer thread), else all of `new`.
    Change {
        uri: lsp::Uri,
        version: i32,
        old: Option<Rope>,
        new: Rope,
        incremental: Option<Encoding>,
    },
}

/// Work the server reported with `$/progress`.
struct Work {
    token: String,
    title: String,
    message: String,
    percentage: Option<u64>,
}

#[derive(Default)]
struct Progress {
    initialized: bool,
    work: Vec<Work>,
}

impl Progress {
    fn status(&self) -> Status {
        match self.work.last() {
            _ if !self.initialized => Status::Starting,
            Some(work) => {
                let mut text = work.title.clone();
                if !work.message.is_empty() {
                    text = format!("{text} · {}", work.message);
                }
                if let Some(pct) = work.percentage {
                    text = format!("{text} {pct}%");
                }
                Status::Busy(text)
            }
            None => Status::Ready,
        }
    }
}

pub struct Server {
    name: String,
    outgoing: crossbeam_channel::Sender<Outgoing>,
    pending: Arc<Mutex<HashMap<i64, Reply>>>,
    next_id: AtomicI64,
    alive: Arc<AtomicBool>,
    progress: Arc<Mutex<Progress>>,
    events: mpsc::UnboundedSender<Event>,
    child: Mutex<Option<Child>>,
    encoding: Mutex<Encoding>,
    capabilities: Mutex<Value>,
}

impl Server {
    /// Starts `command` in `root`. Status changes and diagnostics are sent
    /// on `events`.
    pub fn spawn(
        name: &str,
        command: &Path,
        args: &[String],
        root: &Path,
        path_env: Option<&OsStr>,
        events: mpsc::UnboundedSender<Event>,
    ) -> anyhow::Result<Arc<Self>> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Servers run cargo, node, go…: give them the login shell's PATH.
        if let Some(path) = path_env {
            cmd.env("PATH", path);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("could not start {}", command.display()))?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        {
            let log = log.clone();
            std::thread::Builder::new()
                .name(format!("{name} stderr"))
                .spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        let mut log = log.lock().unwrap();
                        log.push(line);
                        if log.len() > 20 {
                            log.remove(0);
                        }
                    }
                })?;
        }
        let server = Self::with_transport(name, stdout, stdin, events, Some(log));
        *server.child.lock().unwrap() = Some(child);
        Ok(server)
    }

    /// A server on any pair of streams (tests use pipes).
    pub fn with_transport(
        name: &str,
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        events: mpsc::UnboundedSender<Event>,
        stderr_log: Option<Arc<Mutex<Vec<String>>>>,
    ) -> Arc<Self> {
        let (outgoing, queue) = crossbeam_channel::unbounded::<Outgoing>();
        let pending = Arc::new(Mutex::new(HashMap::<i64, Reply>::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let progress = Arc::new(Mutex::new(Progress::default()));

        std::thread::Builder::new()
            .name(format!("{name} writer"))
            .spawn(move || {
                let mut writer = std::io::BufWriter::new(writer);
                for message in queue {
                    let value = match message {
                        Outgoing::Message(value) => value,
                        Outgoing::WithText {
                            mut message,
                            pointer,
                            text,
                        } => {
                            if let Some(slot) = message.pointer_mut(pointer) {
                                *slot = Value::String(text.to_string());
                            }
                            message
                        }
                        Outgoing::Change {
                            uri,
                            version,
                            old,
                            new,
                            incremental,
                        } => {
                            let change = match (old, incremental) {
                                (Some(old), Some(encoding)) => {
                                    match crate::diff(&old, &new, encoding) {
                                        Some((range, text)) => {
                                            json!({ "range": range, "text": text })
                                        }
                                        None => continue,
                                    }
                                }
                                _ => json!({ "text": new.to_string() }),
                            };
                            json!({
                                "jsonrpc": "2.0",
                                "method": "textDocument/didChange",
                                "params": {
                                    "textDocument": { "uri": uri, "version": version },
                                    "contentChanges": [change],
                                },
                            })
                        }
                    };
                    let body = serde_json::to_vec(&value).unwrap_or_default();
                    let header = format!("Content-Length: {}\r\n\r\n", body.len());
                    if writer.write_all(header.as_bytes()).is_err()
                        || writer.write_all(&body).is_err()
                        || writer.flush().is_err()
                    {
                        break;
                    }
                }
            })
            .ok();

        {
            let name = name.to_string();
            let outgoing = outgoing.clone();
            let pending = pending.clone();
            let alive = alive.clone();
            let progress = progress.clone();
            let events = events.clone();
            std::thread::Builder::new()
                .name(format!("{name} reader"))
                .spawn(move || {
                    let mut reader = BufReader::new(reader);
                    while let Ok(Some(message)) = read_message(&mut reader) {
                        handle_message(message, &outgoing, &pending, &progress, &events);
                    }
                    alive.store(false, Ordering::Release);
                    for (_, reply) in pending.lock().unwrap().drain() {
                        let _ = reply.send(Err(anyhow!("{name} stopped")));
                    }
                    let detail = stderr_log
                        .and_then(|log| log.lock().unwrap().last().cloned())
                        .unwrap_or_default();
                    let _ = events.unbounded_send(Event::Status(Status::Stopped(detail)));
                })
                .ok();
        }

        Arc::new(Self {
            name: name.to_string(),
            outgoing,
            pending,
            next_id: AtomicI64::new(1),
            alive,
            progress,
            events,
            child: Mutex::new(None),
            encoding: Mutex::new(Encoding::Utf16),
            capabilities: Mutex::new(Value::Null),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    /// How the server counts columns (agreed on in `initialize`).
    pub fn encoding(&self) -> Encoding {
        *self.encoding.lock().unwrap()
    }

    /// Whether the server announced `provider` (e.g. `definitionProvider`).
    pub fn supports(&self, provider: &str) -> bool {
        match self.capabilities.lock().unwrap().get(provider) {
            None | Some(Value::Null) | Some(Value::Bool(false)) => false,
            Some(_) => true,
        }
    }

    /// Whether a nested capability is on (`/renameProvider/prepareProvider`).
    pub fn supports_at(&self, pointer: &str) -> bool {
        match self.capabilities.lock().unwrap().pointer(pointer) {
            None | Some(Value::Null) | Some(Value::Bool(false)) => false,
            Some(_) => true,
        }
    }

    /// Whether the server takes edits as changed ranges (else whole text).
    fn incremental_sync(&self) -> bool {
        let capabilities = self.capabilities.lock().unwrap();
        let sync = capabilities.get("textDocumentSync");
        let kind = match sync {
            Some(Value::Object(sync)) => sync.get("change"),
            other => other,
        };
        kind.and_then(Value::as_u64) == Some(2)
    }

    /// The `initialize` handshake. The server is usable once it returns.
    pub async fn initialize(&self, root: &Path, options: Option<Value>) -> anyhow::Result<()> {
        let root_uri = uri_from_path(root).context("folder path is not a valid URI")?;
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "Me", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": root_uri,
            "rootPath": root,
            "workspaceFolders": [{ "uri": root_uri, "name": name }],
            "initializationOptions": options,
            "capabilities": {
                "general": { "positionEncodings": ["utf-8", "utf-16"] },
                "window": { "workDoneProgress": true },
                "workspace": {
                    "configuration": true,
                    "workspaceFolders": true,
                    "workspaceEdit": { "documentChanges": true },
                },
                "textDocument": {
                    "synchronization": { "didSave": true },
                    "hover": { "contentFormat": ["markdown", "plaintext"] },
                    "rename": { "prepareSupport": true },
                    "publishDiagnostics": {
                        "versionSupport": true,
                        "tagSupport": { "valueSet": [1, 2] },
                    },
                    "definition": { "linkSupport": true },
                    "declaration": { "linkSupport": true },
                    "typeDefinition": { "linkSupport": true },
                    "implementation": { "linkSupport": true },
                    "references": {},
                },
            },
        });
        let result = self.request_raw("initialize", params).await?;
        let capabilities = result.get("capabilities").cloned().unwrap_or(Value::Null);
        if capabilities.get("positionEncoding").and_then(Value::as_str) == Some("utf-8") {
            *self.encoding.lock().unwrap() = Encoding::Utf8;
        }
        *self.capabilities.lock().unwrap() = capabilities;
        self.send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
        let status = {
            let mut progress = self.progress.lock().unwrap();
            progress.initialized = true;
            progress.status()
        };
        let _ = self.events.unbounded_send(Event::Status(status));
        Ok(())
    }

    fn send(&self, message: Value) {
        let _ = self.outgoing.send(Outgoing::Message(message));
    }

    /// Sends a request; the future resolves with the server's answer.
    pub fn request_raw(
        &self,
        method: &str,
        params: Value,
    ) -> impl Future<Output = anyhow::Result<Value>> + use<> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let name = self.name.clone();
        self.pending.lock().unwrap().insert(id, tx);
        // Checked after inserting: the reader fails everything pending once
        // it has marked the server dead, so nothing is left waiting.
        if self.is_alive() {
            self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        } else {
            self.pending.lock().unwrap().remove(&id);
        }
        async move { rx.await.map_err(|_| anyhow!("{name} is not running"))? }
    }

    pub fn request<R: lsp::request::Request>(
        &self,
        params: R::Params,
    ) -> impl Future<Output = anyhow::Result<R::Result>> + use<R> {
        let params = serde_json::to_value(params).unwrap_or(Value::Null);
        let response = self.request_raw(R::METHOD, params);
        async move { Ok(serde_json::from_value(response.await?)?) }
    }

    pub fn notify<N: lsp::notification::Notification>(&self, params: N::Params) {
        let params = serde_json::to_value(params).unwrap_or(Value::Null);
        self.send(json!({ "jsonrpc": "2.0", "method": N::METHOD, "params": params }));
    }

    pub fn did_open(&self, uri: &lsp::Uri, language_id: &str, version: i32, text: Rope) {
        let _ = self.outgoing.send(Outgoing::WithText {
            message: json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": { "textDocument": {
                    "uri": uri, "languageId": language_id, "version": version, "text": "",
                } },
            }),
            pointer: "/params/textDocument/text",
            text,
        });
    }

    /// Tells the server a buffer went from `old` (the text it has) to
    /// `new`: just the changed range if it takes those, else the whole text.
    pub fn did_change(&self, uri: &lsp::Uri, version: i32, old: Option<Rope>, new: Rope) {
        let incremental = self.incremental_sync().then(|| self.encoding());
        let _ = self.outgoing.send(Outgoing::Change {
            uri: uri.clone(),
            version,
            old,
            new,
            incremental,
        });
    }

    pub fn did_save(&self, uri: &lsp::Uri) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didSave",
            "params": { "textDocument": { "uri": uri } },
        }));
    }

    pub fn did_close(&self, uri: &lsp::Uri) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": uri } },
        }));
    }

    /// Asks the server to exit, and kills it if it doesn't within a second
    /// or two. Doesn't block.
    pub fn shutdown(self: &Arc<Self>) {
        let mut response = Box::pin(self.request_raw("shutdown", Value::Null));
        let this = self.clone();
        std::thread::Builder::new()
            .name(format!("{} shutdown", self.name))
            .spawn(move || {
                let deadline = Instant::now() + Duration::from_millis(1500);
                let waker = futures::task::noop_waker();
                let mut task_cx = std::task::Context::from_waker(&waker);
                while this.is_alive() && Instant::now() < deadline {
                    if response.as_mut().poll(&mut task_cx).is_ready() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                this.send(json!({ "jsonrpc": "2.0", "method": "exit" }));
                let deadline = Instant::now() + Duration::from_millis(500);
                while Instant::now() < deadline {
                    let exited = this
                        .child
                        .lock()
                        .unwrap()
                        .as_mut()
                        .is_none_or(|c| matches!(c.try_wait(), Ok(Some(_))));
                    if exited {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                this.kill();
            })
            .ok();
    }

    /// Ends the process now.
    pub fn kill(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            // Reap it without blocking the caller.
            std::thread::spawn(move || child.wait());
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.kill();
    }
}

fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<Value>> {
    let mut length = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some((key, value)) = line.split_once(':')
            && key.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length.unwrap_or(0)];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body).unwrap_or(Value::Null)))
}

fn handle_message(
    message: Value,
    outgoing: &crossbeam_channel::Sender<Outgoing>,
    pending: &Mutex<HashMap<i64, Reply>>,
    progress: &Mutex<Progress>,
    events: &mpsc::UnboundedSender<Event>,
) {
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").cloned();
    match (method, id) {
        // A response to one of our requests.
        (None, Some(id)) => {
            let Some(reply) = id
                .as_i64()
                .and_then(|id| pending.lock().unwrap().remove(&id))
            else {
                return;
            };
            let result = match message.get("error") {
                Some(error) => Err(anyhow!(
                    "{}",
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("request failed")
                )),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = reply.send(result);
        }
        // A request from the server: answer so it doesn't wait on us.
        (Some(method), Some(id)) => {
            let params = message.get("params");
            let result = match method {
                "workspace/configuration" => {
                    let items = params
                        .and_then(|p| p.get("items"))
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    Ok(Value::Array(vec![Value::Null; items]))
                }
                "workspace/applyEdit" => Ok(json!({ "applied": false })),
                "window/workDoneProgress/create"
                | "client/registerCapability"
                | "client/unregisterCapability"
                | "window/showMessageRequest"
                | "window/showDocument"
                | "workspace/diagnostic/refresh"
                | "workspace/semanticTokens/refresh"
                | "workspace/inlayHint/refresh"
                | "workspace/codeLens/refresh" => Ok(Value::Null),
                _ => Err(json!({ "code": -32601, "message": "method not found" })),
            };
            let reply = match result {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
            };
            let _ = outgoing.send(Outgoing::Message(reply));
        }
        (Some("textDocument/publishDiagnostics"), None) => {
            if let Some(params) = message
                .get("params")
                .cloned()
                .and_then(|p| serde_json::from_value(p).ok())
            {
                let _ = events.unbounded_send(Event::Diagnostics(params));
            }
        }
        (Some("$/progress"), None) => {
            let Some(params) = message.get("params") else {
                return;
            };
            let token = match params.get("token") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => return,
            };
            let value = params.get("value").unwrap_or(&Value::Null);
            let field = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
            let new_status = {
                let mut progress = progress.lock().unwrap();
                let existing = progress.work.iter().position(|w| w.token == token);
                match field("kind").as_deref() {
                    Some("begin") => {
                        if let Some(ix) = existing {
                            progress.work.remove(ix);
                        }
                        progress.work.push(Work {
                            token,
                            title: field("title").unwrap_or_default(),
                            message: field("message").unwrap_or_default(),
                            percentage: value.get("percentage").and_then(Value::as_u64),
                        });
                    }
                    Some("report") => {
                        let Some(ix) = existing else { return };
                        let work = &mut progress.work[ix];
                        if let Some(message) = field("message") {
                            work.message = message;
                        }
                        if let Some(pct) = value.get("percentage").and_then(Value::as_u64) {
                            work.percentage = Some(pct);
                        }
                    }
                    Some("end") => {
                        if let Some(ix) = existing {
                            progress.work.remove(ix);
                        }
                    }
                    _ => return,
                }
                progress.status()
            };
            let _ = events.unbounded_send(Event::Status(new_status));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    /// A scripted server on the other end of two pipes.
    struct Fake {
        reader: BufReader<std::io::PipeReader>,
        writer: std::io::PipeWriter,
    }

    impl Fake {
        fn recv(&mut self) -> Value {
            read_message(&mut self.reader).unwrap().unwrap()
        }

        fn send(&mut self, value: Value) {
            let body = serde_json::to_vec(&value).unwrap();
            write!(self.writer, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
            self.writer.write_all(&body).unwrap();
        }
    }

    fn connect() -> (Arc<Server>, Fake, mpsc::UnboundedReceiver<Event>) {
        let (client_in, server_out) = std::io::pipe().unwrap();
        let (server_in, client_out) = std::io::pipe().unwrap();
        let (tx, rx) = mpsc::unbounded();
        let server = Server::with_transport("fake", client_in, client_out, tx, None);
        let fake = Fake {
            reader: BufReader::new(server_in),
            writer: server_out,
        };
        (server, fake, rx)
    }

    #[test]
    fn initializes_and_answers_requests() {
        let (server, mut fake, mut status) = connect();
        let root = std::env::temp_dir();
        let script = std::thread::spawn(move || {
            let init = fake.recv();
            assert_eq!(init["method"], "initialize");
            assert_eq!(
                init["params"]["capabilities"]["general"]["positionEncodings"][0],
                "utf-8"
            );
            // Ask the client something before answering.
            fake.send(
                json!({ "jsonrpc": "2.0", "id": "c1", "method": "workspace/configuration",
                "params": { "items": [{ "section": "a" }, { "section": "b" }] } }),
            );
            let answer = fake.recv();
            assert_eq!(answer["id"], "c1");
            assert_eq!(answer["result"], json!([null, null]));
            fake.send(json!({ "jsonrpc": "2.0", "id": init["id"], "result": {
                "capabilities": { "positionEncoding": "utf-8", "definitionProvider": true,
                    "textDocumentSync": { "openClose": true, "change": 2 } } } }));
            assert_eq!(fake.recv()["method"], "initialized");

            let open = fake.recv();
            assert_eq!(open["method"], "textDocument/didOpen");
            assert_eq!(open["params"]["textDocument"]["text"], "fn main() {}\n");
            // Only the edit travels.
            let change = fake.recv();
            assert_eq!(change["params"]["textDocument"]["version"], 2);
            assert_eq!(
                change["params"]["contentChanges"],
                json!([{ "range": { "start": { "line": 0, "character": 11 },
                    "end": { "line": 0, "character": 11 } }, "text": " 1 " }])
            );
            fake.send(json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                "params": { "uri": "file:///tmp/main.rs", "version": 2, "diagnostics": [{
                    "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 7 } },
                    "severity": 1, "message": "oops" }] } }));

            fake.send(json!({ "jsonrpc": "2.0", "method": "$/progress",
                "params": { "token": 7, "value": { "kind": "begin", "title": "Indexing", "percentage": 10 } } }));
            fake.send(json!({ "jsonrpc": "2.0", "method": "$/progress",
                "params": { "token": 7, "value": { "kind": "end" } } }));

            let definition = fake.recv();
            assert_eq!(definition["method"], "textDocument/definition");
            assert_eq!(
                definition["params"]["position"],
                json!({ "line": 0, "character": 3 })
            );
            fake.send(json!({ "jsonrpc": "2.0", "id": definition["id"], "result": [{
                "uri": "file:///tmp/a.rs",
                "range": { "start": { "line": 4, "character": 2 }, "end": { "line": 4, "character": 6 } } }] }));
            fake
        });

        futures::executor::block_on(server.initialize(&root, None)).unwrap();
        assert_eq!(server.encoding(), Encoding::Utf8);
        assert!(server.supports("definitionProvider"));
        assert!(!server.supports("referencesProvider"));

        let uri: lsp::Uri = "file:///tmp/main.rs".parse().unwrap();
        let text = Rope::from_str("fn main() {}\n");
        server.did_open(&uri, "rust", 1, text.clone());
        let mut edited = text.clone();
        edited.insert(11, " 1 ");
        server.did_change(&uri, 2, Some(text), edited);
        let params = lsp::GotoDefinitionParams {
            text_document_position_params: lsp::TextDocumentPositionParams {
                text_document: lsp::TextDocumentIdentifier { uri },
                position: lsp::Position::new(0, 3),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let response =
            futures::executor::block_on(server.request::<lsp::request::GotoDefinition>(params))
                .unwrap();
        match response {
            Some(lsp::GotoDefinitionResponse::Array(locations)) => {
                assert_eq!(locations[0].range.start, lsp::Position::new(4, 2))
            }
            other => panic!("unexpected {other:?}"),
        }

        let events: Vec<Event> = futures::executor::block_on(async {
            let mut seen = Vec::new();
            while let Some(s) = status.next().await {
                seen.push(s);
                if seen.len() == 4 {
                    break;
                }
            }
            seen
        });
        assert_eq!(events[0], Event::Status(Status::Ready));
        match &events[1] {
            Event::Diagnostics(d) => {
                assert_eq!(d.version, Some(2));
                assert_eq!(d.diagnostics[0].message, "oops");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            events[2..],
            [
                Event::Status(Status::Busy("Indexing 10%".into())),
                Event::Status(Status::Ready)
            ]
        );

        // The server going away fails what's pending and reports it.
        drop(script.join().unwrap());
        let late = futures::executor::block_on(server.request_raw("x", Value::Null));
        assert!(late.is_err());
        assert!(!server.is_alive());
    }
}
