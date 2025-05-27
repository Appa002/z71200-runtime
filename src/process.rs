use anyhow::Result;
use anyhow::anyhow;
use libc::getppid;
use memmap2::MmapMut;
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;
use std::{io::BufRead, process::Stdio};
use tracing::{Level, error, info, span};

use crate::ll_aloc;
use crate::shm::DATA_OFF;
use crate::shm::LEN;
use crate::shm::SemMutex;
use crate::{shm::SHMHandle, sock::SockHandle};

pub const PROTOCOL_VERSION: usize = 1;

#[derive(Debug)]
pub struct ProcessHandle {
    pub child: std::process::Child,
    pub shm_guard: SHMHandle,
    pub sock_guard: SockHandle,
}
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(None) /* still running */ => {
                if let Err(e) = self.child.kill() {
                    error!("failed to SIGKILL child: {e}");
                }
                // Reap it so we don't leave a zombie.
                let _ = self.child.wait();
            }
            Ok(Some(_status)) => {} /* already gone */
            Err(e) => error!("error interrogating child: {e}"),
        }
    }
}

pub fn spawn_foreign_process(run: &Vec<String>) -> Result<ProcessHandle> {
    let pid: i32 = unsafe { getppid() };

    // Create the socket and mmaped file
    let socket_path = format!("/tmp/z71200_sock_{}", pid);
    let shm_path = format!("/z71200_shm_{}", pid);
    let sock_guard = SockHandle::new(&socket_path)?;
    let shm_guard = SHMHandle::new(&shm_path);

    // Spawn the programme
    let mut cmd = std::process::Command::new(
        run.get(0)
            .ok_or(anyhow!("Must specify a programme to launch with `--`"))?,
    );
    if run.len() > 1 {
        cmd.args(&run[1..]);
    }

    let mut child = cmd
        .env("z71200_PROTOCOL_VERSION", format!("{}", PROTOCOL_VERSION))
        .env("z71200_SHM", &shm_path)
        .env("z71200_SEM_READY", format!("{}_sem_ready", &shm_path))
        .env("z71200_SEM_LOCK", format!("{}_sem_lock", &shm_path))
        .env("z71200_SOCK", &socket_path)
        .stdout(Stdio::piped()) // Capture stdout
        .stderr(Stdio::piped())
        .spawn()?;

    let span = span!(Level::INFO, "(foreign process)",);
    let _guard = span.enter();

    // Three threads, one for checking if the programme has exited, one for stdout and one for stderr
    let stdout = child
        .stdout
        .take()
        .ok_or(anyhow!("Failed to take stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or(anyhow!("Failed to take stderr"))?;

    // We are going to spawn a thread which just prints out stdout and stderr from this process
    let std_span = span!(parent: &span, Level::INFO, "stdout");
    std::thread::spawn(move || {
        let _guard = std_span.enter();
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(line) = line {
                info!("{}", line);
            }
        }
    });

    let err_span = span!(parent: &span, Level::WARN, "stderr");
    std::thread::spawn(move || {
        let _guard = err_span.enter();
        let reader = std::io::BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(line) = line {
                error!("{}", line);
            }
        }
    });

    Ok(ProcessHandle {
        child,
        shm_guard,
        sock_guard,
    })
}

fn handle_sock_msg_falliable(
    shm_handle: &SHMHandle,
    vdoms: &Arc<Mutex<(Option<usize>, Option<Arc<SemMutex<MmapMut>>>)>>,
    msg: serde_json::Map<String, serde_json::Value>,
) -> Result<Option<String>> {
    /* {kind: 'ask', fn: 'foo', args: {...}} */
    let kind = msg
        .get("kind")
        .and_then(|x| x.as_str())
        .ok_or(anyhow!("Expect payload to have stringy key 'kind'"))?;

    match kind {
        "ask" => {
            let fn_name = msg.get("fn").and_then(|x| x.as_str()).ok_or(anyhow!("Expected message of kind 'ask' to have stringy key 'fn' and map key 'args'. Missing 'fn'."))?;
            let args = msg.get("args").and_then(|x| x.as_object()).ok_or(anyhow!("Expected message of kind 'ask' to have stringy key 'fn' and map key 'args'. Missing 'args'."))?;
            match fn_name {
                "aloc" => {
                    let n = args.get("n").and_then(|x| x.as_u64()).ok_or(anyhow!("Function 'aloc' expects one parameter 'n : int' -- the number of bytes to alocate"))?;

                    let mtx = shm_handle.shm_file.clone();
                    let mut file = mtx.lock()?;

                    let file_start = unsafe { file.data.as_mut_ptr().add(DATA_OFF) };
                    let file_end = unsafe { file.data.as_ptr().add(LEN) };
                    let out_ptr = unsafe { ll_aloc::aloc(n as usize, file_start, file_end) }?;

                    Ok(Some(serde_json::to_string(
                        &json!({"kind": "return", "return": out_ptr }),
                    )?))
                }
                "dealoc" => {
                    let ptr = args.get("ptr").and_then(|x| x.as_u64()).ok_or(anyhow!("Function 'dealoc' expects one parameter 'ptr : int' -- offset where to free memory"))?;

                    let mtx = shm_handle.shm_file.clone();
                    let mut file = mtx.lock()?;

                    let file_start = unsafe { file.data.as_mut_ptr().add(DATA_OFF) };
                    let file_end = unsafe { file.data.as_ptr().add(LEN) };
                    unsafe { ll_aloc::dealoc(ptr as usize, file_start, file_end) }?;

                    Ok(Some(serde_json::to_string(
                        &json!({"kind": "return", "return": null }),
                    )?))
                }
                "set_root" => {
                    let ptr = args.get("ptr").and_then(|x| x.as_u64()).ok_or(anyhow!("Function 'set_root' expects one parameter 'ptr : int' -- offset where the layout begins"))?;
                    let mut lock = vdoms.lock().unwrap();
                    lock.0 = Some(ptr as usize);
                    Ok(Some(serde_json::to_string(
                        &json!({"kind": "return", "return": null }),
                    )?))
                }
                _ => {
                    return Err(anyhow!(
                        "Unknown 'fn' in message with kind 'ask', found {}",
                        fn_name
                    ));
                }
            }
        }
        _ => Err(anyhow!("Unknown kind '{}', support one of: ['ask']", kind)),
    }
}

pub fn handle_sock_msg(
    shm_handle: &SHMHandle,
    vdoms: &Arc<Mutex<(Option<usize>, Option<Arc<SemMutex<MmapMut>>>)>>,
    msg: serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    match handle_sock_msg_falliable(shm_handle, vdoms, msg) {
        Ok(o) => o,
        Err(err) => serde_json::to_string(&json!({"kind": "error", "error": err.to_string()})).ok(), /* TODO: log warning here if serealisation fails */
    }
}
