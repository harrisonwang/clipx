use crate::app::{Action, AppEvent, PANEL_WIDTH, Page, PanelState, can_pair, normalize_code};
use anyhow::Result;
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{ProtocolObject, Sel},
    sel,
};
use objc2_app_kit::{
    NSAlert, NSBox, NSBoxType, NSButton, NSColor, NSControl, NSControlTextEditingDelegate, NSFont,
    NSPopUpButton, NSTextAlignment, NSTextField, NSTextFieldDelegate, NSTextView, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use std::cell::{Cell, RefCell};
use tao::{
    dpi::LogicalSize,
    event_loop::{EventLoopProxy, EventLoopWindowTarget},
    platform::macos::WindowExtMacOS,
    window::{Window, WindowBuilder},
};
use tray_icon::menu::{Menu, PredefinedMenuItem, Submenu};
use zipover_core::DeviceId;

struct Callbacks {
    proxy: EventLoopProxy<AppEvent>,
    actions: RefCell<Vec<Action>>,
    input: RefCell<Option<Retained<NSTextField>>>,
    picker: RefCell<Option<Retained<NSPopUpButton>>>,
    pair: RefCell<Option<Retained<NSButton>>>,
    nearby: RefCell<Vec<DeviceId>>,
    submitting: Cell<bool>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Callbacks]
    struct PanelCallbacks;

    unsafe impl NSObjectProtocol for PanelCallbacks {}
    unsafe impl NSControlTextEditingDelegate for PanelCallbacks {
        #[unsafe(method(controlTextDidChange:))]
        fn text_changed(&self, _: &NSNotification) { self.changed(); }

        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn command(&self, _: &NSControl, _: &NSTextView, command: Sel) -> bool {
            if command == sel!(cancelOperation:) {
                self.send(Action::Hide);
                true
            } else { false }
        }
    }
    unsafe impl NSTextFieldDelegate for PanelCallbacks {}

    impl PanelCallbacks {
        #[unsafe(method(performAction:))]
        fn perform_action(&self, button: &NSButton) {
            if let Some(action) = self.ivars().actions.borrow().get(button.tag() as usize) {
                self.send(action.clone());
            }
        }

        #[unsafe(method(submitCode:))]
        fn submit_code(&self, _: &NSControl) { self.submit(); }

        #[unsafe(method(selectionChanged:))]
        fn selection_changed(&self, _: &NSControl) { self.changed(); }
    }
);

impl PanelCallbacks {
    fn new(proxy: EventLoopProxy<AppEvent>, mtm: MainThreadMarker) -> Retained<Self> {
        let value = Self::alloc(mtm).set_ivars(Callbacks {
            proxy,
            actions: RefCell::new(Vec::new()),
            input: RefCell::new(None),
            picker: RefCell::new(None),
            pair: RefCell::new(None),
            nearby: RefCell::new(Vec::new()),
            submitting: Cell::new(false),
        });
        // NSObject has no additional initialization requirements.
        unsafe { msg_send![super(value), init] }
    }

