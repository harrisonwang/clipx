use crate::app::{Action, AppEvent, PANEL_WIDTH, PanelState};
use anyhow::Result;
use clipx_core::DeviceId;
use gtk::{Align, Orientation, prelude::*};
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
    error: gtk::Label,
    submitting: Rc<Cell<bool>>,
    previous: Option<PanelState>,
    visible: bool,
}

fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_line_wrap(true);
    label.set_max_width_chars(24);
    label
}

fn submit(
    input: &gtk::Entry,
    picker: Option<&gtk::ComboBoxText>,
    nearby: &[DeviceId],
    busy: &Cell<bool>,
    proxy: &EventLoopProxy<AppEvent>,
) {
    if busy.replace(true) {
        return;
    }
    let original = input.text();
    let code: String = original
        .chars()
        .filter(char::is_ascii_digit)
        .take(6)
        .collect();
    if code != original {
        input.set_text(&code);
        input.set_position(-1);
    }
    busy.set(false);
    if code.len() != 6 {
        return;
    }
    let device = if nearby.len() == 1 {
        nearby.first().copied()
    } else {
        picker
            .and_then(|picker| picker.active())
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| nearby.get(index as usize).copied())
    };
    busy.set(true);
    let _ = proxy.send_event(AppEvent::Action(Action::Join(code, device)));
}

impl Panel {
    pub fn new(
        target: &EventLoopWindowTarget<AppEvent>,
        proxy: EventLoopProxy<AppEvent>,
    ) -> Result<Self> {
        let window = WindowBuilder::new()
            .with_title("输入配对码")
            .with_resizable(false)
            .with_visible(false)
            .build(target)?;
        window.gtk_window().set_icon_name(Some("clipx"));
        window.gtk_window().set_skip_taskbar_hint(true);
        let root = window.default_vbox().expect("Tao 创建默认容器").clone();
        root.set_spacing(12);
        root.set_margin_start(16);
        root.set_margin_end(16);
        root.set_margin_top(16);
        root.set_margin_bottom(16);
        root.set_size_request(PANEL_WIDTH as i32 - 32, -1);
        Ok(Self {
            window,
            root,
            proxy,
            input: None,
            picker: None,
            error: label(""),
            submitting: Rc::new(Cell::new(false)),
            previous: None,
            visible: false,
        })
    }

    fn rebuild(&mut self, state: &PanelState) {
        for child in self.root.children() {
            self.root.remove(&child);
        }
        self.picker = None;
        self.submitting.set(false);
        let hint = label("输入另一台设备生成的 6 位配对码。");
        hint.style_context().add_class("dim-label");
        self.root.pack_start(&hint, false, false, 0);
        let input = gtk::Entry::new();
        input.set_placeholder_text(Some("输入 6 位配对码"));
        gtk::prelude::EntryExt::set_alignment(&input, 0.5);
        input.set_input_purpose(gtk::InputPurpose::Digits);
        input.set_width_chars(12);
        let attrs = gtk::pango::AttrList::new();
        attrs.insert(gtk::pango::AttrSize::new(16 * gtk::pango::SCALE));
        input.set_attributes(&attrs);
        self.root.pack_start(&input, false, false, 0);
        let nearby: Vec<_> = state
            .snapshot
            .nearby
            .iter()
            .map(|device| device.id)
            .collect();
        if nearby.len() > 1 {
            let picker = gtk::ComboBoxText::new();
            picker.append(None, "选择设备");
            for device in &state.snapshot.nearby {
                picker.append(Some(&hex::encode(device.id)), &state.nearby_name(device));
            }
            // Long device names must not widen the pairing window.
            for cell in picker.cells() {
                if let Ok(cell) = cell.downcast::<gtk::CellRendererText>() {
                    cell.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    cell.set_max_width_chars(18);
                }
            }
            picker.set_active(Some(0));
            self.root.pack_start(&picker, false, false, 0);
            let input = input.downgrade();
            let peers = nearby.clone();
            let busy = self.submitting.clone();
            let proxy = self.proxy.clone();
            picker.connect_changed(move |picker| {
                if let Some(input) = input.upgrade() {
                    submit(&input, Some(picker), &peers, &busy, &proxy);
                }
            });
            self.picker = Some(picker);
        }
        let picker = self.picker.clone();
        let busy = self.submitting.clone();
        let proxy = self.proxy.clone();
        input.connect_changed(move |input| submit(input, picker.as_ref(), &nearby, &busy, &proxy));
        self.input = Some(input);
        self.error = label("");
        self.error.set_no_show_all(true);
        self.root.pack_start(&self.error, false, false, 0);
        let footer = gtk::Box::new(Orientation::Horizontal, 0);
        let cancel = gtk::Button::with_label("取消");
        cancel.set_halign(Align::End);
        let proxy = self.proxy.clone();
        cancel.connect_clicked(move |_| {
            let _ = proxy.send_event(AppEvent::Action(Action::Hide));
        });
        footer.pack_end(&cancel, false, false, 0);
        self.root.pack_start(&footer, false, false, 0);
        self.root.show_all();
    }

    pub fn update(&mut self, state: &PanelState) {
        let rebuild = self
            .previous
            .as_ref()
            .is_none_or(|old| old.snapshot.nearby != state.snapshot.nearby);
        if rebuild {
            let input = self.input.as_ref().map(|input| input.text());
            let selected = self.picker.as_ref().and_then(|picker| picker.active_id());
            self.rebuild(state);
            // Restoring controls must not submit a second pairing attempt.
            self.submitting.set(true);
            if self.previous.is_some() {
                if let (Some(text), Some(input)) = (input, &self.input) {
                    input.set_text(&text);
                }
                if let (Some(picker), Some(selected)) = (&self.picker, selected.as_deref()) {
                    picker.set_active_id(Some(selected));
                }
            }
            if self.visible
                && let Some(input) = &self.input
            {
                input.grab_focus();
            }
        }
        self.submitting.set(state.snapshot.joining);
        if let Some(input) = &self.input {
            input.set_sensitive(!state.snapshot.joining);
            if self
                .previous
                .as_ref()
                .is_some_and(|old| old.snapshot.joining)
                && !state.snapshot.joining
                && state.snapshot.error.is_some()
            {
                input.grab_focus();
                input.select_region(0, -1);
            }
        }
        if let Some(picker) = &self.picker {
            picker.set_sensitive(!state.snapshot.joining);
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
                .is_some_and(|old| old.message().is_some() != state.message().is_some())
        {
            self.window.gtk_window().resize(PANEL_WIDTH as i32, 1);
        }
        self.previous = Some(state.clone());
    }

    pub fn show(&mut self) {
        if self.visible {
            return;
        }
        super::center_window(&self.window);
        self.window.set_visible(true);
        self.window.set_focus();
        self.visible = true;
        if let Some(input) = &self.input {
            input.grab_focus();
        }
    }

    pub fn hide(&mut self) {
        if self.visible {
            self.window.set_visible(false);
            self.visible = false;
            self.previous = None;
        }
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
