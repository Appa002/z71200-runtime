use anyhow::Result;
use serde::de::DeserializeOwned;
use std::{fs, path::Path, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    sync::{Mutex, broadcast},
};
use tracing::{error, trace, warn};

#[derive(Debug, Clone)]
pub struct SockHandle {
    #[allow(dead_code)]
    pub name: String,
    pub listener: Arc<UnixListener>,
    tx: broadcast::Sender<String>,
}
impl SockHandle {
    pub fn new(socket_path: &str) -> Result<Self> {
        if Path::new(&socket_path).exists() {
            fs::remove_file(&socket_path).unwrap();
        }
        // 1. create a *blocking* std listener
        // 2. flip it to non-blocking and lift it into Tokio
        let std_listener = std::os::unix::net::UnixListener::bind(socket_path)?;
        std_listener.set_nonblocking(true).unwrap();
        let listener = UnixListener::from_std(std_listener)?;

        let (tx, _rx) = broadcast::channel(100);

        Ok(SockHandle {
            name: socket_path.to_owned(),
            listener: Arc::new(listener),
            tx,
        })
    }

    pub fn start<F, A, I, J>(&self, cb_sock: F, cb_quit: A) -> impl std::future::Future<Output = ()>
    where
        F: Fn(I) -> Option<String> + Clone + Send + Sync + 'static,
        A: Fn() -> J + Clone + Send + Sync + 'static,
        I: DeserializeOwned,
        J: std::future::Future<Output = ()> + Send + Sync,
    {
        let cb_sock = Arc::new(cb_sock.clone());
        let cb_quit = Arc::new(cb_quit.clone());
        let tx = self.tx.clone();
        async move {
            loop {
                let (stream_raw, _addr) = self.listener.accept().await.unwrap();
                let cb_sock = cb_sock.clone();
                let cb_quit = cb_quit.clone();
                let mut rx = tx.subscribe();
                // Mutex is used to make sure that ask protocol is implemented correctly.
                // Specifically that if an kind=='ask' message is recieved nothing is pushed
                // via the socket until the answer has been send.

                let stream = Mutex::new(stream_raw);

                tokio::spawn(async move {
                    loop {
                        let mut size_buffer = [0; 4];
                        tokio::select! {
                            Ok(data) = rx.recv() => {
                                let bytes = data.as_bytes(); /* this is utf-8 */
                                let size = bytes.len() as u32;
                                let mut buf = Vec::with_capacity(4 + bytes.len());
                                buf.extend_from_slice(&size.to_le_bytes());
                                buf.extend_from_slice(bytes);
                                let _ = stream.lock().await.write_all(&buf).await;

                            },
                            (mut stream_guard, maybe_error) = async {
                                let mut stream_guard =  stream.lock().await;
                                let maybe_error = stream_guard.read_exact(&mut size_buffer).await;
                                (stream_guard, maybe_error) /* this allows rx.recv to go ahead if either the lock or the read_exa */
                            } => {
                            // Note: This branch in the tokio::select! is entred with the lock held and the size data read, so that we guarantee:
                            //       1) This code is the first read_exact to be executed after the initial one, so that we always get the data the size was sent for.
                            //       2) We lock the stream_guard all the way until any potential `cb_sock` callbacks are evaluated. This way, when an `ask` kind
                            //          is recieved, we can guarantee that the next message on the socket is the response; as the protocol demands.

                            if let Err(err) = maybe_error {
                                warn!(
                                    "Error when trying to read_exact on unix socket -- the process probably hungup. {:?}",
                                    err
                                );
                                if err.kind() == std::io::ErrorKind::UnexpectedEof {
                                    /* this means the error is because the process hungup; we consider it dead. */
                                    cb_quit().await;
                                }
                                return;
                            }

                            let message_size = u32::from_le_bytes(size_buffer);

                            // Read the JSON payload based on the size
                            let mut buffer = vec![0; message_size as usize];
                            stream_guard.read_exact(&mut buffer).await.unwrap();



                            match String::from_utf8(buffer) {
                                Ok(json_str) => {
                                    trace!(
                                        "Received message size: {}, JSON: {}",
                                        message_size, json_str
                                    );
                                    let maybe_response =
                                        cb_sock(serde_json::from_str(&json_str).unwrap());
                                    if let Some(response) = maybe_response {
                                        // Prepare response
                                        let response_bytes = response.as_bytes(); /* this is utf-8 */
                                        let response_size = response_bytes.len() as u32;

                                        // Construct out (size + message)
                                        let mut buf =
                                            Vec::with_capacity(4usize + response_size as usize);
                                        buf.extend_from_slice(&response_size.to_le_bytes());
                                        buf.extend_from_slice(response_bytes);
                                        stream_guard.write_all(&buf).await.unwrap();
                                    }
                                }
                                Err(err) => {
                                    error!("Error parsing message as UTF-8: {}", err);
                                }
                            }
                        }
                        }
                    }
                });
            }
        }
    }

    #[allow(dead_code)]
    pub fn broadcast(&mut self, data: &String) -> Result<()> {
        self.tx.send(data.clone())?;
        Ok(())
    }
}

// impl Drop for SockGuard {
//     fn drop(&mut self) {
//         // Close the connection handles first
//         drop(&mut self.listener);

//         // Then remove the socket file
//         if Path::new(&self.name).exists() {
//             match fs::remove_file(&self.name) {
//                 Ok(_) => println!("Socket file '{}' removed successfully", self.name),
//                 Err(e) => eprintln!("Failed to remove socket file '{}': {}", self.name, e),
//             }
//         }
//     }
// }
