//! Restore the visible bounds of a snapped Windows window.

#![allow(unsafe_code)]

use gpui_kit::{Bounds, Pixels, Window, point, px, size};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::{
    Foundation::{HWND, RECT},
    UI::WindowsAndMessaging::{GetClientRect, GetWindowRect, IsIconic},
};

pub(super) fn bounds_to_save(window: &Window, maximized: bool) -> Option<Bounds<Pixels>> {
    let normal = || Some(window.window_bounds().get_bounds());
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return normal();
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return normal();
    };
    let hwnd = HWND(handle.hwnd.get() as _);
    // SAFETY: The handle is borrowed from this live window.
    if unsafe { IsIconic(hwnd) }.as_bool() {
        return None;
    }
    // GPUI's `window_bounds` reads Win32's normal placement, which is the
    // pre-snap rectangle. Use the current rectangle unless maximized, when
    // we still need the restore bounds rather than the full monitor size.
    if maximized {
        return normal();
    }
    let mut outer = RECT::default();
    let mut client = RECT::default();
    // SAFETY: The output buffers are valid and the HWND is still live.
    if unsafe { GetWindowRect(hwnd, &mut outer).and_then(|()| GetClientRect(hwnd, &mut client)) }
        .is_err()
    {
        return normal();
    }
    // Match GPUI's symmetric frame inset so reopening these bounds gives
    // back the same outer rectangle, even with an asymmetric native frame.
    let border_width = (outer.right - outer.left) - client.right;
    let border_height = (outer.bottom - outer.top) - client.bottom;
    let logical = |value: i32| px(value as f32 / window.scale_factor());
    Some(Bounds::new(
        point(
            logical(outer.left + border_width / 2),
            logical(outer.top + border_height / 2),
        ),
        size(logical(client.right), logical(client.bottom)),
    ))
}
