//! Language servers for one window's folder. A server starts the first
//! time a file of its language opens, gets the open buffers (whole text,
//! a moment after each edit), and answers go to definition and find
//! references. When there is no server, it's still starting, or it doesn't
//! answer in time, the workspace falls back to the built-in symbol index.
//!
//! A server stops when its window closes, or a while after its last file
//! closed.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{App, Context, Entity, EntityId, SharedString, Subscription, Task};
use me_lsp::registry::{self, Group};
use me_lsp::types as lsp;
use me_lsp::{Server, Status};
use me_text::Rope;

use crate::editor::{EditorEvent, EditorView};
use crate::settings;

/// How long after an edit the server gets the new text.
const SYNC_DELAY: Duration = Duration::from_millis(250);
/// How long a server with no open files keeps running.
const IDLE_STOP: Duration = Duration::from_secs(10 * 60);
/// Buffers bigger than this aren't sent to servers.
const MAX_SYNC_SIZE: usize = 4 * 1024 * 1024;

/// A place a server pointed at, ready to open.
#[derive(Clone, Debug)]
pub struct Target {
    pub path: PathBuf,
    /// 0-based line and char column.
    pub line: usize,
    pub col: usize,
    /// The line, trimmed (for pickers).
    pub line_text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Query {
    Definition,
    References,
}

/// What the status bar shows for the active file's server.
pub struct ServerInfo {
    pub name: SharedString,
    pub status: Status,
}

struct Entry {
    group: &'static Group,
    /// The server's name, once one was found.
    name: Option<String>,
    server: Option<Arc<Server>>,
    status: Status,
    /// Initialized: requests and document sync go through.
    ready: bool,
    /// No server installed, or turned off in settings or by the user.
    off: bool,
    _task: Option<Task<()>>,
    idle: Option<Task<()>>,
}

/// What a buffer is to its server.
#[derive(Clone, PartialEq)]
struct DocTarget {
    path: PathBuf,
    uri: lsp::Uri,
    group: &'static str,
    language_id: &'static str,
}

struct Doc {
    editor: gpui::WeakEntity<EditorView>,
    target: Option<DocTarget>,
    version: i32,
    /// Revision of the text the server has; `None` until opened there.
    synced: Option<u64>,
    _subscriptions: Vec<Subscription>,
}

pub struct LanguageServers {
    root: PathBuf,
    servers: HashMap<&'static str, Entry>,
    docs: HashMap<EntityId, Doc>,
    sync_task: Option<Task<()>>,
    _quit: Subscription,
}

impl LanguageServers {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let quit = cx.on_app_quit(|this: &mut Self, _| {
            for server in this.servers.values().filter_map(|e| e.server.clone()) {
                server.kill();
            }
            async {}
        });
        Self {
            root,
            servers: HashMap::new(),
            docs: HashMap::new(),
            sync_task: None,
            _quit: quit,
        }
    }

    /// Follows a buffer: opens it in its language's server (starting the
    /// server if needed), keeps the server's copy current, closes it when
    /// the buffer goes away.
    pub fn track(&mut self, editor: &Entity<EditorView>, cx: &mut Context<Self>) {
        let id = editor.entity_id();
        if self.docs.contains_key(&id) {
            return;
        }
        let subscriptions = vec![
            cx.subscribe(
                editor,
                move |this, _, event: &EditorEvent, cx| match event {
                    EditorEvent::Changed => this.schedule_sync(cx),
                    EditorEvent::Loaded => this.refresh_doc(id, cx),
                    EditorEvent::Saved => {
                        this.refresh_doc(id, cx);
                        this.sync_doc(id, cx);
                        if let Some((server, target)) = this.doc_server(id) {
                            server.did_save(&target.uri);
                        }
                    }
                    _ => {}
                },
            ),
            cx.observe_release(editor, move |this, _, cx| this.untrack(id, cx)),
        ];
        self.docs.insert(
            id,
            Doc {
                editor: editor.downgrade(),
                target: None,
                version: 0,
                synced: None,
                _subscriptions: subscriptions,
            },
        );
        self.refresh_doc(id, cx);
    }

