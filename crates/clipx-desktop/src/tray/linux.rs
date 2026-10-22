use anyhow::{Context, Result, ensure};
use gtk::{glib, glib::translate::*, prelude::*};
use libappindicator_sys::{
    AppIndicatorCategory_APP_INDICATOR_CATEGORY_APPLICATION_STATUS,
    AppIndicatorStatus_APP_INDICATOR_STATUS_ACTIVE,
    AppIndicatorStatus_APP_INDICATOR_STATUS_PASSIVE, app_indicator_new_with_path,
    app_indicator_set_menu, app_indicator_set_status, app_indicator_set_title,
};
use tray_icon::menu::{ContextMenu, Menu};

const ICON_THEME: &str = "[Icon Theme]
Name=clipx
Directories=scalable/status

[scalable/status]
Size=24
Type=Scalable
MinSize=16
MaxSize=256
Context=Status
";

pub struct Indicator(glib::Object);

impl Indicator {
    pub fn new(menu: &Menu) -> Result<Self> {
        // The panel runs in another process and needs a file it can resolve by name.
        let directory = glib::user_cache_dir().join("clipx/icons");
        let theme = directory.join("hicolor");
        let icons = theme.join("scalable/status");
        std::fs::create_dir_all(&icons).context("创建托盘图标缓存目录失败")?;
        std::fs::write(theme.join("index.theme"), ICON_THEME).context("写入托盘图标主题失败")?;
        std::fs::write(
            icons.join("clipx-symbolic.svg"),
            include_bytes!("../../../../assets/clipx-symbolic.svg"),
        )
        .context("写入托盘图标失败")?;

        // AppIndicator returns an owned GObject; glib releases it on drop.
        // Construction and menu access both occur on the GTK thread.
        unsafe {
            let indicator = app_indicator_new_with_path(
                c"clipx".as_ptr(),
                c"clipx-symbolic".as_ptr(),
                AppIndicatorCategory_APP_INDICATOR_CATEGORY_APPLICATION_STATUS,
                directory.to_glib_none().0,
            );
            ensure!(!indicator.is_null(), "创建托盘图标失败");
            let object = from_glib_full(indicator.cast::<glib::gobject_ffi::GObject>());
            app_indicator_set_title(indicator, c"clipx".as_ptr());
            app_indicator_set_menu(indicator, menu.gtk_context_menu().to_glib_none().0);
            app_indicator_set_status(indicator, AppIndicatorStatus_APP_INDICATOR_STATUS_ACTIVE);
            Ok(Self(object))
        }
    }
}

impl Drop for Indicator {
    fn drop(&mut self) {
        // The owned GObject is still alive here, on the GTK thread.
        unsafe {
            app_indicator_set_status(
                self.0.as_ptr().cast(),
                AppIndicatorStatus_APP_INDICATOR_STATUS_PASSIVE,
            );
        }
    }
}
