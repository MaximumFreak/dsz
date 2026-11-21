//! Set the native app icon at runtime.
//!
//! TODO(emilk): port this to [`winit`].

use std::sync::Arc;

use egui::IconData;

pub struct AppTitleIconSetter {
    title: String,
    icon_data: Option<Arc<IconData>>,
    status: AppIconStatus,
}

impl AppTitleIconSetter {
    pub fn new(title: String, mut icon_data: Option<Arc<IconData>>) -> Self {
        if let Some(icon) = &icon_data {
            if **icon == IconData::default() {
                icon_data = None;
            }
        }

        Self {
            title,
            icon_data,
            status: AppIconStatus::NotSetTryAgain,
        }
    }

    /// Call once per frame; we will set the icon when we can.
    pub fn update(&mut self) {
        if self.status == AppIconStatus::NotSetTryAgain {
            self.status = set_title_and_icon(&self.title, self.icon_data.as_deref());
        }
    }
}

/// In which state the app icon is (as far as we know).
#[derive(PartialEq, Eq)]
enum AppIconStatus {
    /// We did not set it or failed to do it. In any case we won't try again.
    NotSetIgnored,

    /// We haven't set the icon yet, we should try again next frame.
    ///
    /// This can happen repeatedly due to lazy window creation on some platforms.
    NotSetTryAgain,

    /// We successfully set the icon and it should be visible now.
    #[allow(dead_code, clippy::allow_attributes)] // Not used on Linux
    Set,
}

/// Sets app icon at runtime.
///
/// By setting the icon at runtime and not via resource files etc. we ensure that we'll get the chance
/// to set the same icon when the process/window is started from python (which sets its own icon ahead of us!).
///
/// Since window creation can be lazy, call this every frame until it's either successfully or gave up.
/// (See [`AppIconStatus`])
fn set_title_and_icon(_title: &str, _icon_data: Option<&IconData>) -> AppIconStatus {
    profiling::function_scope!();

    #[cfg(target_os = "windows")]
    {
        return set_app_icon_windows(_icon_data);
    }

    #[cfg(target_os = "macos")]
    return set_title_and_icon_mac(_title, _icon_data);

    #[allow(unreachable_code, clippy::allow_attributes)]
    AppIconStatus::NotSetIgnored
}

/// Set icon for Windows applications.
///
/// Loads the embedded multi-size `.ico` resource (ID 1) directly via `LoadImageW`,
/// falling back to a 32-bit DIB icon from `icon_data` if no resource is embedded.
#[cfg(target_os = "windows")]
#[expect(unsafe_code)]
fn set_app_icon_windows(icon_data: Option<&IconData>) -> AppIconStatus {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateIconFromResourceEx, GetSystemMetrics, LoadImageW, SendMessageW, HICON, ICON_BIG,
        ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXICON, SM_CXSMICON, WM_SETICON,
    };

    // SAFETY: Win32 query with no side effects.
    let window_handle = unsafe { GetActiveWindow() };
    if window_handle.is_null() {
        return AppIconStatus::NotSetTryAgain;
    }

    fn load_or_make_hicon(icon_data: Option<&IconData>, target_size: i32) -> HICON {
        // SAFETY: Loads icon resource ID 1 from the current module if present.
        let res = unsafe {
            LoadImageW(
                GetModuleHandleW(std::ptr::null()),
                1 as *const u16,
                IMAGE_ICON,
                target_size,
                target_size,
                LR_DEFAULTCOLOR,
            )
        };
        if !res.is_null() {
            return res as HICON;
        }
        let Some(icon) = icon_data else {
            return std::ptr::null_mut();
        };
        let (w, h) = (icon.width as usize, icon.height as usize);
        if w == 0 || h == 0 || icon.rgba.len() != w * h * 4 {
            return std::ptr::null_mut();
        }
        let mask_row = w.div_ceil(32) * 4;
        let mut dib = Vec::with_capacity(40 + w * h * 4 + mask_row * h);
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&(w as i32).to_le_bytes());
        dib.extend_from_slice(&((2 * h) as i32).to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&[0u8; 24]);
        for y in (0..h).rev() {
            for x in 0..w {
                let i = (y * w + x) * 4;
                dib.extend_from_slice(&[
                    icon.rgba[i + 2],
                    icon.rgba[i + 1],
                    icon.rgba[i],
                    icon.rgba[i + 3],
                ]);
            }
        }
        dib.resize(dib.len() + mask_row * h, 0);
        // SAFETY: `dib` is a valid 32-bit BI_RGB DIB icon buffer for the call's duration.
        unsafe {
            CreateIconFromResourceEx(
                dib.as_ptr(),
                dib.len() as u32,
                1,
                0x00030000,
                target_size,
                target_size,
                LR_DEFAULTCOLOR,
            )
        }
    }

    // SAFETY: Win32 metric queries with no side effects.
    let icon_size_big = unsafe { GetSystemMetrics(SM_CXICON) };
    let icon_big = load_or_make_hicon(icon_data, icon_size_big);
    if !icon_big.is_null() {
        // SAFETY: Valid window handle and HICON.
        unsafe {
            SendMessageW(window_handle, WM_SETICON, ICON_BIG as usize, icon_big as isize);
        }
    }

    // SAFETY: Win32 metric query with no side effects.
    let icon_size_small = unsafe { GetSystemMetrics(SM_CXSMICON) };
    let icon_small = load_or_make_hicon(icon_data, icon_size_small);
    if !icon_small.is_null() {
        // SAFETY: Valid window handle and HICON.
        unsafe {
            SendMessageW(
                window_handle,
                WM_SETICON,
                ICON_SMALL as usize,
                icon_small as isize,
            );
        }
    }

    if icon_big.is_null() && icon_small.is_null() {
        AppIconStatus::NotSetIgnored
    } else {
        AppIconStatus::Set
    }
}

