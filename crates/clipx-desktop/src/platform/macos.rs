use crate::app::{Action, AppEvent, PANEL_WIDTH, Page, PanelState};
use anyhow::Result;
use clipx_core::DeviceId;
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{ProtocolObject, Sel},
    sel,
};
use objc2_app_kit::{
    NSAlert, NSColor, NSControl, NSControlTextEditingDelegate, NSFont, NSMenu, NSMenuDelegate,
    NSMenuItem, NSPopUpButton, NSStatusItem, NSTextAlignment, NSTextField, NSTextFieldDelegate,
    NSTextView, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSNotification, NSObject, NSObjectNSDelayedPerforming,
    NSObjectProtocol, NSPoint, NSRect, NSRunLoopCommonModes, NSSize, NSString,
};
use std::cell::{Cell, RefCell};
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tray_icon::menu::{ContextMenu, Menu};

struct Callbacks {
    proxy: EventLoopProxy<AppEvent>,
    actions: RefCell<Vec<Action>>,
    input: RefCell<Option<Retained<NSTextField>>>,
    picker: RefCell<Option<Retained<NSPopUpButton>>>,
    nearby: RefCell<Vec<DeviceId>>,
    submitting: Cell<bool>,
    opened: Cell<bool>,
    opening: Cell<bool>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Callbacks]
    struct MenuCallbacks;

    unsafe impl NSObjectProtocol for MenuCallbacks {}
    unsafe impl NSMenuDelegate for MenuCallbacks {
        #[unsafe(method(menuWillOpen:))]
        fn will_open(&self, _: &NSMenu) {
            self.ivars().opened.set(true);
            self.ivars().opening.set(false);
            let _ = self.ivars().proxy.send_event(AppEvent::Action(Action::MenuOpened));
            self.schedule_focus();
        }

        #[unsafe(method(menuDidClose:))]
        fn did_close(&self, _: &NSMenu) {
            self.ivars().opened.set(false);
            let _ = self.ivars().proxy.send_event(AppEvent::Action(Action::Hide));
        }
    }
    unsafe impl NSControlTextEditingDelegate for MenuCallbacks {
        #[unsafe(method(controlTextDidChange:))]
        fn text_changed(&self, _: &NSNotification) { self.submit(); }

        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn command(&self, _: &NSControl, _: &NSTextView, command: Sel) -> bool {
            if command == sel!(cancelOperation:) {
                let _ = self.ivars().proxy.send_event(AppEvent::Action(Action::Hide));
                true
            } else { false }
        }
    }
    unsafe impl NSTextFieldDelegate for MenuCallbacks {}

    impl MenuCallbacks {
        #[unsafe(method(performMenu:))]
        fn perform_menu(&self, item: &NSMenuItem) {
            if let Some(action) = self.ivars().actions.borrow().get(item.tag() as usize) {
                let _ = self.ivars().proxy.send_event(AppEvent::Action(action.clone()));
            }
        }

        #[unsafe(method(submitCode:))]
        fn submit_code(&self, _: &NSControl) { self.submit(); }

        #[unsafe(method(focusInput:))]
        fn focus_input(&self, _: Option<&NSObject>) {
            if !self.ivars().opened.get() { return; }
            if let Some(input) = self.ivars().input.borrow().as_ref()
                && let Some(window) = input.window() {
                window.makeKeyWindow();
                window.makeFirstResponder(Some(input));
            }
        }
    }
);

impl MenuCallbacks {
    fn new(proxy: EventLoopProxy<AppEvent>, mtm: MainThreadMarker) -> Retained<Self> {
        let value = Self::alloc(mtm).set_ivars(Callbacks {
            proxy,
            actions: RefCell::new(Vec::new()),
            input: RefCell::new(None),
            picker: RefCell::new(None),
            nearby: RefCell::new(Vec::new()),
            submitting: Cell::new(false),
            opened: Cell::new(false),
            opening: Cell::new(false),
        });
        // NSObject has no additional initialization requirements.
        unsafe { msg_send![super(value), init] }
    }

    fn schedule_focus(&self) {
        // The menu's window exists after menuWillOpen returns. Common modes also
        // run while AppKit is tracking the menu.
        unsafe {
            self.performSelector_withObject_afterDelay_inModes(
                sel!(focusInput:),
                None,
                0.0,
                &NSArray::from_slice(&[NSRunLoopCommonModes]),
            );
        }
    }

