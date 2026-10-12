//! The helper as a separate process with administrator privileges, which reading physical
//! drives needs, while the GUI keeps running as the user.
//!
//! The GUI makes a folder only its user can enter, listens on a Unix socket in it and
//! starts the helper elevated (macOS: the system's administrator password prompt through
//! `osascript ... with administrator privileges`). The helper connects to the socket and
//! serves [`Helper`] calls as JSON lines ([`hakk_protocol::wire`]); events go the same
//! way. When the GUI goes away the socket closes and the helper exits. Files the helper
//! recovers are handed to the GUI's user, so they are not left owned by root.
//!
//! Reference: osascript(1) and AppleScript `do shell script ... with administrator
//! privileges` (Apple Technical Note TN2065); unix(7) sockets.

use std::{
    collections::HashMap,
    io::{self, BufRead, BufReader, Write},
    os::unix::{
        fs::DirBuilderExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{Sender, bounded};
use hakk_protocol::{
    Destination, Drive, DriveId, EventSink, Helper, HelperError, RecoveryId, RecoveryRequest,
    RepairRequest, ScanId,
    wire::{Answer, Call, HelperMessage, Request},
};

use crate::RealHelper;

/// Argument that makes the program run as the helper instead of the GUI.
pub const SERVE_ARG: &str = "--serve-helper";
/// How long to wait for the helper to connect (the user types the password meanwhile).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(300);
/// Calls waiting for their reply, by request id.
type Waiting = Mutex<HashMap<u64, Sender<Result<Answer, HelperError>>>>;

/// How long a call may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Runs the helper: connects to the GUI's socket and serves it until it closes.
/// `owner` (user id, group id) receives the recovered files and saved scans, which go in
/// `data_dir` (the GUI's user's folder; root's own folder otherwise).
pub fn serve(
    socket: &Path,
    owner: Option<(u32, u32)>,
    data_dir: Option<PathBuf>,
) -> io::Result<()> {
    let stream = UnixStream::connect(socket)?;
    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    let events = writer.clone();
    let sink: EventSink = Box::new(move |event| {
        let _ = send(&events, &HelperMessage::Event { event });
    });
    let mut helper = RealHelper::new(sink);
    helper.set_owner(owner);
    if let Some(dir) = data_dir {
        helper.set_data_dir(dir);
    }
    for line in BufReader::new(stream).lines() {
        let line = line?;
        let request: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("helper: bad request: {e}");
                continue;
            }
        };
        let result = dispatch(&helper, request.call);
        send(
            &writer,
            &HelperMessage::Reply {
                id: request.id,
                result,
            },
        )?;
    }
    Ok(())
}

fn dispatch(h: &RealHelper, call: Call) -> Result<Answer, HelperError> {
    Ok(match call {
        Call::ListDrives => Answer::Drives(h.list_drives()?),
        Call::OpenImage { path } => Answer::Drive(h.open_image(&path)?),
        Call::ListDestinations { source } => Answer::Destinations(h.list_destinations(&source)?),
        Call::StartScan { drive, fresh } => Answer::Scan(h.start_scan(&drive, fresh)?),
        Call::PauseScan { scan } => h.pause_scan(scan).map(|()| Answer::Done)?,
        Call::ResumeScan { scan } => h.resume_scan(scan).map(|()| Answer::Done)?,
        Call::CancelScan { scan } => h.cancel_scan(scan).map(|()| Answer::Done)?,
        Call::Recover { request } => Answer::Recovery(h.recover(request)?),
        Call::Repair { request } => h.repair(request).map(|()| Answer::Done)?,
    })
}

fn send<T: serde::Serialize>(writer: &Mutex<UnixStream>, message: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(io::Error::other)?;
    line.push(b'\n');
    let mut w = writer.lock().unwrap_or_else(PoisonError::into_inner);
    w.write_all(&line)?;
    w.flush()
}

/// A helper in another process, reached through a Unix socket.
pub struct RemoteHelper {
    writer: Arc<Mutex<UnixStream>>,
    pending: Arc<Waiting>,
    next: AtomicU64,
    /// Removed when the GUI quits.
    dir: PathBuf,
}

