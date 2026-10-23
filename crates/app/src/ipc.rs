//! Bridges the CLI socket (blocking threads) to the GPUI main thread.

#[cfg(unix)]
pub use unix::*;

#[cfg(unix)]
mod unix {
    use editor_ipc::{Request, Server};
    use futures::StreamExt;
    use futures::channel::mpsc;
    use gpui::App;

    use crate::app_state::{self, Reply};

    /// Serves CLI requests for the life of the app.
    pub fn start(server: Server, cx: &mut App) {
        let (tx, mut rx) = mpsc::unbounded::<(Request, Reply)>();
        std::thread::Builder::new()
            .name("ipc-accept".into())
            .spawn(move || {
                loop {
                    let Ok(mut conn) = server.accept() else { break };
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        // Liveness probes connect and hang up without a request.
                        let Ok(Some(request)) = conn.recv() else {
                            return;
                        };
                        let (reply_tx, mut reply_rx) = mpsc::unbounded();
                        if tx.unbounded_send((request, reply_tx)).is_err() {
                            return;
                        }
                        // Forward responses until the app drops every sender
                        // (all waited-on tabs closed) or the CLI goes away.
                        while let Some(response) = futures::executor::block_on(reply_rx.next()) {
                            if conn.send(&response).is_err() {
                                break;
                            }
                        }
                    });
                }
            })
            .expect("spawn ipc thread");

        cx.spawn(async move |cx| {
            while let Some((request, reply)) = rx.next().await {
                cx.update(|cx| app_state::handle_request(request, Some(reply), cx));
            }
        })
        .detach();
    }
}