/// Set icon & app title for `MacOS` applications.
#[cfg(target_os = "macos")]
#[expect(unsafe_code)]
fn set_title_and_icon_mac(title: &str, icon_data: Option<&IconData>) -> AppIconStatus {
    use crate::icon_data::IconDataExt as _;
    profiling::function_scope!();

    use objc2::ClassType as _;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSString;

    // Do NOT use png even though creating `NSImage` from it is much easier than from raw images data!
    //
    // Some MacOS versions have a bug where creating an `NSImage` from a png will cause it to load an arbitrary `libpng.dylib`.
    // If this dylib isn't the right version, the application will crash with SIGBUS.
    // For details see https://github.com/emilk/egui/issues/7155
    let image = if let Some(icon_data) = icon_data {
        match icon_data.to_image() {
            Ok(image) => Some(image),
            Err(err) => {
                log::warn!("Failed to read icon data: {err}");
                return AppIconStatus::NotSetIgnored;
            }
        }
    } else {
        None
    };

    // TODO(madsmtm): Move this into `objc2-app-kit`
    unsafe extern "C" {
        static NSApp: Option<&'static NSApplication>;
    }

    // SAFETY: we don't do anything dangerous here
    unsafe {
        let Some(app) = NSApp else {
            log::debug!("NSApp is null");
            return AppIconStatus::NotSetIgnored;
        };

        if let Some(image) = image {
            use objc2_app_kit::{NSBitmapImageRep, NSDeviceRGBColorSpace};
            use objc2_foundation::NSSize;

            log::trace!(
                "NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel"
            );
            let Some(image_rep) = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                [image.as_raw().as_ptr().cast_mut()].as_mut_ptr(),
                image.width() as isize,
                image.height() as isize,
                8, // bits per sample
                4, // samples per pixel
                true, // has alpha
                false, // is not planar
                NSDeviceRGBColorSpace,
                (image.width() * 4) as isize, // bytes per row
                32 // bits per pixel
            ) else {
                log::warn!("Failed to create NSBitmapImageRep from app icon data.");
                return AppIconStatus::NotSetIgnored;
            };

            log::trace!("NSImage::initWithSize");
            let app_icon = NSImage::initWithSize(
                NSImage::alloc(),
                NSSize::new(image.width() as f64, image.height() as f64),
            );
            log::trace!("NSImage::addRepresentation");
            app_icon.addRepresentation(&image_rep);

            profiling::scope!("setApplicationIconImage_");
            log::trace!("setApplicationIconImage…");
            app.setApplicationIconImage(Some(&app_icon));
        }

        // Change the title in the top bar - for python processes this would be again "python" otherwise.
        if let Some(main_menu) = app.mainMenu() {
            if let Some(item) = main_menu.itemAtIndex(0) {
                if let Some(app_menu) = item.submenu() {
                    profiling::scope!("setTitle_");
                    app_menu.setTitle(&NSString::from_str(title));
                }
            }
        }

        // The title in the Dock apparently can't be changed.
        // At least these people didn't figure it out either:
        // https://stackoverflow.com/questions/69831167/qt-change-application-title-dynamically-on-macos
        // https://stackoverflow.com/questions/28808226/changing-cocoa-app-icon-title-and-menu-labels-at-runtime
    }

    AppIconStatus::Set
}