impl RemoteHelper {
    /// Starts this program as the helper with administrator privileges and connects to it.
    /// Fails if the user declines the password prompt or the helper does not come up.
    pub fn launch_elevated(sink: EventSink, prompt: &str) -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let dir = std::env::temp_dir().join(format!("hakk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let socket = dir.join("helper.sock");
        let listener = UnixListener::bind(&socket).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;

        // SAFETY: getuid and getgid cannot fail and have no preconditions.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        // The helper's output goes to a log the user can read.
        let log = log_dir().map_or_else(|| PathBuf::from("/dev/null"), |d| d.join("helper.log"));
        let command = format!(
            "{} {SERVE_ARG} {} --owner {uid}:{gid} --data-dir {} >>{} 2>&1 &",
            shell_quote(&exe.to_string_lossy()),
            shell_quote(&socket.to_string_lossy()),
            shell_quote(&crate::default_data_dir().to_string_lossy()),
            shell_quote(&log.to_string_lossy()),
        );
        let script = format!(
            "do shell script \"{}\" with prompt \"{}\" with administrator privileges",
            applescript_escape(&command),
            applescript_escape(prompt),
        );
        let status = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            let _ = std::fs::remove_dir_all(&dir);
            return Err("administrator privileges were not granted".into());
        }

        let started = Instant::now();
        let stream = loop {
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if started.elapsed() > CONNECT_TIMEOUT {
                        let _ = std::fs::remove_dir_all(&dir);
                        return Err("the helper did not start".into());
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e.to_string()),
            }
        };
        stream.set_nonblocking(false).map_err(|e| e.to_string())?;
        let writer = Arc::new(Mutex::new(stream.try_clone().map_err(|e| e.to_string())?));
        let pending: Arc<Waiting> = Arc::default();
        let waiting = pending.clone();
        thread::Builder::new()
            .name("helper-reader".into())
            .spawn(move || read_messages(stream, &sink, &waiting))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            writer,
            pending,
            next: AtomicU64::new(1),
            dir,
        })
    }

    fn call(&self, call: Call) -> Result<Answer, HelperError> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = bounded(1);
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, tx);
        send(&self.writer, &Request { id, call }).map_err(|e| unavailable(&e))?;
        rx.recv_timeout(CALL_TIMEOUT).map_err(|e| unavailable(&e))?
    }
}

impl Drop for RemoteHelper {
    fn drop(&mut self) {
        // Closing the socket ends the helper.
        let _ = self
            .writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .shutdown(std::net::Shutdown::Both);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn unavailable(e: &dyn std::fmt::Display) -> HelperError {
    HelperError::HelperUnavailable {
        detail: e.to_string(),
    }
}

/// Hands replies to the calls waiting for them and events to the GUI, until the helper
/// goes away; then every waiting call fails.
fn read_messages(stream: UnixStream, sink: &EventSink, pending: &Waiting) {
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        match serde_json::from_str::<HelperMessage>(&line) {
            Ok(HelperMessage::Reply { id, result }) => {
                let waiter = pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id);
                if let Some(tx) = waiter {
                    let _ = tx.send(result);
                }
            }
            Ok(HelperMessage::Event { event }) => sink(event),
            Err(e) => eprintln!("helper sent a bad message: {e}"),
        }
    }
    let gone = HelperError::HelperUnavailable {
        detail: "the helper exited".into(),
    };
    for (_, tx) in pending
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .drain()
    {
        let _ = tx.send(Err(gone.clone()));
    }
}

fn wrong(answer: Answer) -> HelperError {
    HelperError::Internal {
        detail: format!("unexpected answer from the helper: {answer:?}"),
    }
}

impl Helper for RemoteHelper {
    fn list_drives(&self) -> Result<Vec<Drive>, HelperError> {
        match self.call(Call::ListDrives)? {
            Answer::Drives(d) => Ok(d),
            other => Err(wrong(other)),
        }
    }

