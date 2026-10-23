//! End-to-end: the real `editor` binary against a fake app.
#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

use editor_ipc::{Request, Response, Server};

#[test]
fn wait_mode_blocks_until_closed() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("editor.sock");
    let server = Server::bind(&socket).unwrap();
    std::fs::write(dir.path().join("COMMIT_EDITMSG"), "").unwrap();

    let app = std::thread::spawn(move || {
        let mut conn = server.accept().unwrap();
        let request = conn.recv().unwrap().unwrap();
        let Request::Open { paths, wait, .. } = request else {
            panic!("expected open")
        };
        assert!(wait);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].line, Some(3));
        assert!(paths[0].path.ends_with("COMMIT_EDITMSG"));
        conn.send(&Response::Opened { ids: vec![42] }).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        conn.send(&Response::Closed { id: 42 }).unwrap();
    });

    let start = Instant::now();
    let status = Command::new(env!("CARGO_BIN_EXE_editor"))
        .args(["-w", "COMMIT_EDITMSG:3"])
        .current_dir(dir.path())
        .env("EDITOR_SOCKET", &socket)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        start.elapsed() >= Duration::from_millis(300),
        "returned before the file was closed"
    );
    app.join().unwrap();
}

#[test]
fn without_wait_returns_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("editor.sock");
    let server = Server::bind(&socket).unwrap();
    let app = std::thread::spawn(move || {
        let mut conn = server.accept().unwrap();
        conn.recv().unwrap().unwrap();
        conn.send(&Response::Opened { ids: vec![1] }).unwrap();
        // Keep the connection open; the CLI must not wait for it.
        std::thread::sleep(Duration::from_millis(500));
    });
    let start = Instant::now();
    let status = Command::new(env!("CARGO_BIN_EXE_editor"))
        .arg(".")
        .env("EDITOR_SOCKET", &socket)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(start.elapsed() < Duration::from_millis(400));
    app.join().unwrap();
}

#[test]
fn app_quitting_while_waiting_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("editor.sock");
    let server = Server::bind(&socket).unwrap();
    let app = std::thread::spawn(move || {
        let mut conn = server.accept().unwrap();
        conn.recv().unwrap().unwrap();
        conn.send(&Response::Opened { ids: vec![1] }).unwrap();
    });
    let output = Command::new(env!("CARGO_BIN_EXE_editor"))
        .args(["-w", "x.txt"])
        .env("EDITOR_SOCKET", &socket)
        .output()
        .unwrap();
    app.join().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("quit before"));
}

#[test]
fn launches_the_app_when_not_running() {
    // A fake "app" that only records that it was launched. It never opens
    // the socket, so the CLI must time out with a clear message.
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("launched");
    let script = dir.path().join("fake-app");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_editor"))
        .arg("a.txt")
        .env("EDITOR_SOCKET", dir.path().join("nobody.sock"))
        .env("EDITOR_APP", &script)
        .env("EDITOR_CONNECT_TIMEOUT_MS", "300")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(marker.exists(), "the CLI should have launched the app");
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not reach the editor"));
}
