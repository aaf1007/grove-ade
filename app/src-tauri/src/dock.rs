//! The Dock icon while grove runs (#55). The UI picks the icon. This only shows it.

use serde::Deserialize;
use tauri::AppHandle;

/// A Dock icon grove bundles, named as the UI names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DockIcon {
    MonoDark,
    MonoLight,
    BlueDark,
    BlueLight,
}

impl DockIcon {
    /// The icon as a 1024 px PNG.
    fn png(self) -> &'static [u8] {
        match self {
            DockIcon::MonoDark => include_bytes!("../icons/dock/grove-mono-dark.png"),
            DockIcon::MonoLight => include_bytes!("../icons/dock/grove-mono-light.png"),
            DockIcon::BlueDark => include_bytes!("../icons/dock/grove-blue-dark.png"),
            DockIcon::BlueLight => include_bytes!("../icons/dock/grove-blue-light.png"),
        }
    }
}

/// Shows `icon` in the Dock until grove quits. AppKit only runs on the main thread.
#[tauri::command]
pub async fn set_dock_icon(app: AppHandle, icon: DockIcon) -> Result<(), String> {
    let (done, shown) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = done.send(show(icon.png()));
    })
    .map_err(|e| e.to_string())?;
    shown.await.map_err(|e| e.to_string())?
}

#[cfg(target_os = "macos")]
fn show(png: &[u8]) -> Result<(), String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let main_thread =
        MainThreadMarker::new().ok_or("the Dock icon can only be set on the main thread")?;
    let image = image(png).ok_or("macOS could not read the icon")?;
    // SAFETY: on the main thread, with an image, as AppKit asks.
    unsafe { NSApplication::sharedApplication(main_thread).setApplicationIconImage(Some(&image)) };
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn show(_png: &[u8]) -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn image(png: &[u8]) -> Option<objc2::rc::Retained<objc2_app_kit::NSImage>> {
    use objc2::AllocAnyThread;
    use objc2_app_kit::NSImage;
    use objc2_foundation::NSData;

    NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(png))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ICONS: [(&str, DockIcon); 4] = [
        ("mono-dark", DockIcon::MonoDark),
        ("mono-light", DockIcon::MonoLight),
        ("blue-dark", DockIcon::BlueDark),
        ("blue-light", DockIcon::BlueLight),
    ];

    #[test]
    fn each_name_the_ui_sends_is_a_1024_px_png() {
        for (name, icon) in ICONS {
            let parsed: DockIcon = serde_json::from_value(serde_json::json!(name)).expect(name);
            assert_eq!(parsed, icon);
            let png = icon.png();
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "{name}");
            // The IHDR chunk's width and height.
            assert_eq!(&png[16..24], &[0, 0, 4, 0, 0, 0, 4, 0], "{name}");
        }
    }

    #[test]
    fn an_unknown_name_is_refused() {
        assert!(serde_json::from_value::<DockIcon>(serde_json::json!("green-dark")).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reads_each_icon() {
        for (name, icon) in ICONS {
            assert!(image(icon.png()).is_some(), "{name}");
        }
    }
}
