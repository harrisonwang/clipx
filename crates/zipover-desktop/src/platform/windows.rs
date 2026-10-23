use crate::app::{Action, AppEvent, PANEL_WIDTH, Page, PanelState, can_pair, normalize_code};
use anyhow::{Result, bail};
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
};
use tao::{
    dpi::LogicalSize,
    event_loop::{EventLoopProxy, EventLoopWindowTarget},
    platform::windows::{WindowBuilderExtWindows, WindowExtWindows},
    window::{Window, WindowBuilder},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::SystemServices::{SS_CENTER, SS_ETCHEDHORZ, SS_LEFT},
    UI::{
        Controls::*,
        HiDpi::*,
        Input::KeyboardAndMouse::*,
        Shell::{DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};
use zipover_core::DeviceId;

const INPUT: usize = 10;
const PICKER: usize = 11;
const BUTTONS: usize = 100;

fn panel_height(state: &PanelState) -> i32 {
    let base = if state.page == Page::Waiting {
        260
    } else {
        196
    };
    base + if state.page == Page::Pair && state.snapshot.nearby.len() > 1 {
        72
    } else {
        0
    } + if state.message().is_some() { 54 } else { 0 }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn text(window: HWND) -> String {
    // The handle is a live child control owned by Panel.
    unsafe {
        let mut buffer = vec![0; GetWindowTextLengthW(window) as usize + 1];
        let length = GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32);
        String::from_utf16_lossy(&buffer[..length as usize])
    }
}

fn set_text(window: HWND, value: &str) {
    if !window.is_null() && text(window) != value {
        unsafe {
            SetWindowTextW(window, wide(value).as_ptr());
        }
    }
}

struct Callbacks {
    proxy: EventLoopProxy<AppEvent>,
    input: Cell<HWND>,
    picker: Cell<HWND>,
    pair: Cell<HWND>,
    generate: Cell<HWND>,
    nearby: RefCell<Vec<DeviceId>>,
    actions: RefCell<Vec<Action>>,
    submitting: Cell<bool>,
}

impl Callbacks {
    fn send(&self, action: Action) {
        let _ = self.proxy.send_event(AppEvent::Action(action));
    }

    fn selected(&self) -> Option<DeviceId> {
        if self.nearby.borrow().len() == 1 {
            return self.nearby.borrow().first().copied();
        }
        let index = unsafe { SendMessageW(self.picker.get(), CB_GETCURSEL, 0, 0) };
        usize::try_from(index - 1)
            .ok()
            .and_then(|index| self.nearby.borrow().get(index).copied())
    }

    fn changed(&self) {
        if self.input.get().is_null() || self.submitting.replace(true) {
            return;
        }
        let original = text(self.input.get());
        let code = normalize_code(&original);
        if code != original {
            set_text(self.input.get(), &code);
            unsafe {
                SendMessageW(self.input.get(), EM_SETSEL, code.len(), code.len() as isize);
            }
        }
        self.submitting.set(false);
        self.enable_pair();
    }

    fn enable_pair(&self) {
        if !self.pair.get().is_null() {
            let enabled = !self.submitting.get()
                && can_pair(
                    &text(self.input.get()),
                    &self.nearby.borrow(),
                    self.selected(),
                );
            unsafe {
                EnableWindow(self.pair.get(), enabled as i32);
            }
        }
    }

    fn submit(&self) {
        if self.input.get().is_null() || self.submitting.get() {
            return;
        }
        let code = normalize_code(&text(self.input.get()));
        let selected = self.selected();
        if can_pair(&code, &self.nearby.borrow(), selected) {
            self.submitting.set(true);
            self.enable_pair();
            self.send(Action::Join(code, selected));
        }
    }
}

// The subclass is removed before its boxed callback state is dropped. All access
// takes place on the window thread; Cells also allow synchronous Win32 callbacks.
unsafe extern "system" fn panel_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    let callbacks = unsafe { &*(data as *const Callbacks) };
    match message {
        WM_COMMAND => {
            let id = wparam & 0xffff;
            let notification = (wparam >> 16) as u32;
            if id == INPUT && notification == EN_CHANGE
                || id == PICKER && notification == CBN_SELCHANGE
            {
                callbacks.changed();
            } else if id == IDCANCEL as usize {
                callbacks.send(Action::Hide);
            } else if id == IDOK as usize {
                callbacks.submit();
            } else if notification == BN_CLICKED
                && let Some(action) = id
                    .checked_sub(BUTTONS)
                    .and_then(|index| callbacks.actions.borrow().get(index).cloned())
            {
                callbacks.send(action);
            }
            return 0;
        }
        WM_ERASEBKGND => unsafe {
            let mut area = zeroed();
            GetClientRect(hwnd, &mut area);
            FillRect(wparam as HDC, &area, GetSysColorBrush(COLOR_BTNFACE));
            return 1;
        },
        WM_CTLCOLORSTATIC => unsafe {
            SetBkMode(wparam as HDC, TRANSPARENT as i32);
            SetTextColor(wparam as HDC, GetSysColor(COLOR_BTNTEXT));
            return GetSysColorBrush(COLOR_BTNFACE) as isize;
        },
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

pub fn process_message(message: *const c_void) -> bool {
    // Tao supplies a valid MSG before dispatch. Only our panel participates in
    // dialog navigation, so the tray's hidden window keeps its own message loop.
    unsafe {
        let message = message.cast::<MSG>();
        let parent = GetAncestor((*message).hwnd, GA_ROOT);
        let mut data = 0;
        !parent.is_null()
            && GetWindowSubclass(parent, Some(panel_proc), 1, &mut data) != 0
            && IsDialogMessageW(parent, message) != 0
    }
}

pub struct Panel {
    window: Window,
    callbacks: Box<Callbacks>,
    children: Vec<HWND>,
    fonts: Vec<HFONT>,
    code: HWND,
    remaining: HWND,
    error: HWND,
    previous: Option<PanelState>,
    scale: f64,
    visible: bool,
}

impl Panel {
    pub fn new(
        target: &EventLoopWindowTarget<AppEvent>,
        proxy: EventLoopProxy<AppEvent>,
    ) -> Result<Self> {
        let window = WindowBuilder::new()
            .with_title("添加设备")
            .with_inner_size(LogicalSize::new(PANEL_WIDTH, 136.0))
            .with_resizable(false)
            .with_minimizable(false)
            .with_maximizable(false)
            .with_skip_taskbar(true)
            .with_visible(false)
            .build(target)?;
        let callbacks = Box::new(Callbacks {
            proxy,
            input: Cell::new(null_mut()),
            picker: Cell::new(null_mut()),
            pair: Cell::new(null_mut()),
            generate: Cell::new(null_mut()),
            nearby: RefCell::new(Vec::new()),
            actions: RefCell::new(Vec::new()),
            submitting: Cell::new(false),
        });
        let hwnd = window.hwnd() as HWND;
        unsafe {
            if SetWindowSubclass(
                hwnd,
                Some(panel_proc),
                1,
                &*callbacks as *const Callbacks as usize,
            ) == 0
            {
                bail!("创建 Win32 面板失败");
            }
            let icon = LoadImageW(
                window.hinstance() as HINSTANCE,
                std::ptr::without_provenance(1),
                IMAGE_ICON,
                0,
                0,
                LR_DEFAULTSIZE | LR_SHARED,
            );
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, icon as isize);
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, icon as isize);
        }
        Ok(Self {
            window,
            callbacks,
            children: Vec::new(),
            fonts: Vec::new(),
            code: null_mut(),
            remaining: null_mut(),
            error: null_mut(),
            previous: None,
            scale: 1.0,
            visible: false,
        })
    }

    fn child(
        &mut self,
        class: &str,
        title: &str,
        style: u32,
        id: usize,
        bounds: [i32; 4],
        font: usize,
    ) -> HWND {
        let [x, y, width, height] = bounds.map(|value| (value as f64 * self.scale).round() as i32);
        unsafe {
            let control = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(title).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                x,
                y,
                width,
                height,
                self.window.hwnd() as HWND,
                id as HMENU,
                self.window.hinstance() as HINSTANCE,
                null(),
            );
            if control.is_null() {
                let _ = self.callbacks.proxy.send_event(AppEvent::Fatal(format!(
                    "创建 Win32 控件失败：{}",
                    std::io::Error::last_os_error()
                )));
            }
            SendMessageW(control, WM_SETFONT, self.fonts[font] as usize, 1);
            self.children.push(control);
            control
        }
    }

    fn button(&mut self, title: &str, action: Action, bounds: [i32; 4]) -> HWND {
        let id = BUTTONS + self.callbacks.actions.borrow().len();
        self.callbacks.actions.borrow_mut().push(action);
        self.child(
            "BUTTON",
            title,
            WS_TABSTOP | BS_PUSHBUTTON as u32,
            id,
            bounds,
            0,
        )
    }

    fn rebuild(&mut self, state: &PanelState) {
        self.callbacks.submitting.set(true);
        unsafe {
            for child in self.children.drain(..) {
                DestroyWindow(child);
            }
            for font in self.fonts.drain(..) {
                DeleteObject(font);
            }
            let dpi = GetDpiForWindow(self.window.hwnd() as HWND);
            self.scale = dpi as f64 / 96.0;
            let mut metrics: NONCLIENTMETRICSW = zeroed();
            metrics.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
            SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS,
                metrics.cbSize,
                (&mut metrics as *mut NONCLIENTMETRICSW).cast(),
                0,
                dpi,
            );
            let mut font = metrics.lfMessageFont;
            self.fonts.push(CreateFontIndirectW(&font));
            font.lfHeight = -(30.0 * self.scale).round() as i32;
            self.fonts.push(CreateFontIndirectW(&font));
        }
        self.callbacks.input.set(null_mut());
        self.callbacks.picker.set(null_mut());
        self.callbacks.pair.set(null_mut());
        self.callbacks.generate.set(null_mut());
        self.code = null_mut();
        self.remaining = null_mut();
        self.callbacks.actions.borrow_mut().clear();
        self.callbacks
            .nearby
            .replace(state.snapshot.nearby.iter().map(|peer| peer.id).collect());
        let height = panel_height(state);
        self.window
            .set_inner_size(LogicalSize::new(PANEL_WIDTH, height as f64));
        self.child(
            "STATIC",
            "",
            SS_ETCHEDHORZ,
            0,
            [0, height - 60, PANEL_WIDTH as i32, 1],
            0,
        );
        if state.page == Page::Waiting {
            self.child("STATIC", "本机配对码", SS_LEFT, 0, [24, 24, 320, 20], 0);
            self.child(
                "STATIC",
                "在另一台设备上输入此码。",
                SS_LEFT,
                0,
                [24, 52, 320, 20],
                0,
            );
            self.code = self.child("STATIC", "", SS_CENTER, 0, [24, 88, 320, 44], 1);
            self.remaining = self.child("STATIC", "", SS_CENTER, 0, [24, 140, 320, 18], 0);
            self.child(
                "STATIC",
                "关闭窗口后仍可配对。",
                SS_LEFT,
                0,
                [24, 170, 320, 18],
                0,
            );
            self.button("取消配对", Action::Cancel, [24, height - 44, 88, 28]);
        } else {
            self.child(
                "STATIC",
                "输入另一台设备上显示的配对码。",
                SS_LEFT,
                0,
                [24, 24, 320, 20],
                0,
            );
            let mut y = 64;
            if state.snapshot.nearby.len() > 1 {
                self.child("STATIC", "设备", SS_LEFT, 0, [24, y, 320, 18], 0);
                let picker = self.child(
                    "COMBOBOX",
                    "",
                    WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
                    PICKER,
                    [24, y + 24, 320, 180],
                    0,
                );
                unsafe {
                    SendMessageW(picker, CB_ADDSTRING, 0, wide("选择设备").as_ptr() as isize);
                    for peer in &state.snapshot.nearby {
                        SendMessageW(
                            picker,
                            CB_ADDSTRING,
                            0,
                            wide(&state.nearby_name(peer)).as_ptr() as isize,
                        );
                    }
                    SendMessageW(picker, CB_SETCURSEL, 0, 0);
                }
                self.callbacks.picker.set(picker);
                y += 72;
            }
            self.child("STATIC", "配对码", SS_LEFT, 0, [24, y, 320, 18], 0);
            let input = self.child(
                "EDIT",
                "",
                WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32 | ES_NUMBER as u32,
                INPUT,
                [24, y + 24, 320, 28],
                0,
            );
            self.callbacks.input.set(input);
            unsafe {
                SendMessageW(
                    input,
                    EM_SETCUEBANNER,
                    0,
                    wide("6 位数字").as_ptr() as isize,
                );
            }
            let generate = self.button("本机配对码…", Action::Generate, [24, height - 44, 116, 28]);
            self.callbacks.generate.set(generate);
        }
        self.error = if state.message().is_some() {
            self.child("STATIC", "", SS_LEFT, 0, [24, height - 106, 320, 42], 0)
        } else {
            null_mut()
        };
        let close_x = if state.page == Page::Waiting {
            280
        } else {
            208
        };
        self.button("关闭", Action::Hide, [close_x, height - 44, 64, 28]);
        if state.page == Page::Pair {
            let pair = self.child(
                "BUTTON",
                "配对",
                WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
                IDOK as usize,
                [280, height - 44, 64, 28],
                0,
            );
            self.callbacks.pair.set(pair);
        }
    }

    pub fn update(&mut self, state: &PanelState) {
        let rebuild = self.scale != self.window.scale_factor()
            || self.previous.as_ref().is_none_or(|old| {
                old.page != state.page
                    || panel_height(old) != panel_height(state)
                    || (state.page == Page::Pair && old.snapshot.nearby != state.snapshot.nearby)
            });
        if rebuild {
            let keep_input = !state.snapshot.paired
                && self
                    .previous
                    .as_ref()
                    .is_some_and(|old| old.page == Page::Pair && state.page == Page::Pair);
            let input =
                (!self.callbacks.input.get().is_null()).then(|| text(self.callbacks.input.get()));
            let selected = self.callbacks.selected();
            self.rebuild(state);
            if keep_input {
                if let Some(input) = input {
                    set_text(self.callbacks.input.get(), &input);
                }
                if let Some(selected) = selected
                    && let Some(index) = state
                        .snapshot
                        .nearby
                        .iter()
                        .position(|peer| peer.id == selected)
                {
                    unsafe {
                        SendMessageW(self.callbacks.picker.get(), CB_SETCURSEL, index + 1, 0);
                    }
                }
            }
            if self.visible && !self.callbacks.input.get().is_null() {
                unsafe {
                    SetFocus(self.callbacks.input.get());
                }
            }
        }
        self.callbacks
            .submitting
            .set(state.snapshot.joining.is_some());
        unsafe {
            for control in [
                self.callbacks.input.get(),
                self.callbacks.picker.get(),
                self.callbacks.generate.get(),
            ] {
                EnableWindow(control, (state.snapshot.joining.is_none()) as i32);
            }
            if self
                .previous
                .as_ref()
                .is_some_and(|old| old.snapshot.joining.is_some())
                && state.snapshot.joining.is_none()
                && state.snapshot.error.is_some()
            {
                SetFocus(self.callbacks.input.get());
                SendMessageW(self.callbacks.input.get(), EM_SETSEL, 0, -1);
            }
        }
        if state.snapshot.paired {
            set_text(self.callbacks.input.get(), "");
        }
        self.callbacks.enable_pair();
        set_text(self.code, &state.code_label());
        set_text(self.remaining, &state.remaining_label());
        set_text(self.error, state.message().unwrap_or(""));
        self.previous = Some(state.clone());
    }

    pub fn show(&mut self) {
        if !self.visible {
            super::center_window(&self.window);
        }
        self.window.set_visible(true);
        self.window.set_focus();
        self.visible = true;
        if !self.callbacks.input.get().is_null() {
            unsafe {
                SetFocus(self.callbacks.input.get());
            }
        }
    }

    pub fn hide(&mut self) {
        if self.visible {
            self.window.set_visible(false);
            self.visible = false;
        }
    }

    pub fn confirm_remove(&self, name: &str) -> bool {
        unsafe {
            MessageBoxW(
                self.window.hwnd() as HWND,
                wide(&format!(
                    "移除“{name}”？\n将解除本机配对。再次连接需要重新配对。"
                ))
                .as_ptr(),
                wide("移除设备").as_ptr(),
                MB_OKCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2,
            ) == IDOK
        }
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        unsafe {
            RemoveWindowSubclass(self.window.hwnd() as HWND, Some(panel_proc), 1);
            for child in self.children.drain(..) {
                DestroyWindow(child);
            }
            for font in self.fonts.drain(..) {
                DeleteObject(font);
            }
        }
    }
}

pub fn show_error(message: &str) {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(message).as_ptr(),
            wide("无法启动 Zipover").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