    fn untrack(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.remove(&id) else {
            return;
        };
        if let Some(target) = doc.target {
            if doc.synced.is_some()
                && let Some(server) = self.live_server(target.group)
            {
                server.did_close(&target.uri);
            }
            self.schedule_idle_stop(target.group, cx);
        }
    }

    /// Re-reads a buffer's path and language (after loading, Save As or a
    /// language change) and moves it to the right server.
    fn refresh_doc(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(&id) else {
            return;
        };
        let target = doc.editor.upgrade().and_then(|editor| {
            let editor = editor.read(cx);
            let path = editor.path()?.to_path_buf();
            let (group, language_id) = registry::group_for(editor.language_name()?)?;
            Some(DocTarget {
                uri: me_lsp::uri_from_path(&path)?,
                path,
                group: group.key,
                language_id,
            })
        });
        if target == doc.target {
            self.sync_doc(id, cx);
            return;
        }
        let old = self.docs.get_mut(&id).and_then(|doc| {
            let old = std::mem::replace(&mut doc.target, target.clone());
            let was_open = doc.synced.take().is_some();
            old.filter(|_| was_open)
        });
        if let Some(old) = old {
            if let Some(server) = self.live_server(old.group) {
                server.did_close(&old.uri);
            }
            self.schedule_idle_stop(old.group, cx);
        }
        if let Some(target) = target
            && let Some(group) = registry::GROUPS.iter().find(|g| g.key == target.group)
        {
            self.ensure_server(group, cx);
            if let Some(entry) = self.servers.get_mut(group.key) {
                entry.idle = None;
            }
            self.sync_doc(id, cx);
        }
    }

    fn live_server(&self, group: &str) -> Option<Arc<Server>> {
        let entry = self.servers.get(group)?;
        entry.server.clone().filter(|s| entry.ready && s.is_alive())
    }

    fn doc_server(&self, id: EntityId) -> Option<(Arc<Server>, DocTarget)> {
        let doc = self.docs.get(&id)?;
        let target = doc.target.clone()?;
        doc.synced?;
        Some((self.live_server(target.group)?, target))
    }