    fn open_image(&self, path: &str) -> Result<Drive, HelperError> {
        match self.call(Call::OpenImage { path: path.into() })? {
            Answer::Drive(d) => Ok(d),
            other => Err(wrong(other)),
        }
    }

    fn list_destinations(&self, source: &DriveId) -> Result<Vec<Destination>, HelperError> {
        match self.call(Call::ListDestinations {
            source: source.clone(),
        })? {
            Answer::Destinations(d) => Ok(d),
            other => Err(wrong(other)),
        }
    }

    fn start_scan(&self, drive: &DriveId, fresh: bool) -> Result<ScanId, HelperError> {
        match self.call(Call::StartScan {
            drive: drive.clone(),
            fresh,
        })? {
            Answer::Scan(s) => Ok(s),
            other => Err(wrong(other)),
        }
    }

    fn pause_scan(&self, scan: ScanId) -> Result<(), HelperError> {
        self.call(Call::PauseScan { scan }).map(|_| ())
    }

    fn resume_scan(&self, scan: ScanId) -> Result<(), HelperError> {
        self.call(Call::ResumeScan { scan }).map(|_| ())
    }

    fn cancel_scan(&self, scan: ScanId) -> Result<(), HelperError> {
        self.call(Call::CancelScan { scan }).map(|_| ())
    }

    fn recover(&self, request: RecoveryRequest) -> Result<RecoveryId, HelperError> {
        match self.call(Call::Recover { request })? {
            Answer::Recovery(r) => Ok(r),
            other => Err(wrong(other)),
        }
    }

    fn repair(&self, request: RepairRequest) -> Result<(), HelperError> {
        self.call(Call::Repair { request }).map(|_| ())
    }
}

/// The folder for the app's logs (macOS: `~/Library/Logs/hakk`), created if needed.
pub fn log_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home).join("Library/Logs/hakk");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// `s` as one word for /bin/sh.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `s` inside an AppleScript string literal.
fn applescript_escape(s: &str) -> String {
    s.replace('\\', r"\\").replace('"', "\\\"")
}

/// Parses `--owner uid:gid`.
pub fn parse_owner(arg: &str) -> Option<(u32, u32)> {
    let (u, g) = arg.split_once(':')?;
    Some((u.parse().ok()?, g.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn dir_data() -> PathBuf {
        std::env::temp_dir().join(format!("hakk-test-{}", std::process::id()))
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("/a b/it's"), r"'/a b/it'\''s'");
        assert_eq!(
            applescript_escape(r#"say "hi" \ ok"#),
            r#"say \"hi\" \\ ok"#
        );
        assert_eq!(parse_owner("501:20"), Some((501, 20)));
        assert_eq!(parse_owner("x"), None);
    }

    /// A served helper answers calls over a socket the same as in-process.
    #[test]
    fn serves_over_a_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = {
            let socket = socket.clone();
            thread::spawn(move || serve(&socket, None, Some(dir_data())))
        };
        let (stream, _) = listener.accept().unwrap();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: EventSink = Box::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
        });
        let pending: Arc<Waiting> = Arc::default();
        let waiting = pending.clone();
        let reader = stream.try_clone().unwrap();
        thread::spawn(move || read_messages(reader, &sink, &waiting));
        let client = RemoteHelper {
            writer: Arc::new(Mutex::new(stream)),
            pending,
            next: AtomicU64::new(1),
            dir: dir.path().join("unused"),
        };
        let image = dir.path().join("x.img");
        std::fs::write(&image, vec![0u8; 1 << 20]).unwrap();
        let drive = client.open_image(&image.to_string_lossy()).unwrap();
        assert_eq!(drive.capacity_bytes, 1 << 20);
        assert!(client.list_drives().unwrap().contains(&drive));
        assert!(matches!(
            client.start_scan(&DriveId("nope".into()), false),
            Err(HelperError::DriveNotFound)
        ));
        drop(client);
        drop(rx);
        server.join().unwrap().unwrap();
    }
}