    fn selected(&self) -> Option<DeviceId> {
        let peers = self.ivars().nearby.borrow();
        if peers.len() == 1 {
            peers.first().copied()
        } else {
            self.ivars().picker.borrow().as_ref().and_then(|picker| {
                usize::try_from(picker.indexOfSelectedItem() - 1)
                    .ok()
                    .and_then(|index| peers.get(index).copied())
            })
        }
    }

    fn submit(&self) {
        let fields = self.ivars().input.borrow();
        let Some(input) = fields.as_ref() else {
            return;
        };
        if self.ivars().submitting.get() {
            return;
        }
        let original = input.stringValue().to_string();
        let code: String = original
            .chars()
            .filter(char::is_ascii_digit)
            .take(6)
            .collect();
        if code != original {
            input.setStringValue(&NSString::from_str(&code));
        }
        if code.len() != 6 {
            return;
        }
        self.ivars().submitting.set(true);
        let _ = self
            .ivars()
            .proxy
            .send_event(AppEvent::Action(Action::Join(code, self.selected())));
    }
}

pub struct Panel {
    menu: Menu,
    native: Retained<NSMenu>,
    callbacks: Retained<MenuCallbacks>,
    status_item: Option<Retained<NSStatusItem>>,
    error: Option<(Retained<NSMenuItem>, Retained<NSTextField>)>,
    code: Option<Retained<NSTextField>>,
    remaining: Option<Retained<NSMenuItem>>,
    generate: Option<Retained<NSMenuItem>>,
    previous: Option<PanelState>,
}

impl Panel {
    pub fn new(
        _: &EventLoopWindowTarget<AppEvent>,
        proxy: EventLoopProxy<AppEvent>,
    ) -> Result<Self> {
        let mtm = MainThreadMarker::new().expect("AppKit 需要主线程");
        let menu = Menu::new();
        // Muda owns this NSMenu; retain it while attaching our native items and delegate.
        let native = unsafe { Retained::retain(menu.ns_menu().cast::<NSMenu>()) }
            .expect("Muda 创建原生菜单");
        native.setAutoenablesItems(false);
        native.setMinimumWidth(PANEL_WIDTH);
        let callbacks = MenuCallbacks::new(proxy, mtm);
        native.setDelegate(Some(ProtocolObject::from_ref(&*callbacks)));
        let mut panel = Self {
            menu,
            native,
            callbacks,
            status_item: None,
            error: None,
            code: None,
            remaining: None,
            generate: None,
            previous: None,
        };
        panel.update(&PanelState::default());
        Ok(panel)
    }

    pub fn menu(&self) -> Menu {
        self.menu.clone()
    }

    pub fn bind_tray(&mut self, item: Retained<NSStatusItem>) {
        self.status_item = Some(item);
    }

    fn item(&self, menu: &NSMenu, title: &str, action: Option<Action>) -> Retained<NSMenuItem> {
        let item = NSMenuItem::new(self.callbacks.mtm());
        item.setTitle(&NSString::from_str(title));
        item.setEnabled(action.is_some());
        if let Some(action) = action {
            let mut actions = self.callbacks.ivars().actions.borrow_mut();
            item.setTag(actions.len() as isize);
            actions.push(action);
            // Panel retains the target until after all native items are removed.
            unsafe {
                item.setTarget(Some(&self.callbacks));
                item.setAction(Some(sel!(performMenu:)));
            }
        }
        menu.addItem(&item);
        item
    }

    fn view(&self, height: f64) -> (Retained<NSMenuItem>, Retained<NSView>) {
        let view = NSView::initWithFrame(
            NSView::alloc(self.callbacks.mtm()),
            NSRect::new(NSPoint::ZERO, NSSize::new(PANEL_WIDTH, height)),
        );
        let item = NSMenuItem::new(self.callbacks.mtm());
        item.setView(Some(&view));
        self.native.addItem(&item);
        (item, view)
    }

    fn label(&self, parent: &NSView, text: &str, size: f64) -> Retained<NSTextField> {
        let label = NSTextField::labelWithString(&NSString::from_str(text), self.callbacks.mtm());
        label.setFrame(NSRect::new(
            NSPoint::new(14.0, 4.0),
            NSSize::new(PANEL_WIDTH - 28.0, parent.frame().size.height - 8.0),
        ));
        label.setFont(Some(&NSFont::systemFontOfSize(size)));
        label.setMaximumNumberOfLines(2);
        parent.addSubview(&label);
        label
    }

