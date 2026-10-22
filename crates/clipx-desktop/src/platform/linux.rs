use crate::app::{Action, AppEvent, PANEL_WIDTH, Page, PanelState, can_pair, normalize_code};
use anyhow::Result;
use clipx_core::DeviceId;
use gtk::{Orientation, prelude::*};
use std::{cell::Cell, rc::Rc};
use tao::{
    event_loop::{EventLoopProxy, EventLoopWindowTarget},
    platform::unix::WindowExtUnix,
    window::{Window, WindowBuilder},
};

pub struct Panel {
    window: Window,
    root: gtk::Box,
    proxy: EventLoopProxy<AppEvent>,
    input: Option<gtk::Entry>,
    picker: Option<gtk::ComboBoxText>,
    pair: Option<gtk::Button>,
    generate: Option<gtk::Button>,
    code: Option<gtk::Label>,
    remaining: Option<gtk::Label>,
    error: gtk::Label,
    submitting: Rc<Cell<bool>>,
    previous: Option<PanelState>,
    visible: bool,
}

fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_line_wrap(true);
    label.set_max_width_chars(32);
    label
}

fn selected(picker: Option<&gtk::ComboBoxText>, nearby: &[DeviceId]) -> Option<DeviceId> {
    if nearby.len() == 1 {
        nearby.first().copied()
    } else {
        picker
            .and_then(|picker| picker.active())
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| nearby.get(index as usize).copied())
    }
}

fn changed(
    input: &gtk::Entry,
    picker: Option<&gtk::ComboBoxText>,
    pair: &gtk::Button,
    nearby: &[DeviceId],
    busy: &Cell<bool>,
) {
    if busy.replace(true) {
        return;
    }
    let original = input.text();
    let code = normalize_code(&original);
    if code != original {
        input.set_text(&code);
        input.set_position(-1);
    }
    busy.set(false);
    pair.set_sensitive(can_pair(&code, nearby, selected(picker, nearby)));
}

impl Panel {
    pub fn new(
        target: &EventLoopWindowTarget<AppEvent>,
        proxy: EventLoopProxy<AppEvent>,
    ) -> Result<Self> {
        let window = WindowBuilder::new()
            .with_title("添加设备")
            .with_resizable(false)
            .with_minimizable(false)
            .with_maximizable(false)
            .with_visible(false)
            .build(target)?;
        window.gtk_window().set_icon_name(Some("clipx"));
        window.gtk_window().set_skip_taskbar_hint(true);
        let root = window.default_vbox().expect("Tao 创建默认容器").clone();
        root.set_size_request(PANEL_WIDTH as i32, -1);
        Ok(Self {
            window,
            root,
            proxy,
            input: None,
            picker: None,
            pair: None,
            generate: None,
            code: None,
            remaining: None,
            error: label(""),
            submitting: Rc::new(Cell::new(false)),
            previous: None,
            visible: false,
        })
    }

    fn button(&self, title: &str, action: Action) -> gtk::Button {
        let button = gtk::Button::with_label(title);
        let proxy = self.proxy.clone();
        button.connect_clicked(move |_| {
            let _ = proxy.send_event(AppEvent::Action(action.clone()));
        });
        button
    }

