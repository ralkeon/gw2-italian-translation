//! Text Translator module that does no translating itself.
//!
//! Text Translator starts this exe (module.toml) from inside the game process. It writes the
//! handshake at once (the game waits for it), then relays frames unchanged:
//! stdin -> request pipe -> GW2 Italian Translation addon -> reply pipe -> stdout.
//!
//! The addon may be missing or reloading: requests that arrive meanwhile are queued and sent once
//! it is back, so reloading the DLL never needs a module restart.

#![windows_subsystem = "windows"]

use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{self, BufReader, Write},
    process,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::Duration,
};

use gw2it_proto as proto;

/// Requests kept while the addon is not connected; older ones are dropped beyond this.
const MAX_PENDING: usize = 200_000;
const RETRY: Duration = Duration::from_secs(1);

struct Args {
    cache_key: String,
    result_version: u32,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { cache_key: "it".into(), result_version: 1 };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--cache-key" => args.cache_key = value()?,
            "--result-version" => {
                args.result_version = value()?.parse().map_err(|_| "--result-version must be a number")?;
            }
            _ => return Err(format!("unknown argument {arg}")),
        }
    }
    Ok(args)
}

/// stderr lines go to the Nexus log; the prefix sets the level.
fn log(level: char, msg: &str) {
    let mut err = io::stderr().lock();
    let _ = writeln!(err, "{level}: [gw2it shim] {}", msg.replace('\n', "\0"));
    let _ = err.flush();
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            log('e', &e);
            process::exit(2);
        }
    };

    // Handshake first, before anything that may block.
    let handshake = match proto::handshake(proto::SOURCE_ENGLISH, args.result_version, &args.cache_key) {
        Ok(h) => h,
        Err(e) => {
            log('e', &format!("bad handshake: {e}"));
            process::exit(2);
        }
    };
    {
        let mut out = io::stdout().lock();
        if proto::write_frame(&mut out, &handshake).and_then(|()| out.flush()).is_err() {
            process::exit(1);
        }
    }

    let game_pid = match parent_pid() {
        Some(pid) => pid,
        None => {
            log('e', "cannot find the game process (parent process)");
            process::exit(1);
        }
    };
    log('i', &format!("started, cache key {:?}, result version {}, game pid {game_pid}", args.cache_key, args.result_version));

    // stdin reader: the addon closing stdin means the module must exit.
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut input = BufReader::new(io::stdin().lock());
        loop {
            match proto::read_frame(&mut input) {
                Ok(frame) => {
                    if tx.send(frame).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    if e.kind() != io::ErrorKind::UnexpectedEof {
                        log('e', &format!("stdin: {e}"));
                    }
                    break;
                }
            }
        }
        process::exit(0);
    });

    relay(game_pid, &rx);
}

/// Connects to the addon, forwards frames both ways, reconnects when the addon goes away.
fn relay(game_pid: u32, rx: &Receiver<Vec<u8>>) -> ! {
    let mut pending: VecDeque<Vec<u8>> = VecDeque::new();
    let mut waiting_logged = false;
    loop {
        let (requests, replies) = match connect(game_pid) {
            Ok(pipes) => pipes,
            Err(_) => {
                if !waiting_logged {
                    log('w', "GW2 Italian Translation addon not loaded yet, waiting for it");
                    waiting_logged = true;
                }
                queue_while_waiting(rx, &mut pending, RETRY);
                continue;
            }
        };
        waiting_logged = false;
        log('i', &format!("connected to the addon ({} queued requests)", pending.len()));

        let alive = Arc::new(AtomicBool::new(true));
        let reader = {
            let alive = alive.clone();
            thread::spawn(move || forward_replies(replies, &alive))
        };

        let mut requests = requests;
        'session: loop {
            // One write per frame, unbuffered: a frame that fails goes back in the queue whole.
            while let Some(frame) = pending.pop_front() {
                let mut buf = Vec::with_capacity(4 + frame.len());
                if proto::write_frame(&mut buf, &frame).and_then(|()| requests.write_all(&buf)).is_err() {
                    pending.push_front(frame);
                    break 'session;
                }
            }
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(frame) => {
                    pending.push_back(frame);
                    pending.extend(rx.try_iter());
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => process::exit(0),
            }
            if !alive.load(Ordering::Relaxed) {
                break;
            }
        }
        drop(requests);
        let _ = reader.join();
        log('w', "addon disconnected, waiting for it to come back");
    }
}

fn queue_while_waiting(rx: &Receiver<Vec<u8>>, pending: &mut VecDeque<Vec<u8>>, wait: Duration) {
    match rx.recv_timeout(wait) {
        Ok(frame) => {
            pending.push_back(frame);
            pending.extend(rx.try_iter());
        }
        Err(RecvTimeoutError::Timeout) => {}
        Err(RecvTimeoutError::Disconnected) => process::exit(0),
    }
    if pending.len() > MAX_PENDING {
        let extra = pending.len() - MAX_PENDING;
        pending.drain(..extra);
        log('w', &format!("dropped {extra} old requests while waiting for the addon"));
    }
}

fn connect(game_pid: u32) -> io::Result<(File, File)> {
    let requests = OpenOptions::new().write(true).open(proto::request_pipe(game_pid))?;
    let replies = OpenOptions::new().read(true).open(proto::reply_pipe(game_pid))?;
    Ok((requests, replies))
}

fn forward_replies(replies: File, alive: &AtomicBool) {
    let mut input = BufReader::new(replies);
    let stdout = io::stdout();
    loop {
        let frame = match proto::read_frame(&mut input) {
            Ok(f) => f,
            Err(_) => break,
        };
        let mut out = stdout.lock();
        if proto::write_frame(&mut out, &frame).and_then(|()| out.flush()).is_err() {
            process::exit(0); // the addon closed our stdout
        }
    }
    alive.store(false, Ordering::Relaxed);
}

/// The process that started us: Text Translator runs inside the game, so this is the game's pid.
fn parent_pid() -> Option<u32> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS},
            Threading::GetCurrentProcessId,
        },
    };
    unsafe {
        let me = GetCurrentProcessId();
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        let mut ok = Process32FirstW(snap, &mut entry) != 0;
        while ok {
            if entry.th32ProcessID == me {
                found = Some(entry.th32ParentProcessID);
                break;
            }
            ok = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
        found
    }
}
