//! kiosk-kit: what a pikiosk app needs to draw on the kiosk display.
//!
//! An app implements [`App`] and hands it to [`run`]; kiosk-kit opens the
//! display (DRM/KMS + GBM + EGL/GLES2), reads touch through libinput and
//! applies the touch calibration. The glass look comes from [`glass`] and
//! [`widgets`].

pub mod calibrate;
pub mod glass;
pub mod layout;
pub mod widgets;

#[cfg(target_os = "linux")]
pub mod gfx;
#[cfg(target_os = "linux")]
pub mod kms;

pub use calibrate::TouchRequest;
pub use layout::Lay;

#[cfg(target_os = "linux")]
pub use kms::run;

/// An app that kiosk-kit can run on the display.
pub trait App {
    /// Draws one frame.
    fn ui(&mut self, ui: &mut egui::Ui);

    /// The touch calibration to apply at startup.
    fn touch_calibration(&self) -> TouchRequest {
        TouchRequest::Default
    }

    /// A new touch calibration for the backend to apply, if the app asked for one.
    fn take_touch_request(&mut self) -> Option<TouchRequest> {
        None
    }

    /// Tells the app where a mouse pointer is, or `None` when none is in use.
    fn set_cursor(&mut self, _pos: Option<egui::Pos2>) {}
}