    fn rebuild(&mut self, state: &PanelState) {
        for child in self.root.children() {
            self.root.remove(&child);
        }
        self.picker = None;
        self.input = None;
        self.pair = None;
        self.generate = None;
        self.code = None;
        self.remaining = None;
        self.submitting.set(true);
        let body = gtk::Box::new(Orientation::Vertical, 0);
        body.set_margin_start(24);
        body.set_margin_end(24);
        body.set_margin_top(24);
        body.set_margin_bottom(20);
        self.root.pack_start(&body, false, false, 0);
        let footer = gtk::Box::new(Orientation::Horizontal, 8);
        footer.set_margin_start(24);
        footer.set_margin_end(24);
        footer.set_margin_top(16);
        footer.set_margin_bottom(16);

        if state.page == Page::Waiting {
            body.pack_start(&label("本机配对码"), false, false, 0);
            let hint = label("在另一台设备上输入此码。");
            hint.set_margin_top(8);
            body.pack_start(&hint, false, false, 0);
            let code = label("");
            code.set_xalign(0.5);
            code.set_selectable(true);
            code.set_margin_top(24);
            let attrs = gtk::pango::AttrList::new();
            attrs.insert(gtk::pango::AttrSize::new(30 * gtk::pango::SCALE));
            code.set_attributes(Some(&attrs));
            body.pack_start(&code, false, false, 0);
            self.code = Some(code);
            let remaining = label("");
            remaining.set_xalign(0.5);
            remaining.set_margin_top(8);
            remaining.style_context().add_class("dim-label");
            body.pack_start(&remaining, false, false, 0);
            self.remaining = Some(remaining);
            let note = label("关闭窗口后仍可配对。");
            note.set_margin_top(20);
            note.style_context().add_class("dim-label");
            body.pack_start(&note, false, false, 0);
            footer.pack_start(&self.button("取消配对", Action::Cancel), false, false, 0);
        } else {
            let hint = label("输入另一台设备上显示的配对码。");
            hint.set_margin_bottom(20);
            body.pack_start(&hint, false, false, 0);
            let nearby: Rc<Vec<_>> = Rc::new(
                state
                    .snapshot
                    .nearby
                    .iter()
                    .map(|device| device.id)
                    .collect(),
            );
            if nearby.len() > 1 {
                let title = label("设备");
                title.set_margin_bottom(8);
                body.pack_start(&title, false, false, 0);
                let picker = gtk::ComboBoxText::new();
                picker.append(None, "选择设备");
                for device in &state.snapshot.nearby {
                    picker.append(Some(&hex::encode(device.id)), &state.nearby_name(device));
                }
                for cell in picker.cells() {
                    if let Ok(cell) = cell.downcast::<gtk::CellRendererText>() {
                        cell.set_ellipsize(gtk::pango::EllipsizeMode::End);
                        cell.set_max_width_chars(24);
                    }
                }
                picker.set_active(Some(0));
                picker.set_margin_bottom(20);
                body.pack_start(&picker, false, false, 0);
                self.picker = Some(picker);
            }
            let title = label("配对码");
            title.set_margin_bottom(8);
            body.pack_start(&title, false, false, 0);
            let input = gtk::Entry::new();
            input.set_placeholder_text(Some("6 位数字"));
            input.set_input_purpose(gtk::InputPurpose::Digits);
            input.set_activates_default(true);
            body.pack_start(&input, false, false, 0);
            let pair = gtk::Button::with_label("配对");
            pair.set_can_default(true);
            pair.set_sensitive(false);
            pair.set_size_request(64, -1);
            footer.pack_end(&pair, false, false, 0);
            {
                let input = input.downgrade();
                let picker = self.picker.as_ref().map(|p| p.downgrade());
                let nearby = nearby.clone();
                let busy = self.submitting.clone();
                let proxy = self.proxy.clone();
                pair.connect_clicked(move |pair| {
                    if busy.get() {
                        return;
                    }
                    let Some(input) = input.upgrade() else { return };
                    let picker = picker.as_ref().and_then(|p| p.upgrade());
                    let code = normalize_code(&input.text());
                    let device = selected(picker.as_ref(), &nearby);
                    if can_pair(&code, &nearby, device) {
                        busy.set(true);
                        pair.set_sensitive(false);
                        let _ = proxy.send_event(AppEvent::Action(Action::Join(code, device)));
                    }
                });
            }
            if let Some(picker) = &self.picker {
                let input = input.downgrade();
                let pair = pair.downgrade();
                let nearby = nearby.clone();
                let busy = self.submitting.clone();
                picker.connect_changed(move |picker| {
                    if let (Some(input), Some(pair)) = (input.upgrade(), pair.upgrade()) {
                        changed(&input, Some(picker), &pair, &nearby, &busy);
                    }
                });
            }
            {
                let picker = self.picker.as_ref().map(|p| p.downgrade());
                let pair = pair.downgrade();
                let busy = self.submitting.clone();
                input.connect_changed(move |input| {
                    if let Some(pair) = pair.upgrade() {
                        let picker = picker.as_ref().and_then(|p| p.upgrade());
                        changed(input, picker.as_ref(), &pair, &nearby, &busy);
                    }
                });
            }
            self.input = Some(input);
            self.pair = Some(pair);
            let generate = self.button("本机配对码…", Action::Generate);
            footer.pack_start(&generate, false, false, 0);
            self.generate = Some(generate);
        }
        let close = self.button("关闭", Action::Hide);
        close.set_size_request(64, -1);
        footer.pack_end(&close, false, false, 0);
        self.error = label("");
        self.error.set_no_show_all(true);
        self.error.set_margin_top(16);
        body.pack_start(&self.error, false, false, 0);
        self.root.pack_start(
            &gtk::Separator::new(Orientation::Horizontal),
            false,
            false,
            0,
        );
        self.root.pack_start(&footer, false, false, 0);
        self.root.show_all();
        if let Some(pair) = &self.pair {
            pair.grab_default();
        }
    }

