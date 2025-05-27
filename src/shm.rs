use anyhow::{Result, anyhow};
use core::panic;
use libc::{
    EAGAIN, O_CREAT, O_RDWR, S_IRUSR, S_IWUSR, c_long, ftruncate, sem_open, sem_post, sem_trywait,
    sem_unlink, sem_wait, shm_open, shm_unlink,
};
use memmap2::{MmapMut, MmapOptions};
use std::{
    ffi::CString,
    fs::File,
    os::fd::FromRawFd,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tokio::{io, task};

use crate::{ll_aloc, process::PROTOCOL_VERSION};
pub const VERSION_OFF: usize = 0;
pub const DATA_OFF: usize = VERSION_OFF + size_of::<usize>();
pub const LEN: usize = 1_024 * 32 /*32 kb*/;

/// Create-or-open a POSIX shared-memory object and return the file descriptor
fn open_shm(c_name: &CString, len: usize) -> std::io::Result<File> {
    let fd = unsafe {
        shm_open(
            c_name.as_ptr(),
            O_RDWR | O_CREAT,
            (S_IRUSR | S_IWUSR) as c_long,
        )
    };
    if fd == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // Resize to the desired length
    let res = unsafe { ftruncate(fd, len as _) };
    if res == -1 {
        unsafe { libc::close(fd) };
        return Err(std::io::Error::last_os_error());
    }
    // Turn the raw fd into a File that closes automatically
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn map_shared(file: &File, len: usize) -> std::io::Result<MmapMut> {
    let mut opts = MmapOptions::new();
    opts.len(len);

    unsafe { opts.map_mut(file) }
}

unsafe fn init_data(mm: &mut MmapMut) {
    unsafe {
        let version_ptr = mm.as_mut_ptr().add(VERSION_OFF) as *mut usize;
        let data_ptr = mm.as_mut_ptr().add(DATA_OFF) as *mut u8;

        assert_eq!(version_ptr as usize % size_of::<usize>(), 0);
        assert_eq!(data_ptr as usize % size_of::<usize>(), 0);

        *version_ptr = PROTOCOL_VERSION.to_le();

        // init default linked list alocator
        ll_aloc::init(data_ptr).unwrap();
    }
}

unsafe fn open_sem(c_name: &CString, initial: usize) -> std::io::Result<*mut i32> {
    let sem = unsafe { sem_open(c_name.as_ptr(), O_CREAT, (S_IRUSR | S_IWUSR) as c_long, 0) };

    if sem == libc::SEM_FAILED {
        panic!(
            "Failed to create semaphore: {}",
            std::io::Error::last_os_error()
        )
    }

    // This sets the semaphore to initial many; there are some platform differences on
    // how to open a semaphore with O_CREAT such that the initial value is alwys set. So we
    // are doing this instead. WARNING: This code assumes that no other process/thread is
    // accessing this sem between the two loop (there is an actual race condition), however
    // since this is the creation code it is fine for our purposes.

    // TODO: Error handling (ie. if trywait error is not EAGAIN we hit something else)
    while unsafe { sem_trywait(sem) } != -1 {} // drains it to zero
    for _ in 0..initial {
        unsafe { sem_post(sem) };
    } // adds initial many

    Ok(sem)
}

pub struct SemMuextGuard<'a, T> {
    sem: UnsafeSendSyncRawSem,
    pub data: MutexGuard<'a, T>,
}

impl<'a, T> Drop for SemMuextGuard<'a, T> {
    fn drop(&mut self) {
        unsafe { sem_post(self.sem.0) };
    }
}

#[derive(Debug)]
pub struct SemMutex<T> {
    sem: UnsafeSendSyncRawSem,
    data: Mutex<T>,
}
impl<T> SemMutex<T> {
    fn new(sem: *mut i32, data: T) -> Self {
        Self {
            sem: UnsafeSendSyncRawSem(sem),
            data: Mutex::new(data),
        }
    }

    pub fn lock(&self) -> Result<SemMuextGuard<'_, T>> {
        let r = unsafe { sem_wait(self.sem.0) };
        if r == 0 {
            Ok(SemMuextGuard {
                sem: self.sem,
                data: self.data.lock().unwrap(),
            })
        } else {
            Err(anyhow!(
                "Error locking semaphore. {:#}",
                std::io::Error::last_os_error()
            ))
        }
    }

    #[allow(dead_code)]
    pub fn try_lock(&self) -> Result<Option<SemMuextGuard<'_, T>>> {
        let r = unsafe { sem_trywait(self.sem.0) };
        if r == 0 {
            return // we got the lock
            Ok(Some(SemMuextGuard {
                sem: self.sem,
                data:self.data.lock().unwrap(),
            }));
        }

        if let Some(errno) = std::io::Error::last_os_error().raw_os_error() {
            if errno == EAGAIN {
                // currently locked
                return Ok(None);
            } else {
                return Err(anyhow!(
                    "Error locking semaphore. {:#}",
                    io::Error::from_raw_os_error(errno)
                ));
            }
        } else {
            unreachable!(
                "r should have been 0 if no OS error is reported -- something badly went wrong in the kernel."
            )
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
struct UnsafeSendSyncRawSem(*mut i32);
unsafe impl Sync for UnsafeSendSyncRawSem {}
unsafe impl Send for UnsafeSendSyncRawSem {}
/* POSIX guarantees that semaphores are thread-safe when accessed with the same handle from any number of threads */
/* We could make the SemMutex itself Send + Sync to get around the stored *mut i32 not being Send + Sync HOWEVER this disregards the
semantics of the `T` type. A better implementation is this here, which wraps the *mut i32 so that the compiler can proof Send + Sync-ness
from the T type. natrually we have to guarantee that accessing data referenced by this pointer is thread safe (thanks POSIX).
*/
impl UnsafeSendSyncRawSem {
    unsafe fn try_wait(&self) -> i32 {
        unsafe { sem_trywait(self.0) }
    }
}

#[derive(Debug, Clone)]
pub struct SHMHandle {
    sem_ready: UnsafeSendSyncRawSem,      /* sem_ready */
    pub shm_file: Arc<SemMutex<MmapMut>>, /* sem_lock */
}

impl SHMHandle {
    pub fn new(toplevel_name: &str) -> Self {
        let shm_name = CString::new(format!("{toplevel_name}")).unwrap();
        let sem_ready_name = CString::new(format!("{toplevel_name}_sem_ready")).unwrap();
        let sem_lock_name = CString::new(format!("{toplevel_name}_sem_lock")).unwrap();

        // Delete previous file and sem if it exists
        // these fail if the file doesn't exist, but that's okay we just move on silently.
        unsafe {
            shm_unlink(shm_name.as_ptr());
            sem_unlink(sem_lock_name.as_ptr());
            sem_unlink(sem_ready_name.as_ptr());
        };

        // Setup Shared Data
        let sem_ready = unsafe { open_sem(&sem_ready_name, 0).unwrap() };
        let sem_lock = unsafe { open_sem(&sem_lock_name, 1).unwrap() };
        let file = open_shm(&shm_name, LEN).unwrap();
        let mut mmaped = map_shared(&file, LEN).unwrap();
        unsafe {
            init_data(&mut mmaped);
        } // Setup default linked list alocator

        Self {
            sem_ready: UnsafeSendSyncRawSem(sem_ready),
            shm_file: Arc::new(SemMutex::new(sem_lock, mmaped)),
        }
    }

    pub fn recv(&self) -> impl std::future::Future<Output = Arc<SemMutex<MmapMut>>> {
        let sem_ready = self.sem_ready.clone();
        let shm_file = self.shm_file.clone();

        async move {
            // Things used in the loop
            const BASE_BACKOFF_US: u64 = 50;
            let mut backoff = Duration::from_micros(BASE_BACKOFF_US);

            loop {
                // wait for a signal from the other process that a new tree is available. (non-blockin)
                match unsafe { sem_ready.try_wait() } {
                    /* we can't use a mutex for this because we DONT want to increment the sem value once we are done (only the client process signals for new data) */
                    0 => {
                        /* new data signal, return the mutex to the now valid file */
                        return shm_file.clone();
                    }
                    _ => {
                        if let Some(errno) = std::io::Error::last_os_error().raw_os_error() {
                            if errno == EAGAIN {
                                task::yield_now().await;
                                // exponential back-off, max 5 ms
                                tokio::time::sleep(backoff).await;
                                backoff = (backoff * 2).min(Duration::from_millis(5));
                                continue;
                            } else {
                                panic!(
                                    "sem_trywait failed with error: {}",
                                    io::Error::from_raw_os_error(errno).to_string()
                                );
                            }
                        }
                        unreachable!("sem_ready didn't return 0 but also no error was indicated.")
                    }
                }
            }
        }
    }
}
impl Drop for SHMHandle {
    fn drop(&mut self) {
        /* figure out how to unlink the fles, this is tricky because the infinite loop takes self by reference so you have to respond to the external abort on the returned future. */
        // shm_unlink(self.shm_name.as_ptr());
        // sem_unlink(self.sem_ready_name.as_ptr());
        // sem_unlink(self.sem_read_name.as_ptr());
        // this both may fail if they are unlinked already, but that's fine we just continue silently
    }
}
