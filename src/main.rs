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
    ) Race condition? with set_tree_root and just having written
    ) Ui scrolling
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

// mod actor;
// mod from_memory;
// mod ll_aloc;
// mod other;
// mod shm;
// mod sock;
// mod to_gpui;
// mod ui;

// use actix::{Actor, Addr};
// use actor::{LaunchState, ProcessActor};
// use anyhow::Result;
// use other::{UserCode, build_folder_tree, cli_prompt, print_folder_tree};
// use std::{
//     collections::HashMap,
//     sync::{Arc, Mutex},
// }; can you read this?
// use tokio::sync::mpsc::{self, Sender};
// use tracing::Level;
// use tracing_subscriber::FmtSubscriber;

// async fn create_process_actor(
//     tx: Sender<()>,
//     vdoms: &Arc<Mutex<HashMap<String, (Option<usize>, Arc<Vec<u8>>)>>>,
//     actors: &Arc<Mutex<HashMap<String, Addr<ProcessActor>>>>,
//     user_code: &UserCode,
//     uid_counter: &mut u16,
// ) -> Addr<ProcessActor> {
//     let uid = format!("{:04x}", *uid_counter);
//     *uid_counter += 1;

//     let out = ProcessActor {
//         tx,
//         vdoms: vdoms.clone(),
//         actors: actors.clone(),
//         pstate: LaunchState::Stopped {
//             uid: uid.to_string(),
//         },
//         active_tree_loc: Arc::new(Mutex::new(None)),
//         user_code: user_code.clone(),
//     }
//     .start();
//     actors
//         .lock()
//         .unwrap()
//         .entry(uid.to_string())
//         .insert_entry(out.clone());
//     out
// }

// #[tokio::main]
// async fn main() -> Result<()> {
//     let subscriber = FmtSubscriber::builder()
//         .with_max_level(Level::DEBUG)
//         .with_thread_ids(true)
//         .with_thread_names(true)
//         .with_ansi(true)
//         .finish();
//     tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

//     // Begin by analysing the root folder and gathering the static user code
//     let root_path = cli_prompt();
//     let user_tree = Arc::new(Mutex::new(build_folder_tree(&root_path).unwrap()));
//     print_folder_tree(&user_tree.lock().unwrap(), 0, 0);

//     // Structure used to keep track of uid->vdom and channel for notifying screen refresh
//     let tree_store = Arc::new(Mutex::new(HashMap::new()));
//     let actor_store: Arc<Mutex<HashMap<String, Addr<ProcessActor>>>> =
//         Arc::new(Mutex::new(HashMap::new()));
//     let mut uid_counter = 0u16;

//     let (tx, rx) = mpsc::channel::<()>(1);

//     // Actix-Launches to handle actor infrastructure
//     let tree_store_1 = tree_store.clone();
//     let actor_store_1 = actor_store.clone();
//     std::thread::spawn(move || {
//         let system = actix::System::new();

//         system.block_on(create_process_actor(
//             tx,
//             &tree_store_1,
//             &actor_store_1,
//             &UserCode::from_id(user_tree, 0),
//             &mut uid_counter,
//         ));

//         system.run().unwrap(); /* blocking */
//     });

//     // Everything gpui related must happen on the main thread.
//     ui::start(800, 450, "Neosqueak", "0000", tree_store, &actor_store, rx).await;
//     Ok(())
// }