    fn rebuild(&mut self, state: &PanelState) {
        self.native.removeAllItems();
        self.callbacks.ivars().actions.borrow_mut().clear();
        self.callbacks.ivars().input.replace(None);
        self.callbacks.ivars().picker.replace(None);
        self.callbacks
            .ivars()
            .nearby
            .replace(state.snapshot.nearby.iter().map(|peer| peer.id).collect());
        self.code = None;
        self.remaining = None;
        self.generate = None;
        let mtm = self.callbacks.mtm();
        self.item(&self.native, &state.title(), None);
        self.native.addItem(&NSMenuItem::separatorItem(mtm));
        match state.page {
            Page::Pair => {
                self.generate = Some(self.item(&self.native, "生成配对码", Some(Action::Generate)));
                let (_, view) = self.view(38.0);
                let input = NSTextField::initWithFrame(
                    NSTextField::alloc(mtm),
                    NSRect::new(
                        NSPoint::new(14.0, 8.0),
                        NSSize::new(PANEL_WIDTH - 28.0, 24.0),
                    ),
                );
                input.setPlaceholderString(Some(&NSString::from_str("或输入 6 位配对码")));
                input.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
                    13.0, 0.0,
                )));
                unsafe {
                    input.setDelegate(Some(ProtocolObject::from_ref(&*self.callbacks)));
                    input.setTarget(Some(&self.callbacks));
                    input.setAction(Some(sel!(submitCode:)));
                }
                view.addSubview(&input);
                self.callbacks.ivars().input.replace(Some(input));
                if state.snapshot.nearby.len() > 1 {
                    let (_, view) = self.view(30.0);
                    let picker = NSPopUpButton::initWithFrame_pullsDown(
                        NSPopUpButton::alloc(mtm),
                        NSRect::new(
                            NSPoint::new(10.0, 3.0),
                            NSSize::new(PANEL_WIDTH - 20.0, 24.0),
                        ),
                        false,
                    );
                    picker.addItemWithTitle(&NSString::from_str("选择设备"));
                    for device in &state.snapshot.nearby {
                        picker.addItemWithTitle(&NSString::from_str(&state.nearby_name(device)));
                    }
                    unsafe {
                        picker.setTarget(Some(&self.callbacks));
                        picker.setAction(Some(sel!(submitCode:)));
                    }
                    view.addSubview(&picker);
                    self.callbacks.ivars().picker.replace(Some(picker));
                }
                if !state.snapshot.devices.is_empty() {
                    self.item(&self.native, "返回设备列表", Some(Action::Cancel));
                }
            }
            Page::Waiting => {
                let (_, view) = self.view(36.0);
                let code = self.label(&view, "", 20.0);
                code.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
                    20.0, 0.0,
                )));
                code.setAlignment(NSTextAlignment::Center);
                code.setSelectable(true);
                self.code = Some(code);
                self.remaining = Some(self.item(&self.native, "正在生成…", None));
                self.generate =
                    Some(self.item(&self.native, "重新生成配对码", Some(Action::Generate)));
                self.item(&self.native, "取消配对", Some(Action::Cancel));
            }
            Page::Devices => {
                for device in &state.snapshot.devices {
                    let item = self.item(
                        &self.native,
                        &format!(
                            "{} · {}",
                            device.name,
                            if device.connected { "在线" } else { "离线" }
                        ),
                        None,
                    );
                    let submenu = NSMenu::new(mtm);
                    submenu.setAutoenablesItems(false);
                    self.item(&submenu, "移除设备…", Some(Action::Remove(device.id)));
                    item.setSubmenu(Some(&submenu));
                    item.setEnabled(true);
                }
                self.item(&self.native, "添加设备…", Some(Action::Add));
            }
        }
        let (item, view) = self.view(36.0);
        let error = self.label(&view, "", 11.0);
        item.setHidden(true);
        self.error = Some((item, error));
        self.native.addItem(&NSMenuItem::separatorItem(mtm));
        self.item(&self.native, "退出", Some(Action::Quit));
    }

    pub fn update(&mut self, state: &PanelState) {
        let rebuild = self.previous.as_ref().is_none_or(|old| {
            old.page != state.page
                || (state.page == Page::Devices && old.snapshot.devices != state.snapshot.devices)
                || (state.page == Page::Pair && old.snapshot.nearby != state.snapshot.nearby)
        });
        if rebuild {
            let selected = self.callbacks.selected();
            let text = self
                .callbacks
                .ivars()
                .input
                .borrow()
                .as_ref()
                .map(|input| input.stringValue());
            self.callbacks.ivars().submitting.set(true);
            self.rebuild(state);
            if self
                .previous
                .as_ref()
                .is_some_and(|old| old.page == Page::Pair)
                && state.page == Page::Pair
            {
                if let (Some(text), Some(input)) =
                    (text, self.callbacks.ivars().input.borrow().as_ref())
                {
                    input.setStringValue(&text);
                }
                if let Some(index) = state
                    .snapshot
                    .nearby
                    .iter()
                    .position(|peer| Some(peer.id) == selected)
                    && let Some(picker) = self.callbacks.ivars().picker.borrow().as_ref()
                {
                    picker.selectItemAtIndex(index as isize + 1);
                }
            }
            if self.callbacks.ivars().opened.get() {
                self.callbacks.schedule_focus();
            }
        }
        self.callbacks
            .ivars()
            .submitting
            .set(state.snapshot.joining);
        if let Some(input) = self.callbacks.ivars().input.borrow().as_ref() {
            input.setEnabled(!state.snapshot.joining);
            if self
                .previous
                .as_ref()
                .is_some_and(|old| old.snapshot.joining)
                && !state.snapshot.joining
                && state.snapshot.error.is_some()
            {
                unsafe {
                    input.selectText(None);
                }
            }
        }
        if let Some(picker) = self.callbacks.ivars().picker.borrow().as_ref() {
            picker.setEnabled(!state.snapshot.joining);
        }
        if let Some(item) = &self.generate {
            item.setEnabled(!state.snapshot.joining);
            item.setHidden(state.page == Page::Waiting && state.snapshot.error.is_none());
        }
        if let Some(code) = &self.code {
            code.setStringValue(&NSString::from_str(
                state.snapshot.code.as_deref().unwrap_or("正在生成…"),
            ));
        }
        if let Some(item) = &self.remaining {
            item.setTitle(&NSString::from_str(&if state.snapshot.code.is_some() {
                format!(
                    "剩余 {:02}:{:02} · 用后失效",
                    state.snapshot.seconds_left / 60,
                    state.snapshot.seconds_left % 60
                )
            } else {
                String::new()
            }));
        }
        if let Some((item, error)) = &self.error {
            error.setStringValue(&NSString::from_str(state.message().unwrap_or("")));
            let color = if state.snapshot.error.is_some() {
                NSColor::systemRedColor()
            } else {
                NSColor::secondaryLabelColor()
            };
            error.setTextColor(Some(&color));
            item.setHidden(state.message().is_none());
        }
        self.previous = Some(state.clone());
    }

    pub fn show(&mut self) {
        if self.callbacks.ivars().opened.get() || self.callbacks.ivars().opening.get() {
            return;
        }
        if let Some(button) = self
            .status_item
            .as_ref()
            .and_then(|item| item.button(self.callbacks.mtm()))
        {
            self.callbacks.ivars().opening.set(true);
            // Open outside Tao's callback so its common-mode event source can
            // deliver pairing progress while AppKit tracks the native menu.
            unsafe {
                button.performSelector_withObject_afterDelay(sel!(performClick:), None, 0.0);
            }
        }
    }

    pub fn hide(&mut self) {
        if self.callbacks.ivars().opening.replace(false)
            && let Some(button) = self
                .status_item
                .as_ref()
                .and_then(|item| item.button(self.callbacks.mtm()))
        {
            unsafe {
                NSObject::cancelPreviousPerformRequestsWithTarget_selector_object(
                    &button,
                    sel!(performClick:),
                    None,
                );
            }
        }
        if self.callbacks.ivars().opened.get() {
            self.native.cancelTracking();
        }
    }

    pub fn confirm_remove(&self, name: &str) -> bool {
        let alert = NSAlert::new(self.callbacks.mtm());
        alert.setMessageText(&NSString::from_str(&format!("移除“{name}”？")));
        alert.setInformativeText(&NSString::from_str(
            "将解除本机配对。再次连接需要重新配对。",
        ));
        alert.addButtonWithTitle(&NSString::from_str("取消"));
        alert.addButtonWithTitle(&NSString::from_str("移除"));
        alert.runModal() == 1001
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.hide();
        unsafe {
            NSObject::cancelPreviousPerformRequestsWithTarget(&self.callbacks);
            self.native.setDelegate(None);
        }
        self.native.removeAllItems();
    }
}

pub fn show_error(message: &str) {
    if let Some(mtm) = MainThreadMarker::new() {
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("无法启动 clipx"));
        alert.setInformativeText(&NSString::from_str(message));
        alert.addButtonWithTitle(&NSString::from_str("好"));
        alert.runModal();
    }
}
