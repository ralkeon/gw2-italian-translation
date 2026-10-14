//! Named pipe server for the shim: requests in, translations out.
//!
//! One thread waits for the shim, reads its requests and handles them. Replies are written
//! straight to the reply pipe by whoever has them: the pipe thread for saved translations, the
//! translator thread for new ones.

use std::{
    ffi::c_void,
    fs::File,
    io::{self, BufReader, Write},
    iter,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use gw2it_proto::{self as proto, AddonMessage};
use windows_sys::Win32::{
    Foundation::{ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND},
    System::{
        IO::CancelSynchronousIo,
        Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT},
        Threading::GetCurrentProcessId,
    },
};

use crate::{
    install,
    state::SHARED,
    store::{self, Seen},
    translate::{self, Job},
};

static STOP: AtomicBool = AtomicBool::new(false);
static THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static REPLIES: Mutex<Option<File>> = Mutex::new(None);

pub fn start() {
    STOP.store(false, Ordering::SeqCst);
    let handle = thread::Builder::new()
        .name("gw2it-pipe".into())
        .spawn(serve)
        .expect("spawn pipe thread");
    *THREAD.lock().unwrap() = Some(handle);
}

pub fn stop() {
    STOP.store(true, Ordering::SeqCst);
    let Some(handle) = THREAD.lock().ok().and_then(|mut t| t.take()) else {
        return;
    };
    // The thread blocks in ConnectNamedPipe or ReadFile: keep cancelling until it notices STOP
    // (a single cancel can land just before it enters the next blocking call).
    let deadline = Instant::now() + Duration::from_secs(3);
    while !handle.is_finished() && Instant::now() < deadline {
        unsafe { CancelSynchronousIo(handle.as_raw_handle() as HANDLE) };
        thread::sleep(Duration::from_millis(20));
    }
    if handle.is_finished() {
        let _ = handle.join();
    } else {
        log::error!("pipe thread did not stop");
    }
    *REPLIES.lock().unwrap() = None;
}

/// Sends a translation to the game. False if the shim is not connected or the pipe broke.
pub fn send_text(id: u32, hash: u64, text: &str) -> bool {
    match proto::encode_text(id, hash, text) {
        Ok(payload) => send(&payload),
        Err(e) => {
            log::warn!("text {id} not sent: {e}");
            false
        }
    }
}

fn send(payload: &[u8]) -> bool {
    let mut buf = Vec::with_capacity(4 + payload.len());
    if proto::write_frame(&mut buf, payload).is_err() {
        return false;
    }
    let Ok(mut replies) = REPLIES.lock() else {
        return false;
    };
    let Some(pipe) = replies.as_mut() else {
        return false;
    };
    if pipe.write_all(&buf).is_err() {
        *replies = None;
        return false;
    }
    true
}

fn serve() {
    let pid = unsafe { GetCurrentProcessId() };
    let (req_name, rep_name) = (proto::request_pipe(pid), proto::reply_pipe(pid));
    while !STOP.load(Ordering::SeqCst) {
        match session(&req_name, &rep_name) {
            Ok(()) => log::info!("shim disconnected"),
            Err(e) if STOP.load(Ordering::SeqCst) => {
                let _ = e;
                break;
            }
            Err(e) => {
                log::warn!("pipe: {e}");
                thread::sleep(Duration::from_secs(1));
            }
        }
        SHARED.shim_connected.store(false, Ordering::Relaxed);
        *REPLIES.lock().unwrap() = None;
    }
}

/// One shim connection, until it goes away.
fn session(req_name: &str, rep_name: &str) -> io::Result<()> {
    let requests = create_pipe(req_name, PIPE_ACCESS_INBOUND)?;
    let replies = create_pipe(rep_name, PIPE_ACCESS_OUTBOUND)?;
    // The shim opens the request pipe first, then the reply pipe.
    connect_pipe(&requests)?;
    connect_pipe(&replies)?;
    if STOP.load(Ordering::SeqCst) {
        return Ok(());
    }

    *REPLIES.lock().unwrap() = Some(File::from(replies));
    SHARED.shim_connected.store(true, Ordering::Relaxed);
    log::info!("shim connected");
    install::module_works();
    let repaired = store::take_repaired();
    for (id, hash, it) in &repaired {
        send_text(*id, *hash, it);
    }
    if !repaired.is_empty() {
        log::info!("{} repaired translations sent to the game", repaired.len());
    }

    let mut input = BufReader::new(File::from(requests));
    loop {
        let frame = match proto::read_frame(&mut input) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe || e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        match proto::decode_addon(&frame) {
            Ok(AddonMessage::Text { id, hash, text }) => on_text(id, hash, text),
            Ok(AddonMessage::Unknown(kind)) => log::debug!("skipping message kind {kind}"),
            Err(e) => log::warn!("skipping malformed message: {e}"),
        }
    }
}

fn on_text(id: u32, hash: u64, text: String) {
    SHARED.received.fetch_add(1, Ordering::Relaxed);
    match store::lookup(id, hash) {
        Some(it) => {
            send_text(id, hash, &it);
        }
        None => translate::request(Job { id, hash, en: text.clone(), force: false }),
    }
    store::record(Seen { id, hash, text, at_ms: store::now_ms() });
}

fn create_pipe(name: &str, access: u32) -> io::Result<OwnedHandle> {
    let wide: Vec<u16> = name.encode_utf16().chain(iter::once(0)).collect();
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            access | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            1 << 16,
            1 << 16,
            0,
            ptr::null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle as *mut c_void) })
}

fn connect_pipe(pipe: &OwnedHandle) -> io::Result<()> {
    let ok = unsafe { ConnectNamedPipe(pipe.as_raw_handle() as HANDLE, ptr::null_mut()) };
    if ok == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
