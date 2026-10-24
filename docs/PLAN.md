# Me: plan and architecture

A native, fast code editor for reading markdown and looking things up in code.
It should start in about 0.1 s, use little memory, open huge files, and look
like it belongs on macOS. It has no AI features.

This document records the technology decision, the architecture, and the
phased plan. Each phase has acceptance criteria that can be checked.

---

## 1. Requirements

| # | Requirement | Notes |
|---|-------------|-------|
| R1 | Native app, starts in ~0.1 s | Cold start to first painted frame. |
| R2 | Low memory | Target under 60 MB idle with one window. Cursor/VS Code idle at 500 MB+. |
| R3 | File tree sidebar on the left | Lazy loading, respects `.gitignore`. |
| R4 | Tabs | Preview tab (single click) and pinned tab (edit or double click), as in Sublime. |
| R5 | Code editor | Fast syntax highlighting. |
| R6 | Seamless markdown preview | `.md` opens rendered. One key flips to source. No inline editing. |
| R7 | CLI: `me .`, `me file:12`, `-w` wait mode | Works as `EDITOR="me -w"` for git. |
| R8 | Can be the default editor | macOS Launch Services and `$EDITOR`. Linux `.desktop`/xdg-mime. |
| R9 | Fast Cmd+T file search | Fuzzy, incremental, 100k+ files. |
| R10 | Fast project search and replace | ripgrep engine, streaming results. |
| R11 | Multiple selections | Cmd+D, Cmd+L, Cmd+Shift+L, Ctrl+Cmd+G, cursor above/below. |
| R12 | Option-drag column (box) selection and multi-cursor | Cmd+click adds a cursor. |
| R13 | Massive files | Multi-GB files stay responsive. |
| R14 | Automatic light/dark mode | Follows the system live. |
| R15 | Native look, good logo | macOS first. |
| R16 | Cross-platform | macOS first, then Linux and Windows. |

---

## 2. Is Rust the way to go? Yes.

The options considered:

| Option | Startup | RAM | Native feel | Huge files | Cross-platform | Verdict |
|--------|---------|-----|-------------|------------|----------------|---------|
| Swift + AppKit/TextKit 2 | Excellent | Excellent | Best | Weak. TextKit slows down on very large files and long lines. | No | Best on Mac, but needs a rewrite for every other OS. |
| Electron / Tauri (web UI) | Poor or fair | Poor | Weak | Weak | Yes | Rejected. This is the stack the request is moving away from. |
| Rust + egui / iced / Slint / Floem | Good | Good | Weak. Immediate-mode or generic look, weak text and IME. | Fair | Yes | Too much text infrastructure to build ourselves. |
| Rust core + native UI per OS (AppKit via objc2, GTK, WinUI) | Excellent | Excellent | Best | Good | Yes, at 3× the UI work | Too slow to build for one person. |
| **Rust + GPUI** (Zed's UI framework) | **Excellent** | **Good** | **Good.** Native menus, traffic lights, vibrancy, system fonts, Metal. | **Excellent.** GPU text, custom editor element. | **Yes.** macOS (Metal), Linux (Vulkan/GL via wgpu), Windows (DirectX). | **Chosen** |

Why GPUI:

* It was built for a code editor. Zed shows it can render text at 120 fps,
  handle IME, and make native macOS windows, menus, and dialogs.
* Text is shaped by the platform (CoreText on macOS) and drawn by the GPU, so
  we can write our own editor element that paints only the visible lines.
  Multi-cursor, column selection, and huge files all need that.
* It runs on macOS, Linux, and Windows from one codebase.
* [`gpui-component`](https://github.com/longbridge/gpui-kit) adds a good
  markdown renderer (tables, task lists, highlighted code blocks, selectable
  text), virtualized lists and trees, resizable panels, text inputs, and
  light/dark theming. That saves weeks of chrome work.

Risks and mitigations:

* **GPUI is pre-1.0 and its API changes.** We pin exact versions
  (`gpui-pre =0.3.9`, a weekly crates.io snapshot of Zed's gpui, and
  `gpui-component =0.8.0`). Upgrades are deliberate and done one at a time.
  Editor logic lives in UI-independent crates (`me-text`, `me-project`,
  `me-syntax`, `me-ipc`) so most of the code never touches GPUI.
* **The widgets are custom-drawn, not AppKit.** On macOS we use the native
  menu bar, native traffic lights in a transparent titlebar, sidebar
  vibrancy, the system UI font, native open/save panels and alerts, and macOS
  metrics (13 pt UI text, 28 px rows, SF Symbols-like icons). This is the same
  approach Zed and Sublime Text use.
* **Markdown rendering is not ours.** It sits behind our own
  `MarkdownPreview` view, so we can replace it with a pulldown-cmark renderer
  later without touching the rest of the app.

---

## 3. Architecture

```
┌──────────────┐  unix socket / named pipe   ┌────────────────────────────────────────┐
│ editor (CLI) │ ──────────────────────────▶ │ Me.app  (me-app, GPUI)          │
│ ~300 KB, no  │ ◀──── "closed" events ───── │                                        │
│ GUI deps     │       (for -w)              │  Workspace (one per window)            │
└──────────────┘                             │  ├─ Sidebar: FileTree view             │
                                             │  ├─ TabBar                             │
                                             │  ├─ Pane: EditorView | MarkdownPreview │
                                             │  ├─ FileFinder (Cmd+T)                 │
                                             │  └─ SearchPanel (Cmd+Shift+F)          │
                                             └───────────────┬────────────────────────┘
                                                             │ uses (no GPUI below this line)
              ┌──────────────────┬───────────────────────────┼──────────────────┐
              ▼                  ▼                           ▼                  ▼
        me-text        me-syntax              me-project       me-ipc
        rope buffer,       tree-sitter, lazy          file tree, fuzzy      CLI ⇄ app
        selections,        grammars, viewport         finder (nucleo),      protocol
        multi-cursor,      highlighting               search & replace
        undo/redo                                     (ripgrep crates)
```

### Crates

| Crate | Kind | Depends on GPUI | Purpose |
|-------|------|-----------------|---------|
| `crates/text` (`me-text`) | lib | no | `Document` (ropey rope + line endings + undo history), `Selection`/`SelectionSet`, movement (grapheme and word aware), every edit and multi-cursor command, in-file search. Heavily unit tested. |
| `crates/syntax` (`me-syntax`) | lib | no | Language detection, lazily built tree-sitter grammars and queries, incremental reparse, highlight spans for a byte range only (the viewport). Size cutoffs for huge files. |
| `crates/project` (`me-project`) | lib | no | Directory listing for the tree, background file index, fuzzy matching (nucleo), project search (grep-searcher + ignore's parallel walker), safe replace-in-files. |
| `crates/ipc` (`me-ipc`) | lib | no | Socket path, message types, framing, client and server helpers. |
| `crates/cli` (`me-cli`) | bin `me` | no | Parses arguments, connects to or launches the app, forwards the request, waits for `-w`. |
| `crates/app` (`me-app`) | bin `me-app` | yes | GPUI app: windows, views, keymap, theme, menus, platform integration. |

### Key design decisions

**Text storage.** We use a [ropey](https://crates.io/crates/ropey) rope, so
edits cost O(log n), line lookup costs O(log n), and cloning a snapshot is
O(1) (for background highlighting and saving). Positions are char offsets.
Files are kept byte-exact: CRLF is not normalized, and the detected line
ending is used for new lines.

**Rendering.** A custom GPUI `Element` paints only the visible lines. Line
height is fixed and soft wrap is off in code, so scroll offset maps straight
to a line index. Each frame does about 60 shaping calls no matter how big the
file is. On very long lines (minified files) we shape only the visible column
window.

**Highlighting.** Grammars are compiled into the binary, but a
`tree_sitter::Language` and its query are built the first time that language
is used, so startup pays nothing. Parsing runs on a background thread with a
timeout. Highlight queries run only on the visible byte range
(`QueryCursor::set_byte_range`). Files over 8 MB (configurable) get no
syntax highlighting by default, as in Sublime.

**Multi-cursor.** `SelectionSet` is a sorted, merged `Vec<Selection>` with a
primary index. Each edit command builds one batch of non-overlapping changes,
applies it in reverse order, and maps every selection through it. One
transaction means one undo step, even with 100k cursors.

**Markdown.** `.md`, `.markdown`, `.mdx` open in preview mode. `Cmd+Shift+V`
toggles source and preview for the tab. The preview re-renders when the file
changes on disk (file watcher) or when the source tab is edited. Relative
links to other files open in tabs. Relative images resolve against the file.

**Single instance and the CLI.** The app listens on a per-user Unix domain
socket (named pipe on Windows). The CLI is a separate tiny binary with no GUI
dependencies, so it starts in about 1 ms. If no app is listening, the CLI
launches it (`open -a` on macOS, a detached spawn elsewhere) and retries for
up to 5 s. With `-w`, the app sends `closed` events and the CLI exits once
every file it opened is closed.

**Theme.** There are two built-in themes (light and dark) modelled on macOS
and Xcode colors. `window.observe_window_appearance` swaps them live when the
system appearance changes.

---

## 4. Performance budgets

These are acceptance criteria. CI will track them with benchmarks from
Phase 2.

| Metric | Budget (Apple Silicon) |
|--------|------------------------|
| Cold start to first frame (`me .` with the app closed) | ≤ 100 ms |
| Warm `me .` (app running) to new window | ≤ 30 ms |
| Idle RSS, one window, small project | ≤ 60 MB |
| Keystroke to paint | ≤ 8 ms (one frame at 120 Hz) |
| Cmd+T: index 100k files | ≤ 300 ms, in the background, never blocks typing |
| Cmd+T: per-keystroke rematch, 100k files | ≤ 5 ms |
| Project search: first results | ≤ 50 ms; whole-repo throughput about the same as `rg` |
| Open 100 MB file | Interactive in ≤ 500 ms |
| Open 1 GB file | Interactive in ≤ 3 s, with progress; memory ≈ 1.1 × file size |
| Scrolling | 120 fps on any file size |

Rules that keep startup at 0.1 s:

1. Do nothing on the main thread that isn't needed for the first frame. Load
   one level of the tree first. Build the file index, the highlight query,
   and session tabs that aren't visible later.
2. Load no web views, no JS runtime, and no plugin host.
3. Bundle no fonts. Use system fonts (SF Pro and SF Mono/Menlo on macOS).
4. Ship release builds with `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`.

---

## 5. Keybindings (macOS defaults, Sublime flavored)

The default keymap mirrors Sublime Text with VS Code aliases where they don't
conflict. Phase 3 makes it user-configurable in `keymap.json`.

| Action | Keys |
|--------|------|
| Go to file | `Cmd+T`, `Cmd+P` |
| Find in file / replace | `Cmd+F` / `Cmd+Alt+F` |
| Find next / previous | `Cmd+G` / `Cmd+Shift+G` (`F3` / `Shift+F3` elsewhere), `Enter` / `Shift+Enter` in the find bar |
| Find in project | `Cmd+Shift+F` |
| **Select line** (repeat to extend) | `Cmd+L` |
| **Split selection into lines** (cursor per line) | `Cmd+Shift+L` |
| **Add next occurrence** | `Cmd+D` |
| Skip occurrence | menu or command palette |
| **Select all occurrences** | `Ctrl+Cmd+G` |
| Add cursor above / below | `Ctrl+Shift+↑/↓`, `Cmd+Alt+↑/↓` |
| **Column selection** | `Option+drag` |
| Add cursor | `Cmd+click` |
| Collapse to a single cursor | `Esc` |
| Command palette | `Cmd+K`, `Cmd+Shift+P` |
| Toggle sidebar | `Cmd+B` |
| Word wrap | `Alt+Z` |
| Text size bigger / smaller / default | `Cmd+=` / `Cmd+-` / `Cmd+0` |
| Reveal in Finder / copy path / copy relative path | `Cmd+Alt+R` / `Cmd+Alt+C` / `Cmd+Alt+Shift+C` |
| Rename / move to trash (in the sidebar) | `F2` / `Cmd+Backspace` |
| Toggle markdown preview / source | `Cmd+Shift+V` |
| Close tab / reopen closed tab | `Cmd+W` / `Cmd+Shift+T` |
| Switch to tab N | `Cmd+1`…`Cmd+9` |
| Previous / next tab | `Cmd+Shift+[` / `Cmd+Shift+]`, `Ctrl+Tab` |
| Go to line | `Ctrl+G` |
| Undo / redo | `Cmd+Z` / `Cmd+Shift+Z` |
| Duplicate line / move line | `Cmd+Shift+D` / `Ctrl+Cmd+↑/↓` |
| Indent / outdent | `Tab` / `Shift+Tab` (with selection), `Cmd+]` / `Cmd+[` |
| Delete line | `Ctrl+Shift+K` |

---

## 6. CLI

```
me                 # focus the app (launch if needed)
me .               # open the current folder in a window
me path/to/dir     # open a folder
me a.md b.rs:42:7  # open files, optionally at line[:column]
me -w FILE         # wait until FILE's tab is closed (git commit, crontab -e, ...)
me -n ...          # force a new window
me -               # read stdin into a new scratch tab
me --version
```

Set it as the shell's editor: `export EDITOR="me -w"`.

**Protocol (newline-delimited JSON over the socket).**

```jsonc
// CLI → app
{"type":"open","cwd":"/Users/me/src/x","paths":[{"path":"/abs/a.md","line":42,"column":7}],"wait":true,"new_window":false}
// app → CLI
{"type":"opened","ids":[17]}
{"type":"closed","id":17}            // -w: the CLI exits when all ids are closed
{"type":"error","message":"…"}
```

Socket path: `$XDG_RUNTIME_DIR/me-$UID/me.sock` if set, otherwise
`$TMPDIR/me-$UID/me.sock` (on macOS `$TMPDIR` is already per-user).
The directory is created with mode 0700.

## 7. Default editor registration

* **macOS.** `Info.plist` declares `CFBundleDocumentTypes` for
  `public.plain-text`, `public.source-code`, `public.script`,
  `net.daringfireball.markdown`, `public.json`, `public.xml`, `public.yaml`,
  `public.shell-script`, and similar, with rank `Alternate`, so we never take
  over silently. The **Me → Make Me the Default Editor…** command calls
  `NSWorkspace.setDefaultApplication(at:toOpenContentType:)` (macOS 12+) for
  each type. macOS shows its own confirmation. Opening files from Finder
  arrives through `application:openURLs:`, which GPUI exposes as `on_open_urls`.
* **CLI install.** **Me → Install 'me' Command** symlinks
  `/usr/local/bin/me` → `Me.app/Contents/Resources/bin/me`,
  falling back to `~/.local/bin`. `brew install --cask` will do the same.
* **Linux.** `packaging/linux/me.desktop` lists the text and markdown MIME
  types. `xdg-mime default me.desktop text/markdown text/plain`.
* **Windows.** Phase 4: "Open with" registry entries from the installer.

---

## 8. Phases

### Phase 0: Foundations (done)

* [x] Technology decision and plan (this document).
* [x] Cargo workspace with the six crates.
* [x] `me-text`: rope document, selection model, every multi-cursor
      command (Cmd+D, Cmd+L, Cmd+Shift+L, Ctrl+Cmd+G, add cursor above/below,
      column selection), movement, editing, undo/redo with typing coalescing,
      clipboard with per-cursor distribution, find and replace (regex with
      `$1` groups). 67 unit and behaviour tests.
* [x] `me-project`: ignore-aware directory listing, background file
      index with the nucleo fuzzy matcher, parallel streaming project search,
      atomic replace-in-files. 11 tests.
* [x] `me-ipc` + `me` CLI: single instance, `file:line:col`, `-w`,
      `-n`, `-`. Unit tests and end-to-end tests against a fake app.
* [x] `me-syntax`: 20 languages, built lazily, incremental parsing on a
      background thread, highlighting of the visible byte range only.
* [x] App: sidebar file tree, Sublime-style preview tabs, editor element
      (viewport-only shaping, multi-cursor, Option-drag column selection, IME,
      blinking cursor, current-line highlight, overlay scrollbar), markdown
      preview (live, relative links open in tabs), Cmd+T finder, Cmd+F find
      and replace bar, Cmd+Shift+F project search and replace, status bar,
      native menus, prompts for unsaved changes, light/dark following the system.
* [x] Logo (app icon and in-app mark), icon pipeline, `script/bundle-macos`
      with `Info.plist` document types, Linux `.desktop` file, CI on macOS and
      Linux.

**Verified here** (Linux, X11, software rendering): every crate's tests
pass, and the app was driven with real input. That covered opening folders,
expanding the tree, previewing markdown, Cmd+T, Cmd+D, Option-drag column
editing, find, project search, dark mode, and `me -w` launching the app
and returning when the tab closed.

**Not yet verified by hand:** the macOS build. CI builds and bundles it on
`macos-15`, but nobody has run the native look (vibrancy, traffic lights,
Finder "Open With", **Make Me the Default Editor…**) on a Mac yet. That is the first
task of Phase 1.

### Phase 1: Daily driver for reading (MVP)

Done:

* [x] Context menus (native NSMenus on macOS) in the editor, the tabs and
      the sidebar; new file/folder, rename and trash in the sidebar.
* [x] `Cmd+K` command palette; clickable status bar (syntax, indentation,
      go to line).
* [x] Tabs: drag to reorder, close others / to the right, double-click the
      empty tab bar for a new file.
* [x] Session restore: windows, folders, tabs, cursors, scroll positions
      and unsaved text. Closing a window or quitting never asks.
* [x] Word wrap (display rows; up/down move by row), `Cmd+=`/`Cmd+-` text
      size, `settings.json`, no scrolling past the last line.
* [x] Markdown: typography, local images, code blocks highlighted with the
      editor's grammars, Mermaid diagrams (merman, all diagram types).
* [x] Image and SVG previews; `.mdc` as markdown; `#!` language detection.

To do:

* Run on macOS and fix whatever the native shell needs: traffic-light
  spacing, vibrancy, menu shortcuts, Finder opens, **Install 'me'
  Command**, **Make Me the Default Editor…**.
* Measure cold start on an M-series Mac against the 100 ms budget
  (`ME_TRACE_STARTUP=1`) and trim anything on the startup path.
* Tabs: overflow scrolling.
* Find bar: match count ("3 of 12"), keep the find query per tab.
* Project search: per-match checkboxes before replacing, and a preview of
  each replacement.
* File watching (`notify`) instead of refresh-on-focus: refresh the tree,
  reload unmodified buffers, and re-render the markdown preview live while
  another program writes the file.
* Markdown: keyboard scrolling in the preview (Space, Page Up/Down, arrows,
  Home/End). Mouse and trackpad scrolling already work; a `ScrollHandle`
  offset set from an action didn't repaint the gpui-component `TextView`, so
  this needs a closer look. GitHub alerts (`> [!NOTE]`), heading anchors and
  in-page links, front matter as a table, footnotes, remote images.
* Editor: bracket matching, auto-closing pairs, and scrollbar dragging.

**Done when:** a week of daily use in place of Sublime with no blocker bugs,
and cold start ≤ 100 ms.

### Phase 2: Huge files and performance hardening

* Load in the background with a progress bar. Memory-map and chunk-build the
  rope (`Rope::from_reader` on an mmap). Files over 1 GB open read-only by
  default, with an "edit anyway" option.
* Long-line handling: shape only the visible column range. Cap the
  highlighter's work per line.
* Highlighting in the background with cancellation. Cutoffs by file size and
  by line length.
* Fast multi-cursor batches: rebuild the rope from pieces when there are more
  than about 10k changes.
* Benchmarks (criterion) for edit, search, and highlight, and a startup trace.
  CI fails on regressions over 10 %.
* Memory profiling: idle ≤ 60 MB, no leaks over 1,000 tab open/close cycles.

### Phase 2.5: Go to definition (Cmd+click)

Use the standard language servers (rust-analyzer, gopls,
typescript-language-server, pyright, clangd, …); analysing languages is
their whole job, and they are what VS Code and Zed run too. Two tiers:

1. **Built in, instant, nothing to install** (done). The tree-sitter
   grammars we ship have `tags` queries (plus our own for Bash, Markdown
   headings and Rust constants and impls); a background pass over the
   folder builds a symbol index (name → definitions), refreshed for changed
   files when the window regains focus and per file on save. Cmd+click and
   F12 jump to the definition, nearest first (same file, same folder),
   with a picker when several match; `Cmd+R` lists the file's symbols and
   `Cmd+Shift+R` the project's; the path bar shows the symbols around the
   cursor; Ctrl+- goes back. Shift+F12 searches the project for the word.
   Every language we highlight, a few MB of memory, not type-aware.
2. **LSP, when a server is installed** (done). The `me-lsp` crate speaks
   JSON-RPC over stdio (`lsp-types`), with a writer thread so sending a
   big buffer never blocks the UI. A server starts the first time a file
   of its language opens in a folder (found on the login shell's `PATH`,
   overridable in `settings.json`); open buffers sync 250 ms after an
   edit and right before a request, as the one range that changed (worked
   out on the writer thread by comparing the text the server has with the
   new text) for servers that take incremental changes; positions are
   UTF-8 when the server agrees, else UTF-16. Cmd+click/F12 ask
   `textDocument/definition` and Shift+F12 `textDocument/references`,
   falling back to tier 1 when there's no server, it's starting, or it
   takes over 4 s. The status bar shows `$/progress` and offers
   restart/stop.
3. **Hover, diagnostics and rename** (done). Problems the server pushes
   (`publishDiagnostics`) or answers when asked (`textDocument/diagnostic`
   after each sync and on `workspace/diagnostic/refresh`, retried with
   backoff when a busy server cancels; Ruby LSP only pulls, rust-analyzer
   pulls its own and pushes `cargo check`'s) are merged per file and shown
   as wavy underlines (and colored line numbers),
   kept in byte offsets that move with each edit until the server sends
   new ones; stale versions are dropped. The status bar counts errors and
   warnings in the folder (click, or Cmd+Shift+M, for the list); F8 /
   Shift+F8 step through them. Resting the pointer on a word (or Cmd+I)
   shows a popover with its problems and the server's `textDocument/hover`
   markdown, code highlighted with our grammars. F2 renames with
   `prepareRename` and `rename`: open files get one undoable edit (saved
   if they had no other changes, like VS Code), closed files are edited on
   disk and announced with `didChangeWatchedFiles`. Without a server, or
   when it won't rename something (Ruby LSP renames only constants), F2
   puts a cursor on each occurrence in the file. `file://` links in hovers
   open in the editor.

Language servers can use a lot of memory (rust-analyzer: gigabytes), so
they only run while a window has files of their language open and stop
when idle; the editor's own footprint doesn't change.

### Phase 3: Polish and configuration

* More of `settings.json` (font family, tab width, theme overrides, size
  cutoffs, language servers) and `keymap.json`, both hot-reloaded.
* Scroll sync between source and preview (pulldown-cmark source offsets).
* Minimal git awareness in the tree (modified/untracked dots). No git UI.
* Distribution: Developer ID signing, notarization, Homebrew cask.
  Auto-update is done: CI publishes each master build as the GitHub release
  `build-<n>`; the app checks `releases/latest` (with the GitHub CLI's
  token, the repository being private), downloads and verifies the zip, and
  swaps the bundle in on quit or restart.
* Accessibility pass (VoiceOver labels for the tree, tabs, and editor lines).

### Phase 4: Linux and Windows

* Linux: Wayland and X11 testing, `.desktop` file, AppImage and `.deb`,
  portal file dialogs, Freedesktop dark-mode signal.
* Windows: named-pipe IPC, MSI/MSIX installer with "Open with" entries,
  Mica backdrop for the sidebar.

---

## 9. Out of scope (for now)

AI features, a terminal, a debugger, git UI beyond tree markers, plugins or
extensions, collaboration, and WYSIWYG markdown editing.

## 10. Open questions

* **Cmd+L.** Here it means Sublime's "select line, repeat to extend".
  `Cmd+Shift+L` splits a selection into one cursor per line, and `Cmd+D` adds
  the next occurrence. If you want Cmd+L to do something else (for example
  VS Code's `Cmd+Shift+L`, select all occurrences), change it in
  `crates/app/src/actions.rs`.
* **Name.** The app is called **Me** ("my editor"), the CLI command is `me`,
  and the bundle id is `com.erkie.me`.