    pub fn update(&mut self, state: &PanelState) {
        let rebuild = self.previous.as_ref().is_none_or(|old| {
            old.page != state.page
                || (state.page == Page::Pair && old.snapshot.nearby != state.snapshot.nearby)
        });
        if rebuild {
            let keep_input = !state.snapshot.paired
                && self
                    .previous
                    .as_ref()
                    .is_some_and(|old| old.page == Page::Pair && state.page == Page::Pair);
            let input = self.input.as_ref().map(|input| input.text());
            let selected = self.picker.as_ref().and_then(|picker| picker.active_id());
            self.rebuild(state);
            if keep_input {
                if let (Some(text), Some(input)) = (input, &self.input) {
                    input.set_text(&text);
                }
                if let (Some(selected), Some(picker)) = (selected.as_deref(), &self.picker) {
                    picker.set_active_id(Some(selected));
                }
            }
            if self.visible
                && let Some(input) = &self.input
            {
                input.grab_focus();
            }
        }
        self.submitting.set(state.snapshot.joining.is_some());
        if let Some(input) = &self.input {
            input.set_sensitive(state.snapshot.joining.is_none());
            if state.snapshot.paired {
                input.set_text("");
            }
            if let Some(pair) = &self.pair {
                let nearby: Vec<_> = state.snapshot.nearby.iter().map(|p| p.id).collect();
                pair.set_sensitive(
                    state.snapshot.joining.is_none()
                        && can_pair(
                            &input.text(),
                            &nearby,
                            selected(self.picker.as_ref(), &nearby),
                        ),
                );
            }
        }
        if let Some(picker) = &self.picker {
            picker.set_sensitive(state.snapshot.joining.is_none());
        }
        if let Some(generate) = &self.generate {
            generate.set_sensitive(state.snapshot.joining.is_none());
        }
        if let Some(code) = &self.code {
            code.set_text(&state.code_label());
        }
        if let Some(remaining) = &self.remaining {
            remaining.set_text(&state.remaining_label());
        }
        self.error.set_text(state.message().unwrap_or(""));
        self.error.set_visible(state.message().is_some());
        if state.snapshot.error.is_some() {
            self.error.style_context().add_class("error");
        } else {
            self.error.style_context().remove_class("error");
        }
        if rebuild
            || self
                .previous
                .as_ref()
                .is_some_and(|old| old.message() != state.message())
        {
            self.window.gtk_window().resize(PANEL_WIDTH as i32, 1);
        }
        self.previous = Some(state.clone());
    }

    pub fn show(&mut self) {
        if !self.visible {
            self.window
                .gtk_window()
                .set_position(gtk::WindowPosition::Center);
        }
        self.window.set_visible(true);
        self.window.set_focus();
        self.visible = true;
        if let Some(input) = &self.input {
            input.grab_focus();
        }
    }

    pub fn hide(&mut self) {
        self.window.set_visible(false);
        self.visible = false;
    }

    pub fn confirm_remove(&self, name: &str) -> bool {
        let dialog = gtk::MessageDialog::new(
            Some(self.window.gtk_window()),
            gtk::DialogFlags::MODAL,
            gtk::MessageType::Question,
            gtk::ButtonsType::None,
            &format!("移除“{name}”？"),
        );
        dialog.set_secondary_text(Some("将解除本机配对。再次连接需要重新配对。"));
        dialog.add_button("取消", gtk::ResponseType::Cancel);
        dialog.add_button("移除", gtk::ResponseType::Accept);
        dialog.set_default_response(gtk::ResponseType::Cancel);
        let confirmed = dialog.run() == gtk::ResponseType::Accept;
        dialog.close();
        confirmed
    }
}

pub fn show_error(message: &str) {
    if gtk::init().is_err() {
        return;
    }
    let dialog = gtk::MessageDialog::new(
        gtk::Window::NONE,
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Error,
        gtk::ButtonsType::Close,
        "无法启动 clipx",
    );
    dialog.set_secondary_text(Some(message));
    dialog.run();
    dialog.close();
}
