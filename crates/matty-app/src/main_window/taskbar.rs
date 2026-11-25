//! The main window's unread dot on the Windows taskbar (PLAN §6.7).

#![allow(unsafe_code)]

use std::{cell::Cell, rc::Rc};

use gpui_kit::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        UI::{
            Shell::{
                DefSubclassProc, ITaskbarList3, RemoveWindowSubclass, SetWindowSubclass,
                TaskbarList,
            },
            WindowsAndMessaging::{
                CreateIconFromResourceEx, DestroyIcon, HICON, IMAGE_FLAGS, PostMessageW,
                RegisterWindowMessageW, WM_NCDESTROY,
            },
        },
    },
    core::{HSTRING, PCWSTR, w},
};

/// Kept on the UI thread alongside the main window and GPUI's OLE apartment.
pub(super) struct TaskbarBadge(Rc<State>);

struct State {
    hwnd: HWND,
    button_created: u32,
    refresh: u32,
    ready: Cell<bool>,
    pending: Cell<bool>,
    unread: Cell<bool>,
}

impl TaskbarBadge {
    pub(super) fn new(window: &Window) -> Option<Self> {
        let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
        else {
            return None;
        };
        // SAFETY: The message names are static, terminated UTF-16 strings.
        let (button_created, refresh) = unsafe {
            (
                RegisterWindowMessageW(w!("TaskbarButtonCreated")),
                RegisterWindowMessageW(w!("Matty.RefreshTaskbarBadge")),
            )
        };
        if button_created == 0 || refresh == 0 {
            tracing::warn!("could not register the taskbar badge messages");
            return None;
        }
        let state = Rc::new(State {
            hwnd: HWND(handle.hwnd.get() as _),
            button_created,
            refresh,
            ready: Cell::new(false),
            pending: Cell::new(false),
            unread: Cell::new(false),
        });
        let data = Rc::into_raw(Rc::clone(&state));
        // SAFETY: The HWND belongs to this thread. The subclass owns `data`
        // until WM_NCDESTROY, or we reclaim it immediately on failure.
        if unsafe { SetWindowSubclass(state.hwnd, Some(window_proc), 0, data as usize) }.as_bool() {
            Some(Self(state))
        } else {
            // SAFETY: The subclass was not installed; this is the reference just transferred.
            unsafe { drop(Rc::from_raw(data)) };
            tracing::warn!("could not install the taskbar badge window handler");
            None
        }
    }

    pub(super) fn set(&self, unread: bool) {
        if self.0.unread.replace(unread) != unread {
            self.0.schedule_refresh();
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    let data = data as *const State;
    // SAFETY: This call is on the window thread and the subclass owns the state.
    let state = unsafe { &*data };
    if message == WM_NCDESTROY {
        state.ready.set(false);
        // SAFETY: This is the registered callback on the owning thread.
        if unsafe { RemoveWindowSubclass(hwnd, Some(window_proc), id) }.as_bool() {
            // SAFETY: The removed subclass no longer needs its strong reference.
            unsafe { drop(Rc::from_raw(data)) };
        }
    } else if message == state.button_created {
        // Explorer sends this on first show and after recreating the button.
        state.ready.set(true);
        if state.unread.get() {
            state.schedule_refresh();
        }
    } else if message == state.refresh && state.ready.get() && state.pending.replace(false) {
        // SAFETY: Keep the state alive even if COM dispatches WM_NCDESTROY.
        let state = unsafe {
            Rc::increment_strong_count(data);
            Rc::from_raw(data)
        };
        state.apply();
        return LRESULT(0);
    }
    // SAFETY: Forward the original parameters through the subclass chain.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

impl State {
    fn schedule_refresh(&self) {
        if !self.ready.get() || self.pending.replace(true) {
            return;
        }
        // Do not call into Explorer while it is still announcing the button.
        // SAFETY: The live HWND owns this state and the message has no payload.
        if let Err(error) =
            unsafe { PostMessageW(Some(self.hwnd), self.refresh, WPARAM(0), LPARAM(0)) }
        {
            self.pending.set(false);
            tracing::debug!(%error, "scheduling taskbar badge");
        }
    }

    fn apply(&self) {
        if let Err(error) = self.apply_overlay() {
            tracing::debug!(%error, "taskbar badge");
        }
    }

    fn apply_overlay(&self) -> windows::core::Result<()> {
        // SAFETY: GPUI initialized OLE on this thread, and Explorer has
        // announced the button for this HWND.
        unsafe {
            let taskbar: ITaskbarList3 =
                CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)?;
            taskbar.HrInit()?;
            if !self.ready.get() {
                return Ok(());
            }
            if !self.unread.get() {
                return taskbar.SetOverlayIcon(self.hwnd, HICON::default(), PCWSTR::null());
            }

            let icon = CreateIconFromResourceEx(
                &DOT.0,
                true,
                0x0003_0000,
                SIZE as i32,
                SIZE as i32,
                IMAGE_FLAGS(0),
            )?;
            let description = HSTRING::from(&*crate::t!("taskbar-unread-messages"));
            let result = taskbar.SetOverlayIcon(self.hwnd, icon, &description);
            // Explorer copies the icon. Free ours even if setting the overlay failed.
            let _ = DestroyIcon(icon);
            result
        }
    }
}

const SIZE: usize = 16;
const HEADER: usize = 40;
const MASK: usize = HEADER + SIZE * SIZE * 4;

// An RT_ICON resource: a 32-bit BGRA bitmap followed by a 1-bit transparency
// mask with DWORD-aligned rows. CreateIconFromResourceEx also requires the
// resource itself to be DWORD-aligned.
#[repr(align(4))]
struct IconResource([u8; MASK + SIZE * 4]);

static DOT: IconResource = {
    let mut bytes = [0; MASK + SIZE * 4];
    bytes[0] = HEADER as u8;
    bytes[4] = SIZE as u8;
    bytes[8] = (SIZE * 2) as u8; // Bitmap and mask heights combined.
    bytes[12] = 1; // One color plane.
    bytes[14] = 32; // Bits per pixel, uncompressed.
    let mut y = 0;
    while y < SIZE {
        let mut x = 0;
        while x < SIZE {
            let dx = 2 * x as i32 - (SIZE as i32 - 1);
            let dy = 2 * y as i32 - (SIZE as i32 - 1);
            if dx * dx + dy * dy <= 13 * 13 {
                let pixel = HEADER + (y * SIZE + x) * 4;
                bytes[pixel] = 0x3b;
                bytes[pixel + 1] = 0x38;
                bytes[pixel + 2] = 0xe5;
                bytes[pixel + 3] = 0xff;
            } else {
                bytes[MASK + y * 4 + x / 8] |= 0x80 >> (x % 8);
            }
            x += 1;
        }
        y += 1;
    }
    IconResource(bytes)
};