    fn ensure_server(&mut self, group: &'static Group, cx: &mut Context<Self>) {
        if self.servers.contains_key(group.key) {
            return;
        }
        let settings = settings::get(cx)
            .language_servers
            .get(group.key)
            .cloned()
            .unwrap_or_default();
        let mut entry = Entry {
            group,
            name: None,
            server: None,
            status: Status::Starting,
            ready: false,
            off: settings.enabled == Some(false),
            _task: None,
            idle: None,
        };
        if entry.off {
            self.servers.insert(group.key, entry);
            return;
        }
        let candidates: Vec<(String, String, Vec<String>)> = match settings.command {
            Some(command) => {
                let name = Path::new(&command)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| command.clone());
                vec![(name, command, settings.args)]
            }
            None => group
                .servers
                .iter()
                .map(|s| {
                    let args = s.args.iter().map(|a| a.to_string()).collect();
                    (s.name.to_string(), s.command.to_string(), args)
                })
                .collect(),
        };
        let root = self.root.clone();
        let options = settings.initialization_options;
        let key = group.key;
        entry._task = Some(cx.spawn(async move |this, cx| {
            let (tx, mut statuses) = mpsc::unbounded();
            let spawn_root = root.clone();
            let spawned = cx
                .background_executor()
                .spawn(async move {
                    let (name, path, args) =
                        candidates.into_iter().find_map(|(name, command, args)| {
                            registry::which(&command).map(|path| (name, path, args))
                        })?;
                    let path_env = registry::search_path();
                    Some(
                        Server::spawn(&name, &path, &args, &spawn_root, Some(path_env), tx)
                            .map(|server| (name, server)),
                    )
                })
                .await;
            let server = match spawned {
                Some(Ok((name, server))) => {
                    let stored = this.update(cx, |this, cx| {
                        let entry = this.servers.get_mut(key)?;
                        entry.name = Some(name);
                        entry.server = Some(server.clone());
                        cx.notify();
                        Some(())
                    });
                    if !matches!(stored, Ok(Some(()))) {
                        server.kill();
                        return;
                    }
                    server
                }
                failed => {
                    this.update(cx, |this, cx| {
                        if let Some(entry) = this.servers.get_mut(key) {
                            entry.off = true;
                            if let Some(Err(err)) = failed {
                                entry.status = Status::Stopped(err.to_string());
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            if let Err(err) = server.initialize(&root, options).await {
                server.kill();
                this.update(cx, |this, cx| {
                    if let Some(entry) = this.servers.get_mut(key) {
                        entry.status = Status::Stopped(err.to_string());
                    }
                    cx.notify();
                })
                .ok();
                return;
            }
            let opened = this.update(cx, |this, cx| {
                if let Some(entry) = this.servers.get_mut(key) {
                    entry.ready = true;
                }
                let ids: Vec<EntityId> = this
                    .docs
                    .iter()
                    .filter(|(_, d)| d.target.as_ref().is_some_and(|t| t.group == key))
                    .map(|(id, _)| *id)
                    .collect();
                for id in ids {
                    this.sync_doc(id, cx);
                }
                cx.notify();
            });
            if opened.is_err() {
                return;
            }
            while let Some(status) = statuses.next().await {
                let alive = this.update(cx, |this, cx| {
                    if let Some(entry) = this.servers.get_mut(key) {
                        entry.status = status;
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
        self.servers.insert(group.key, entry);
        cx.notify();
    }

    /// Stops a server a while after its last buffer closed.
    fn schedule_idle_stop(&mut self, group: &'static str, cx: &mut Context<Self>) {
        let in_use = self
            .docs
            .values()
            .any(|d| d.target.as_ref().is_some_and(|t| t.group == group));
        let Some(entry) = self.servers.get_mut(group) else {
            return;
        };
        if in_use {
            return;
        }
        entry.idle = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(IDLE_STOP).await;
            this.update(cx, |this, cx| {
                if let Some(entry) = this.servers.remove(group)
                    && let Some(server) = entry.server
                {
                    server.shutdown();
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn schedule_sync(&mut self, cx: &mut Context<Self>) {
        self.sync_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SYNC_DELAY).await;
            this.update(cx, |this, cx| {
                let ids: Vec<EntityId> = this.docs.keys().copied().collect();
                for id in ids {
                    this.sync_doc(id, cx);
                }
            })
            .ok();
        }));
    }

    /// Sends a buffer's text to its server if the server's copy is stale
    /// (opening it there the first time).
    fn sync_doc(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(&id) else {
            return;
        };
        let Some(target) = doc.target.clone() else {
            return;
        };
        let Some(server) = self.live_server(target.group) else {
            return;
        };
        let Some(editor) = doc.editor.upgrade() else {
            return;
        };
        let editor = editor.read(cx);
        let revision = editor.revision();
        if doc.synced == Some(revision) {
            return;
        }
        let rope = editor.state.doc.rope().clone();
        if rope.len_bytes() > MAX_SYNC_SIZE {
            return;
        }
        let doc = self.docs.get_mut(&id).unwrap();
        doc.version += 1;
        if doc.synced.is_none() {
            server.did_open(&target.uri, target.language_id, doc.version, rope);
        } else {
            server.did_change(&target.uri, doc.version, rope);
        }
        doc.synced = Some(revision);
    }

    /// Asks the server of `editor`'s language where the identifier at char
    /// offset `pos` is defined (or used). `None` when no server can answer;
    /// the task fails or comes back empty when it couldn't.
    pub fn locations(
        &mut self,
        editor: &Entity<EditorView>,
        pos: usize,
        query: Query,
        cx: &mut Context<Self>,
    ) -> Option<Task<anyhow::Result<Vec<Target>>>> {
        let id = editor.entity_id();
        self.sync_doc(id, cx);
        let (server, target) = self.doc_server(id)?;
        let provider = match query {
            Query::Definition => "definitionProvider",
            Query::References => "referencesProvider",
        };
        if !server.supports(provider) {
            return None;
        }
        let encoding = server.encoding();
        let position = me_lsp::position(editor.read(cx).state.doc.rope(), pos, encoding);
        let at = lsp::TextDocumentPositionParams {
            text_document: lsp::TextDocumentIdentifier { uri: target.uri },
            position,
        };
        let response: std::pin::Pin<Box<dyn Future<Output = anyhow::Result<Vec<Spot>>>>> =
            match query {
                Query::Definition => {
                    let request =
                        server.request::<lsp::request::GotoDefinition>(lsp::GotoDefinitionParams {
                            text_document_position_params: at,
                            work_done_progress_params: Default::default(),
                            partial_result_params: Default::default(),
                        });
                    Box::pin(async move { Ok(definition_spots(request.await?)) })
                }
                Query::References => {
                    let request =
                        server.request::<lsp::request::References>(lsp::ReferenceParams {
                            text_document_position: at,
                            work_done_progress_params: Default::default(),
                            partial_result_params: Default::default(),
                            context: lsp::ReferenceContext {
                                include_declaration: true,
                            },
                        });
                    Box::pin(async move {
                        Ok(request
                            .await?
                            .unwrap_or_default()
                            .into_iter()
                            .map(|l| (l.uri, l.range.start))
                            .collect())
                    })
                }
            };
        let open = self.open_texts(cx);
        let background = cx.background_executor().clone();
        Some(cx.foreground_executor().spawn(async move {
            let spots = response.await?;
            Ok(background
                .spawn(async move { resolve(spots, &open, encoding) })
                .await)
        }))
    }

    /// The text of every open buffer, by path (cheap rope clones).
    fn open_texts(&self, cx: &App) -> HashMap<PathBuf, Rope> {
        self.docs
            .values()
            .filter_map(|doc| {
                let editor = doc.editor.upgrade()?;
                let editor = editor.read(cx);
                Some((
                    editor.path()?.to_path_buf(),
                    editor.state.doc.rope().clone(),
                ))
            })
            .collect()
    }

    /// The server for `editor`'s language, for the status bar.
    pub fn info(&self, editor: &Entity<EditorView>) -> Option<ServerInfo> {
        let target = self.docs.get(&editor.entity_id())?.target.as_ref()?;
        let entry = self.servers.get(target.group)?;
        let name = entry
            .name
            .clone()
            .or_else(|| entry.group.servers.first().map(|s| s.name.to_string()))?;
        if entry.off && entry.server.is_none() && !matches!(entry.status, Status::Stopped(_)) {
            return None;
        }
        Some(ServerInfo {
            name: name.into(),
            status: entry.status.clone(),
        })
    }

    /// Restarts (or starts again after a stop) `editor`'s server.
    pub fn restart(&mut self, editor: &Entity<EditorView>, cx: &mut Context<Self>) {
        let Some(group) = self.group_of(editor) else {
            return;
        };
        if let Some(server) = self.servers.remove(group.key).and_then(|e| e.server) {
            server.shutdown();
        }
        for doc in self.docs.values_mut() {
            if doc.target.as_ref().is_some_and(|t| t.group == group.key) {
                doc.synced = None;
            }
        }
        self.ensure_server(group, cx);
    }

    /// Stops `editor`'s server until it's restarted.
    pub fn stop(&mut self, editor: &Entity<EditorView>, cx: &mut Context<Self>) {
        let Some(group) = self.group_of(editor) else {
            return;
        };
        let Some(entry) = self.servers.get_mut(group.key) else {
            return;
        };
        if let Some(server) = entry.server.take() {
            server.shutdown();
        }
        entry._task = None;
        entry.ready = false;
        entry.off = true;
        entry.status = Status::Stopped("Stopped".into());
        for doc in self.docs.values_mut() {
            if doc.target.as_ref().is_some_and(|t| t.group == group.key) {
                doc.synced = None;
            }
        }
        cx.notify();
    }

    fn group_of(&self, editor: &Entity<EditorView>) -> Option<&'static Group> {
        let key = self.docs.get(&editor.entity_id())?.target.as_ref()?.group;
        registry::GROUPS.iter().find(|g| g.key == key)
    }
}

impl Drop for LanguageServers {
    fn drop(&mut self) {
        for server in self.servers.values().filter_map(|e| e.server.clone()) {
            server.shutdown();
        }
    }
}

type Spot = (lsp::Uri, lsp::Position);

fn definition_spots(response: Option<lsp::GotoDefinitionResponse>) -> Vec<Spot> {
    match response {
        None => Vec::new(),
        Some(lsp::GotoDefinitionResponse::Scalar(l)) => vec![(l.uri, l.range.start)],
        Some(lsp::GotoDefinitionResponse::Array(ls)) => {
            ls.into_iter().map(|l| (l.uri, l.range.start)).collect()
        }
        Some(lsp::GotoDefinitionResponse::Link(links)) => links
            .into_iter()
            .map(|l| (l.target_uri, l.target_selection_range.start))
            .collect(),
    }
}

/// Turns server positions into lines and char columns, reading files that
/// aren't open.
fn resolve(
    spots: Vec<Spot>,
    open: &HashMap<PathBuf, Rope>,
    encoding: me_lsp::Encoding,
) -> Vec<Target> {
    let mut files: HashMap<PathBuf, Option<Rope>> = HashMap::new();
    let mut targets: Vec<Target> = Vec::new();
    for (uri, position) in spots {
        let Some(path) = me_lsp::path_from_uri(&uri) else {
            continue;
        };
        let rope = files.entry(path.clone()).or_insert_with(|| {
            open.get(&path).cloned().or_else(|| {
                std::fs::read_to_string(&path)
                    .ok()
                    .map(|text| Rope::from_str(&text))
            })
        });
        let Some(rope) = rope else { continue };
        let offset = me_lsp::offset(rope, position, encoding);
        let line = rope.char_to_line(offset);
        let col = offset - rope.line_to_char(line);
        let line_text: String = rope.line(line).chars().take(200).collect();
        let target = Target {
            path,
            line,
            col,
            line_text: line_text.trim().to_string(),
        };
        if !targets
            .iter()
            .any(|t| t.path == target.path && t.line == target.line && t.col == target.col)
        {
            targets.push(target);
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_positions_in_open_and_closed_files() {
        let dir = tempfile::tempdir().unwrap();
        let closed = dir.path().join("closed.rs");
        std::fs::write(&closed, "fn a() {}\n    fn b() {}\n").unwrap();
        let open_path = dir.path().join("open.rs");
        let open = HashMap::from([(open_path.clone(), Rope::from_str("// 😀 x\nunsaved();\n"))]);
        let spot = |path: &Path, line, character| {
            (
                me_lsp::uri_from_path(path).unwrap(),
                lsp::Position::new(line, character),
            )
        };
        let targets = resolve(
            vec![
                spot(&closed, 1, 7),
                spot(&open_path, 0, 6),
                spot(&closed, 1, 7),
                spot(&dir.path().join("gone.rs"), 0, 0),
            ],
            &open,
            me_lsp::Encoding::Utf16,
        );
        assert_eq!(targets.len(), 2, "duplicates and missing files are dropped");
        assert_eq!((targets[0].line, targets[0].col), (1, 7));
        assert_eq!(targets[0].line_text, "fn b() {}");
        // The open buffer wins over the disk, and UTF-16 columns become
        // char columns (the emoji is two UTF-16 units, one char).
        assert_eq!((targets[1].line, targets[1].col), (0, 5));
    }
}
