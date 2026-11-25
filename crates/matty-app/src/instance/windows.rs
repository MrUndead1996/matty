//! Windows named-pipe transport for second launches (#1148).

use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer};
use tokio::runtime::{Builder, Runtime};
use tokio::time::{sleep, timeout};
use windows::Win32::Foundation::ERROR_PIPE_BUSY;

use super::{MAX_LINK, deliver};

const IO_TIMEOUT: Duration = Duration::from_secs(2);
const ACK: u8 = 1;

fn pipe_name(path: &Path) -> String {
    // Use a stable name across releases without exposing the config path.
    let hash = path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!(r"\\.\pipe\Matty-{hash:016x}")
}

fn runtime() -> io::Result<Runtime> {
    Builder::new_current_thread().enable_all().build()
}

pub(super) fn forward(path: &Path, link: &str) -> bool {
    if link.len() > MAX_LINK {
        return false;
    }
    let result = runtime().and_then(|rt| {
        rt.block_on(async {
            timeout(IO_TIMEOUT, async {
                let name = pipe_name(path);
                let mut pipe = loop {
                    match ClientOptions::new().open(&name) {
                        Ok(pipe) => break pipe,
                        Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) => {
                            sleep(Duration::from_millis(10)).await;
                        }
                        Err(error) => return Err(error),
                    }
                };
                // The pipe name is public. Check its owner before sending
                // a URL that can contain SSO credentials.
                security::check_owner(&pipe)?;
                pipe.write_u32(link.len() as u32).await?;
                pipe.write_all(link.as_bytes()).await?;
                Ok(pipe.read_u8().await? == ACK)
            })
            .await?
        })
    });
    match result {
        Ok(accepted) => accepted,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            tracing::warn!(%error, "could not hand the link to the running Matty");
            false
        }
    }
}

pub(super) fn listen(path: &Path, links: async_channel::Sender<String>) -> io::Result<()> {
    let rt = runtime()?;
    let mut pipe = {
        let _entered = rt.enter();
        security::create_pipe(&pipe_name(path))?
    };
    std::thread::Builder::new()
        .name("matty-instance".into())
        .spawn(move || {
            let result = rt.block_on(async {
                loop {
                    pipe.connect().await?;
                    let _ = timeout(IO_TIMEOUT, receive(&mut pipe, &links)).await;
                    if links.is_closed() {
                        return Ok::<_, io::Error>(());
                    }
                    pipe.disconnect()?;
                }
            });
            if let Err(error) = result {
                tracing::warn!(%error, "a second launch will not reach this one");
            }
        })?;
    Ok(())
}

async fn receive(
    pipe: &mut NamedPipeServer,
    links: &async_channel::Sender<String>,
) -> io::Result<()> {
    let len = pipe.read_u32().await? as usize;
    if len > MAX_LINK {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = vec![0; len];
    pipe.read_exact(&mut bytes).await?;
    let link = String::from_utf8(bytes).map_err(io::Error::other)?;
    if deliver(links, &link) {
        pipe.write_u8(ACK).await?;
        // DisconnectNamedPipe discards unread data. Give the client time
        // to read the acknowledgement and close its end first.
        let _ = pipe.read_u8().await;
    }
    Ok(())
}

#[expect(
    unsafe_code,
    reason = "Win32 pipe ownership and ACLs require raw security APIs"
)]
mod security {
    use std::ffi::c_void;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

    use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
    use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows::Win32::Security::{
        EqualSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        SECURITY_ATTRIBUTES, SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::PWSTR;

    use super::io;

    struct LocalAllocation(*mut c_void);

    impl Drop for LocalAllocation {
        fn drop(&mut self) {
            // SAFETY: these pointers are returned by APIs that allocate with LocalAlloc.
            unsafe { LocalFree(Some(HLOCAL(self.0))) };
        }
    }

    fn with_current_user<T>(f: impl FnOnce(PSID) -> io::Result<T>) -> io::Result<T> {
        #[repr(C)]
        struct TokenBuffer {
            user: TOKEN_USER,
            sid: [u8; SECURITY_MAX_SID_SIZE as usize],
        }

        let mut token = HANDLE::default();
        let mut buffer = TokenBuffer {
            user: TOKEN_USER::default(),
            sid: [0; SECURITY_MAX_SID_SIZE as usize],
        };
        // SAFETY: out parameters are live and the buffer has space and
        // alignment for TOKEN_USER and the SID it points to.
        unsafe {
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
            let _token = OwnedHandle::from_raw_handle(token.0);
            GetTokenInformation(
                token,
                TokenUser,
                Some((&raw mut buffer).cast()),
                size_of::<TokenBuffer>() as u32,
                &mut 0,
            )?;
            f(buffer.user.User.Sid)
        }
    }

    pub(super) fn create_pipe(name: &str) -> io::Result<NamedPipeServer> {
        let sddl = with_current_user(|sid| {
            let mut text = PWSTR::null();
            // SAFETY: the SID is borrowed from the live token buffer.
            unsafe {
                ConvertSidToStringSidW(sid, &mut text)?;
                let _text = LocalAllocation(text.0.cast());
                let text = text.to_string().map_err(io::Error::other)?;
                // Make the user the owner even for an elevated token, and
                // allow only that user to open the pipe.
                Ok(format!("O:{text}D:P(A;;GA;;;{text})\0"))
            }
        })?;
        let sddl: Vec<u16> = sddl.encode_utf16().collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: the string is NUL-terminated and the descriptor and
        // attributes remain live until Windows has created the pipe.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                windows::core::PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )?;
            let _descriptor = LocalAllocation(descriptor.0);
            let mut attrs = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                ..Default::default()
            };
            ServerOptions::new()
                .first_pipe_instance(true)
                .max_instances(1)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(name, (&raw mut attrs).cast())
        }
    }

    pub(super) fn check_owner(pipe: &NamedPipeClient) -> io::Result<()> {
        with_current_user(|sid| {
            let mut owner = PSID::default();
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            // SAFETY: the pipe handle remains open and the returned owner
            // is borrowed from the descriptor until after EqualSid.
            unsafe {
                GetSecurityInfo(
                    HANDLE(pipe.as_raw_handle()),
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION,
                    Some(&mut owner),
                    None,
                    None,
                    None,
                    Some(&mut descriptor),
                )
                .ok()?;
                let _descriptor = LocalAllocation(descriptor.0);
                EqualSid(owner, sid).map_err(|_| io::ErrorKind::PermissionDenied.into())
            }
        })
    }
}
