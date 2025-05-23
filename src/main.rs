use process::{handle_sock_msg, spawn_foreign_process};
use serde_json::json;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use ui::start;

mod ll_aloc;
mod process;
mod shm;
mod sock;
mod ui;

use anyhow::Result;
use std::sync::{Arc, Mutex};

/*
TODO: The file start is not magically 8 byte aligned...

*/

#[tokio::main]
async fn main() -> Result<()> {
    // Tracing
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_ansi(true)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
    // Main:
    let vdoms = Arc::new(Mutex::new((None, Vec::new())));
    let (tx_refresh, rx_refresh) = tokio::sync::mpsc::channel(1);
    let (tx_quit, mut rx_quit) = tokio::sync::mpsc::channel::<()>(1);
    let (tx_broadcast, mut rx_broadcast) = tokio::sync::mpsc::channel::<String>(1);

    let vdoms_1 = vdoms.clone();
    let foreign_process_task = tokio::task::spawn(async move {
        let handle = spawn_foreign_process("python3 -u client.py").unwrap();
        let shm_guard = handle.shm_guard.clone();
        let sock_guard = handle.sock_guard.clone();
        let mut sock_guard_1 = sock_guard.clone();

        let shm_guard_1 = shm_guard.clone();
        let vdoms_1 = vdoms_1.clone();
        let vdoms_2 = vdoms_1.clone();
        tokio::task::spawn(async move {
            sock_guard
                .start(
                    move |msg| handle_sock_msg(&shm_guard_1, &vdoms_1, msg),
                    move || {
                        let tx_quit = tx_quit.clone();
                        async move { tx_quit.send(()).await.unwrap() }
                    },
                )
                .await;
        });

        loop {
            tokio::select! {
                data = rx_broadcast.recv() => {
                    if let Some(data) = data{
                        sock_guard_1.broadcast(&data).expect("Failed to broadcast -- unrecovrable.");
                    } else {/* rx channel closed; socket handled through tx_quit in sock_guard already. */}
                },
                buf = shm_guard.recv() => { /* sem_ready was triggered */
                    vdoms_2.lock().unwrap().1 = buf;
                    tx_refresh.send(()).await.expect("Failed to refresh screen -- channel failed.")
                }
            }
        }
    });

    tokio::task::spawn(async move {
        rx_quit.recv().await.unwrap(); /* waits until quit */
        foreign_process_task.abort();
    });

    let handler = move |id: usize| {
        let tx_broadcast = tx_broadcast.clone();
        tokio::task::spawn(async move {
            tx_broadcast
                .send(
                    serde_json::to_string(&json!({"kind": "event", "evt_id": id}))
                        .expect("Couldn't serialise message."),
                )
                .await
                .expect("Failed to broadcast over channel.");
        });
    };

    start(1600, 900, "z71200-runtime", vdoms, handler, rx_refresh);
    Ok(())
}
