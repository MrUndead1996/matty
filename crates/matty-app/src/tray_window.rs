//! Hide just the main window for the Windows tray, without dropping its view.

#![allow(unsafe_code)]

use gpui_kit::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{IsWindowVisible, SW_HIDE, SW_SHOW, ShowWindowAsync},
};

fn hwnd(window: &Window) -> Option<HWND> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return None;
    };
    Some(HWND(handle.hwnd.get() as _))
}

pub(crate) fn hide(window: &Window) -> bool {
    hwnd(window).is_some_and(hide_window)
}

pub(crate) fn show_if_hidden(window: &Window) {
    if let Some(hwnd) = hwnd(window) {
        show_window_if_hidden(hwnd);
    }
}

fn hide_window(hwnd: HWND) -> bool {
    // SAFETY: The HWND is live and belongs to this UI thread. Post the
    // change so native window callbacks don't re-enter the close handler.
    match unsafe { ShowWindowAsync(hwnd, SW_HIDE) }.ok() {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "could not hide the main window to the tray");
            false
        }
    }
}

fn show_window_if_hidden(hwnd: HWND) {
    // SAFETY: The HWND is live and belongs to this UI thread. SW_SHOW keeps
    // the current window placement, including maximized state. GPUI's
    // activation then handles focus and restoring an ordinarily minimized window.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool()
            && let Err(error) = ShowWindowAsync(hwnd, SW_SHOW).ok()
        {
            tracing::warn!(%error, "could not reopen the main window from the tray");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::{
        Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, DispatchMessageW, IsWindow, IsZoomed, MSG, PM_REMOVE,
            PeekMessageW, SW_SHOWMAXIMIZED, ShowWindow, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW,
        },
        core::w,
    };

    #[test]
    fn tray_hides_the_native_window_instead_of_minimizing_it() {
        // SAFETY: Use a built-in window class on this test thread. All
        // messages and HWND operations below stay on the owning thread.
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("Matty tray test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                640,
                480,
                None,
                None,
                None,
                None,
            )
            .expect("native test window");
            let _ = ShowWindow(hwnd, SW_SHOWMAXIMIZED);
            assert!(IsWindowVisible(hwnd).as_bool());
            assert!(IsZoomed(hwnd).as_bool());

            assert!(hide_window(hwnd));
            drain_messages();
            assert!(IsWindow(Some(hwnd)).as_bool(), "the window was destroyed");
            assert!(
                !IsWindowVisible(hwnd).as_bool(),
                "the window is still eligible for a taskbar button"
            );

            show_window_if_hidden(hwnd);
            drain_messages();
            assert!(IsWindowVisible(hwnd).as_bool(), "the window did not reopen");
            assert!(IsZoomed(hwnd).as_bool(), "reopening lost maximized state");
            DestroyWindow(hwnd).expect("destroy native test window");
        }
    }

    fn drain_messages() {
        // SAFETY: Dispatch only messages from this thread's queue.
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&message);
            }
        }
    }
}
