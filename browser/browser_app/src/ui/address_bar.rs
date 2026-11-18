use std::{cell::Cell, ptr::null_mut};

use display_list::Rect;
use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Gdi::{
        CLEARTYPE_QUALITY, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DeleteObject, HBRUSH,
        HDC, HFONT, SetBkColor, SetTextColor,
    },
    UI::{
        Controls::{EM_SETLIMITTEXT, EM_SETSEL},
        Input::KeyboardAndMouse::{
            GetFocus, GetKeyState, SetFocus, VK_CONTROL, VK_ESCAPE, VK_RETURN, VK_TAB,
        },
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

use super::{
    chrome::ADDRESS_BACKGROUND,
    window::{Event, send},
};

thread_local! {
    static PAINT: Cell<Option<(HWND, HBRUSH)>> = const { Cell::new(None) };
}

pub struct AddressBar {
    parent: HWND,
    edit: HWND,
    brush: HBRUSH,
    font: HFONT,
    font_px: i32,
}

impl AddressBar {
    pub fn new(parent: HWND, instance: HINSTANCE) -> Result<Self, String> {
        let class = wide("EDIT");
        let empty = [0];
        let edit = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                empty.as_ptr(),
                WS_CHILD | WS_TABSTOP | ES_AUTOHSCROLL as u32,
                0,
                0,
                1,
                1,
                parent,
                null_mut(),
                instance,
                std::ptr::null(),
            )
        };
        if edit.is_null() {
            return Err("Could not create the address field.".into());
        }
        let brush = unsafe { CreateSolidBrush(colorref(ADDRESS_BACKGROUND)) };
        if brush.is_null() || unsafe { SetWindowSubclass(edit, Some(procedure), 1, 0) } == 0 {
            unsafe {
                if !brush.is_null() {
                    DeleteObject(brush);
                }
                DestroyWindow(edit);
            }
            return Err("Could not initialize the address field.".into());
        }
        PAINT.set(Some((edit, brush)));
        unsafe {
            SendMessageW(edit, EM_SETLIMITTEXT, network_stack::MAX_URL_LENGTH, 0);
        }
        Ok(Self {
            parent,
            edit,
            brush,
            font: null_mut(),
            font_px: 0,
        })
    }

    pub fn position(&mut self, bounds: Rect, scale: f32) {
        let pixels = |value: f32| (value * scale).round() as i32;
        let font_px = pixels(15.0).max(1);
        if font_px != self.font_px {
            let name = wide("Segoe UI");
            let font = unsafe {
                CreateFontW(
                    -font_px,
                    0,
                    0,
                    0,
                    400,
                    0,
                    0,
                    0,
                    u32::from(DEFAULT_CHARSET),
                    0,
                    0,
                    u32::from(CLEARTYPE_QUALITY),
                    0,
                    name.as_ptr(),
                )
            };
            if !font.is_null() {
                unsafe {
                    SendMessageW(self.edit, WM_SETFONT, font as usize, 1);
                    if !self.font.is_null() {
                        DeleteObject(self.font);
                    }
                }
                self.font = font;
                self.font_px = font_px;
            }
        }
        unsafe {
            SetWindowPos(
                self.edit,
                HWND_TOP,
                pixels(bounds.x + 32.0),
                pixels(bounds.y + 8.0),
                pixels((bounds.width - 49.0).max(1.0)),
                pixels(bounds.height - 16.0).max(1),
                SWP_NOACTIVATE,
            );
        }
    }

    pub fn show(&self, value: &str) {
        let value = wide(value);
        unsafe {
            SetWindowTextW(self.edit, value.as_ptr());
            ShowWindow(self.edit, SW_SHOW);
            SetFocus(self.edit);
            SendMessageW(self.edit, EM_SETSEL, 0, -1);
        }
    }

    pub fn hide(&self) {
        unsafe {
            if GetFocus() == self.edit {
                SetFocus(self.parent);
            }
            ShowWindow(self.edit, SW_HIDE);
        }
    }
}

impl Drop for AddressBar {
    fn drop(&mut self) {
        PAINT.set(None);
        unsafe {
            if IsWindow(self.edit) != 0 {
                DestroyWindow(self.edit);
            }
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
            DeleteObject(self.brush);
        }
    }
}

pub fn paint(dc: HDC, child: HWND) -> Option<LRESULT> {
    let (edit, brush) = PAINT.get()?;
    if child != edit {
        return None;
    }
    unsafe {
        SetBkColor(dc, colorref(ADDRESS_BACKGROUND));
        SetTextColor(dc, colorref(0xe8eef8));
    }
    Some(brush as LRESULT)
}

unsafe extern "system" fn procedure(
    edit: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    _data: usize,
) -> LRESULT {
    match message {
        WM_KEYDOWN => match wparam as u16 {
            VK_RETURN => {
                let length = unsafe { GetWindowTextLengthW(edit) }.max(0) as usize;
                let mut buffer = vec![0u16; length.min(network_stack::MAX_URL_LENGTH) + 1];
                let read = unsafe { GetWindowTextW(edit, buffer.as_mut_ptr(), buffer.len() as i32) }
                    .max(0) as usize;
                send(Event::AddressCommit(String::from_utf16_lossy(
                    &buffer[..read],
                )));
                return 0;
            }
            VK_ESCAPE | VK_TAB => {
                send(Event::AddressCancel);
                return 0;
            }
            0x41 | 0x4c if unsafe { GetKeyState(i32::from(VK_CONTROL)) < 0 } => {
                unsafe {
                    SendMessageW(edit, EM_SETSEL, 0, -1);
                }
                return 0;
            }
            _ => {}
        },
        WM_CHAR if matches!(wparam, 9 | 13 | 27 | 1 | 12) => return 0,
        WM_KILLFOCUS => send(Event::AddressBlur),
        WM_NCDESTROY => unsafe {
            RemoveWindowSubclass(edit, Some(procedure), subclass);
        },
        _ => {}
    }
    unsafe { DefSubclassProc(edit, message, wparam, lparam) }
}

fn colorref(rgb: u32) -> u32 {
    ((rgb & 0xff) << 16) | (rgb & 0xff00) | (rgb >> 16)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}
