//! The icon in the menu bar (macOS) or the notification area (Windows).
//! With "close to tray" on, closing the window hides BiWrite there, with
//! the document and its translations kept; the icon's menu shows the window
//! again or quits.

use std::time::Duration;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Manager, Runtime, Window, Wry};

use crate::files;
use crate::state::AppState;

/// 36 × 36 RGBA, black on transparent: a template image, which macOS
/// colours to suit the menu bar (drawn from `icons/tray-template.svg`).
#[cfg(target_os = "macos")]
const TEMPLATE: &[u8] = include_bytes!("../icons/tray-template.rgba");

pub struct Tray {
    icon: TrayIcon<Wry>,
    show: MenuItem<Wry>,
    quit: MenuItem<Wry>,
}

impl Tray {
    pub fn build(app: &App, visible: bool) -> tauri::Result<Self> {
        let show = MenuItem::with_id(app, "show", "Show BiWrite", true, None::<&str>)?;
        let quit = MenuItem::with_id(app, "quit", "Quit BiWrite", true, None::<&str>)?;
        let menu = Menu::with_items(app, &[&show, &quit])?;
        let builder = TrayIconBuilder::with_id("main")
            .tooltip("BiWrite")
            .menu(&menu)
            .on_menu_event(|app, event| match event.id().as_ref() {
                "show" => show_window(app),
                "quit" => quit_app(app),
                _ => {}
            })
            .on_tray_icon_event(|tray, event| {
                // Windows: a click shows the window; the menu is on the right
                // button. macOS shows the menu on a click.
                if cfg!(not(target_os = "macos"))
                    && let TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    } = event
                {
                    show_window(tray.app_handle());
                }
            });
        #[cfg(target_os = "macos")]
        let builder = builder
            .icon(tauri::image::Image::new_owned(TEMPLATE.to_vec(), 36, 36))
            .icon_as_template(true)
            .show_menu_on_left_click(true);
        #[cfg(not(target_os = "macos"))]
        let builder = match app.default_window_icon() {
            Some(icon) => builder.icon(icon.clone()),
            None => builder,
        }
        .show_menu_on_left_click(false);
        let icon = builder.build(app)?;
        icon.set_visible(visible)?;
        Ok(Self { icon, show, quit })
    }

    pub fn set_visible(&self, visible: bool) {
        if let Err(e) = self.icon.set_visible(visible) {
            log::warn!("failed to {} the tray icon: {e}", if visible { "show" } else { "hide" });
        }
    }

    /// The menu in the interface language.
    pub fn set_language(&self, chinese: bool) {
        let (show, quit) = if chinese {
            ("显示 BiWrite", "退出 BiWrite")
        } else {
            ("Show BiWrite", "Quit BiWrite")
        };
        for (item, text) in [(&self.show, show), (&self.quit, quit)] {
            if let Err(e) = item.set_text(text) {
                log::warn!("failed to label the tray menu: {e}");
            }
        }
    }
}

/// Bring the main window back (from the tray, the Dock or a second launch).
pub fn show_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Hide the window to the tray. A full-screen window leaves full screen
/// first: hidden in full screen, it would leave an empty space behind.
pub fn hide_window<R: Runtime>(window: &Window<R>) {
    if window.is_fullscreen().unwrap_or(false) {
        let _ = window.set_fullscreen(false);
        let window = window.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(900));
            if let Err(e) = window.hide() {
                log::warn!("failed to hide the window: {e}");
            }
        });
    } else if let Err(e) = window.hide() {
        log::warn!("failed to hide the window: {e}");
    }
}

/// Quit, asking first (with the window shown) about unsaved changes.
pub fn quit_app(app: &AppHandle) {
    if !app.state::<AppState>().needs_close_confirmation() {
        app.exit(0);
        return;
    }
    show_window(app);
    let handle = app.clone();
    let parent = app.get_webview_window("main").map(|w| w.as_ref().window());
    files::ask_discard(app, parent.as_ref(), move |discard| {
        if discard {
            handle.state::<AppState>().confirm_discard_on_close();
            handle.exit(0);
        }
    });
}
