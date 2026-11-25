//! One Matty per user (#84, PLAN §6.7). The first launch listens on a
//! local socket or named pipe; a second launch hands its link, or nothing
//! to only bring the window forward, to the first and exits.
//!
//! macOS already sends a bundled app's links to the running copy as
//! open-url events; the socket covers a launch from the command line
//! there and every launch on Linux. Windows forwards over a named pipe.
//! Preview mode never forwards, so fixture windows can run side by side.

use std::path::PathBuf;

/// One endpoint per config folder, and a separate one for debug builds
/// so they never take a release build's links. On Windows this path
/// identifies the pipe; it is not created on disk.
fn socket_path() -> Option<PathBuf> {
    let name = if cfg!(debug_assertions) {
        "matty-dev.sock"
    } else {
        "matty.sock"
    };
    dirs::config_dir().map(|dir| dir.join("Matty").join(name))
}

/// Hands `link` to the Matty already running, if there is one; `true`
/// when it took it and this launch should exit.
pub fn forward(link: Option<&str>) -> bool {
    if crate::preview::enabled() {
        return false;
    }
    socket_path().is_some_and(|path| imp::forward(&path, link.unwrap_or("")))
}

/// Takes later launches' links, sending each to `links`; an empty one
/// asks only for the window. Logs and carries on when it cannot listen.
pub(crate) fn listen(links: async_channel::Sender<String>) {
    if crate::preview::enabled() {
        return;
    }
    if let Some(path) = socket_path()
        && let Err(error) = imp::listen(&path, links)
    {
        tracing::warn!(%error, "a second launch will not reach this one");
    }
}

#[cfg(any(unix, windows))]
const MAX_LINK: usize = 8 * 1024;

#[cfg(any(unix, windows))]
fn deliver(links: &async_channel::Sender<String>, link: &str) -> bool {
    let link = link.trim();
    if !link.is_empty() {
        tracing::info!(
            link = %crate::deeplink::redacted(link),
            "another launch handed over a link"
        );
    }
    links.try_send(link.to_owned()).is_ok()
}

#[cfg(unix)]
mod imp {
    use std::io::{self, Read as _, Write as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::time::Duration;

    use super::{MAX_LINK, deliver};

    pub(super) fn forward(path: &Path, link: &str) -> bool {
        let Ok(mut stream) = UnixStream::connect(path) else {
            return false;
        };
        let sent = stream
            .write_all(link.as_bytes())
            .and_then(|()| stream.shutdown(std::net::Shutdown::Write));
        match sent {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "could not hand the link to the running Matty");
                false
            }
        }
    }

    pub(super) fn listen(path: &Path, links: async_channel::Sender<String>) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let listener = match UnixListener::bind(path) {
            Ok(listener) => listener,
            // Nothing answered `forward`, so the socket is a crashed
            // launch's leftover.
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                std::fs::remove_file(path)?;
                UnixListener::bind(path)?
            }
            Err(error) => return Err(error),
        };
        std::thread::Builder::new()
            .name("matty-instance".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let Some(link) = read_link(stream) else {
                        continue;
                    };
                    if !deliver(&links, &link) {
                        break;
                    }
                }
            })?;
        Ok(())
    }

    fn read_link(stream: UnixStream) -> Option<String> {
        // macOS refuses socket options (EINVAL) once the other launch has
        // already sent its link and closed, which is the usual case; the
        // link is still there to read, so carry on without the timeout.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut link = String::new();
        stream
            .take(MAX_LINK as u64)
            .read_to_string(&mut link)
            .ok()?;
        Some(link)
    }
}

#[cfg(windows)]
#[path = "instance/windows.rs"]
mod imp;

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::path::Path;

    pub(super) fn forward(_path: &Path, _link: &str) -> bool {
        false
    }

    pub(super) fn listen(
        _path: &Path,
        _links: async_channel::Sender<String>,
    ) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::imp;

    #[test]
    fn a_second_launch_hands_its_link_to_the_first() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let path = dir.path().join("Matty").join("matty.sock");
        assert!(
            !imp::forward(&path, "matty://x/team/pl/a"),
            "nobody listens"
        );
        let (tx, rx) = async_channel::unbounded();
        imp::listen(&path, tx).unwrap_or_else(|e| panic!("{e}"));
        #[cfg(windows)]
        {
            let (other, _) = async_channel::unbounded();
            assert!(
                imp::listen(&path, other).is_err(),
                "the first owns the pipe"
            );
            assert!(!imp::forward(&path, &"x".repeat(super::MAX_LINK + 1)));
        }
        let callback = "mattermost://chat.example.com/login/desktop?client_token=test-client&server_token=test-server";
        assert!(imp::forward(&path, &format!("{callback}\n")));
        assert!(imp::forward(&path, ""));
        assert_eq!(rx.recv_blocking().as_deref(), Ok(callback));
        assert_eq!(rx.recv_blocking().as_deref(), Ok(""));
    }

    #[test]
    #[cfg(unix)]
    fn a_crashed_launchs_socket_is_replaced() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let path = dir.path().join("matty.sock");
        drop(std::os::unix::net::UnixListener::bind(&path));
        assert!(path.exists());
        assert!(!imp::forward(&path, "x"));
        let (tx, rx) = async_channel::unbounded();
        imp::listen(&path, tx).unwrap_or_else(|e| panic!("{e}"));
        assert!(imp::forward(&path, "matty://user/ana"));
        assert_eq!(rx.recv_blocking().as_deref(), Ok("matty://user/ana"));
    }
}