    fn send(&self, action: Action) {
        let _ = self.ivars().proxy.send_event(AppEvent::Action(action));
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

    fn changed(&self) {
        let fields = self.ivars().input.borrow();
        let Some(input) = fields.as_ref() else { return };
        let original = input.stringValue().to_string();
        let code = normalize_code(&original);
        if code != original {
            input.setStringValue(&NSString::from_str(&code));
        }
        if let Some(pair) = self.ivars().pair.borrow().as_ref() {
            pair.setEnabled(
                !self.ivars().submitting.get()
                    && can_pair(&code, &self.ivars().nearby.borrow(), self.selected()),
            );
        }
    }

    fn submit(&self) {
        if self.ivars().submitting.get() {
            return;
        }
        let fields = self.ivars().input.borrow();
        let Some(input) = fields.as_ref() else { return };
        let code = normalize_code(&input.stringValue().to_string());
        let selected = self.selected();
        if can_pair(&code, &self.ivars().nearby.borrow(), selected) {
            self.ivars().submitting.set(true);
            self.send(Action::Join(code, selected));
        }
    }
}

fn panel_height(state: &PanelState) -> f64 {
    let base = if state.page == Page::Waiting {
        260.0
    } else {
        196.0
    };
    base + if state.page == Page::Pair && state.snapshot.nearby.len() > 1 {
        72.0
    } else {
        0.0
    } + if state.message().is_some() { 54.0 } else { 0.0 }
}

pub struct Panel {
    window: Window,
    _shortcuts: Menu,
    root: Retained<NSView>,
    callbacks: Retained<PanelCallbacks>,
    generate: Option<Retained<NSButton>>,
    code: Option<Retained<NSTextField>>,
    remaining: Option<Retained<NSTextField>>,
    error: Option<Retained<NSTextField>>,
    previous: Option<PanelState>,
    visible: bool,
}

impl Panel {
    pub fn new(
        target: &EventLoopWindowTarget<AppEvent>,
        proxy: EventLoopProxy<AppEvent>,
    ) -> Result<Self> {
        let mtm = MainThreadMarker::new().expect("AppKit 需要主线程");
        // AppKit routes standard text-editing shortcuts through the application menu.
        let shortcuts = Menu::with_items(&[
            &Submenu::with_items(
                "Zipover",
                true,
                &[&PredefinedMenuItem::quit(Some("退出 Zipover"))],
            )?,
            &Submenu::with_items(
                "编辑",
                true,
                &[
                    &PredefinedMenuItem::cut(Some("剪切")),
                    &PredefinedMenuItem::copy(Some("拷贝")),
                    &PredefinedMenuItem::paste(Some("粘贴")),
                    &PredefinedMenuItem::select_all(Some("全选")),
                ],
            )?,
        ])?;
        shortcuts.init_for_nsapp();
        let window = WindowBuilder::new()
            .with_title("添加设备")
            .with_inner_size(LogicalSize::new(PANEL_WIDTH, 196.0))
            .with_resizable(false)
            .with_minimizable(false)
            .with_maximizable(false)
            .with_visible(false)
            .build(target)?;
        let root = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(PANEL_WIDTH, 196.0)),
        );
        // Keep Tao's content view and event handling; native controls live in a child view.
        unsafe { &*window.ns_view().cast::<NSView>() }.addSubview(&root);
        Ok(Self {
            window,
            _shortcuts: shortcuts,
            root,
            callbacks: PanelCallbacks::new(proxy, mtm),
            generate: None,
            code: None,
            remaining: None,
            error: None,
            previous: None,
            visible: false,
        })
    }

    fn bounds(&self, x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect::new(
            NSPoint::new(x, self.root.frame().size.height - y - height),
            NSSize::new(width, height),
        )
    }

    fn label(&self, text: &str, y: f64, height: f64) -> Retained<NSTextField> {
        let label =
            NSTextField::wrappingLabelWithString(&NSString::from_str(text), self.callbacks.mtm());
        label.setFrame(self.bounds(24.0, y, PANEL_WIDTH - 48.0, height));
        label.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        self.root.addSubview(&label);
        label
    }

    fn button(
        &self,
        title: &str,
        action: Option<Action>,
        x: f64,
        width: f64,
    ) -> Retained<NSButton> {
        // Panel retains the target while its controls are alive.
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(&self.callbacks),
                Some(if action.is_some() {
                    sel!(performAction:)
                } else {
                    sel!(submitCode:)
                }),
                self.callbacks.mtm(),
            )
        };
        if let Some(action) = action {
            let mut actions = self.callbacks.ivars().actions.borrow_mut();
            button.setTag(actions.len() as isize);
            actions.push(action);
        }
        button.setFrame(self.bounds(x, self.root.frame().size.height - 44.0, width, 28.0));
        self.root.addSubview(&button);
        button
    }

    fn rebuild(&mut self, state: &PanelState) {
        let mtm = self.callbacks.mtm();
        if let Some(window) = self.root.window() {
            window.makeFirstResponder(None);
        }
        for child in self.root.subviews() {
            child.removeFromSuperview();
        }
        self.callbacks.ivars().input.replace(None);
        self.callbacks.ivars().picker.replace(None);
        self.callbacks.ivars().pair.replace(None);
        self.callbacks.ivars().actions.borrow_mut().clear();
        self.callbacks
            .ivars()
            .nearby
            .replace(state.snapshot.nearby.iter().map(|p| p.id).collect());
        self.generate = None;
        self.code = None;
        self.remaining = None;
        self.error = None;
        let height = panel_height(state);
        self.window
            .set_inner_size(LogicalSize::new(PANEL_WIDTH, height));
        self.root
            .setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(PANEL_WIDTH, height)));
        let separator = NSBox::initWithFrame(
            NSBox::alloc(mtm),
            self.bounds(0.0, height - 60.0, PANEL_WIDTH, 1.0),
        );
        separator.setBoxType(NSBoxType::Separator);
        self.root.addSubview(&separator);

        if state.page == Page::Waiting {
            self.label("本机配对码", 24.0, 20.0);
            self.label("在另一台设备上输入此码。", 52.0, 20.0);
            let code = self.label("", 88.0, 44.0);
            code.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
                30.0, 0.0,
            )));
            code.setAlignment(NSTextAlignment::Center);
            code.setSelectable(true);
            self.code = Some(code);
            let remaining = self.label("", 140.0, 18.0);
            remaining.setAlignment(NSTextAlignment::Center);
            remaining.setTextColor(Some(&NSColor::secondaryLabelColor()));
            self.remaining = Some(remaining);
            self.label("关闭窗口后仍可配对。", 170.0, 18.0)
                .setTextColor(Some(&NSColor::secondaryLabelColor()));
            self.button("取消配对", Some(Action::Cancel), 20.0, 96.0);
        } else {
            self.label("输入另一台设备上显示的配对码。", 24.0, 20.0);
            let mut y = 64.0;
            if state.snapshot.nearby.len() > 1 {
                self.label("设备", y, 18.0);
                let picker = NSPopUpButton::initWithFrame_pullsDown(
                    NSPopUpButton::alloc(mtm),
                    self.bounds(20.0, y + 24.0, PANEL_WIDTH - 40.0, 26.0),
                    false,
                );
                picker.addItemWithTitle(&NSString::from_str("选择设备"));
                for device in &state.snapshot.nearby {
                    picker.addItemWithTitle(&NSString::from_str(&state.nearby_name(device)));
                }
                unsafe {
                    picker.setTarget(Some(&self.callbacks));
                    picker.setAction(Some(sel!(selectionChanged:)));
                }
                self.root.addSubview(&picker);
                self.callbacks.ivars().picker.replace(Some(picker));
                y += 72.0;
            }
            self.label("配对码", y, 18.0);
            let input = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                self.bounds(24.0, y + 24.0, PANEL_WIDTH - 48.0, 28.0),
            );
            input.setPlaceholderString(Some(&NSString::from_str("6 位数字")));
            input.setFont(Some(&NSFont::systemFontOfSize(16.0)));
            unsafe {
                input.setDelegate(Some(ProtocolObject::from_ref(&*self.callbacks)));
                input.setTarget(Some(&self.callbacks));
                input.setAction(Some(sel!(submitCode:)));
            }
            self.root.addSubview(&input);
            self.callbacks.ivars().input.replace(Some(input));
            self.generate = Some(self.button("本机配对码…", Some(Action::Generate), 20.0, 116.0));
            let pair = self.button("配对", None, 280.0, 68.0);
            pair.setKeyEquivalent(&NSString::from_str("\r"));
            self.callbacks.ivars().pair.replace(Some(pair));
        }
        let close_x = if state.page == Page::Waiting {
            280.0
        } else {
            204.0
        };
        let close = self.button("关闭", Some(Action::Hide), close_x, 68.0);
        close.setKeyEquivalent(&NSString::from_str("\u{1b}"));
        if state.message().is_some() {
            self.error = Some(self.label("", height - 106.0, 42.0));
        }
    }

    pub fn update(&mut self, state: &PanelState) {
        let rebuild = self.previous.as_ref().is_none_or(|old| {
            old.page != state.page
                || panel_height(old) != panel_height(state)
                || (state.page == Page::Pair && old.snapshot.nearby != state.snapshot.nearby)
        });
        if rebuild {
            let selected = self.callbacks.selected();
            let input = self
                .callbacks
                .ivars()
                .input
                .borrow()
                .as_ref()
                .map(|input| input.stringValue());
            let keep_input = !state.snapshot.paired
                && self
                    .previous
                    .as_ref()
                    .is_some_and(|old| old.page == Page::Pair && state.page == Page::Pair);
            self.rebuild(state);
            if keep_input {
                if let (Some(text), Some(input)) =
                    (input, self.callbacks.ivars().input.borrow().as_ref())
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
            if self.visible {
                self.focus_input();
            }
        }
        self.callbacks
            .ivars()
            .submitting
            .set(state.snapshot.joining.is_some());
        if let Some(input) = self.callbacks.ivars().input.borrow().as_ref() {
            input.setEnabled(state.snapshot.joining.is_none());
            if state.snapshot.paired {
                input.setStringValue(&NSString::from_str(""));
            }
        }
        if let Some(picker) = self.callbacks.ivars().picker.borrow().as_ref() {
            picker.setEnabled(state.snapshot.joining.is_none());
        }
        if let Some(generate) = &self.generate {
            generate.setEnabled(state.snapshot.joining.is_none());
        }
        self.callbacks.changed();
        if let Some(code) = &self.code {
            code.setStringValue(&NSString::from_str(&state.code_label()));
        }
        if let Some(remaining) = &self.remaining {
            remaining.setStringValue(&NSString::from_str(&state.remaining_label()));
        }
        if let Some(error) = &self.error {
            error.setStringValue(&NSString::from_str(state.message().unwrap_or("")));
            error.setToolTip(state.message().map(NSString::from_str).as_deref());
            let color = if state.snapshot.error.is_some() {
                NSColor::systemRedColor()
            } else {
                NSColor::secondaryLabelColor()
            };
            error.setTextColor(Some(&color));
        }
        self.previous = Some(state.clone());
    }

    fn focus_input(&self) {
        if let Some(input) = self.callbacks.ivars().input.borrow().as_ref()
            && let Some(window) = self.root.window()
        {
            window.makeFirstResponder(Some(input));
        }
    }

    pub fn show(&mut self) {
        if !self.visible {
            super::center_window(&self.window);
        }
        self.window.set_visible(true);
        self.window.set_focus();
        self.visible = true;
        self.focus_input();
    }

    pub fn hide(&mut self) {
        self.window.set_visible(false);
        self.visible = false;
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

pub fn show_error(message: &str) {
    if let Some(mtm) = MainThreadMarker::new() {
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("无法启动 Zipover"));
        alert.setInformativeText(&NSString::from_str(message));
        alert.addButtonWithTitle(&NSString::from_str("好"));
        alert.runModal();
    }
}
