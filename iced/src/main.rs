use std::{cell::RefCell, env, ffi::OsStr};

use iced::{application, window};

mod app;
mod message;
mod startup;
mod state;
mod theme;
mod utils;
mod widgets;

use app::RingboardApp;

#[cfg(feature = "trace")]
#[global_allocator]
static GLOBAL: tracy_client::ProfiledAllocator<std::alloc::System> =
    tracy_client::ProfiledAllocator::new(std::alloc::System, 100);

fn main() -> iced::Result {
    let daemon = env::var_os("RINGBOARD_NO_DAEMON").is_none();
    let startup_token = if daemon && env::args_os().nth(1).as_deref() == Some(OsStr::new("toggle"))
    {
        startup::maybe_open_existing_instance_and_exit()
            .inspect_err(|e| {
                eprintln!("Failed to check for existing instance: {e}\nDetails: {e:#?}");
            })
            .ok()
    } else {
        None
    };

    let startup_token = RefCell::new(Some(startup_token));
    let result = application(
        move || RingboardApp::boot(startup_token.borrow_mut().take().flatten()),
        RingboardApp::update,
        RingboardApp::view,
    )
    .title(|app: &RingboardApp| app.title())
    .subscription(RingboardApp::subscription)
    .theme(|app: &RingboardApp| app.state.theme.theme.clone())
    .exit_on_close_request(!daemon)
    // Fixed size, not adapted to the monitor: a prior version resized the
    // window responsively after it opened, but a programmatic
    // `window::resize` post-launch turned out to corrupt text rendering in
    // this iced version (confirmed by testing, not a guess), so instead the
    // window opens once at a reasonable middle-of-the-range size and stays
    // there — `min_size`/`max_size` still bound how far the user can drag
    // it manually, which doesn't hit the same bug.
    .window(window::Settings {
        size: app::WINDOW_DEFAULT_SIZE,
        min_size: Some(app::WINDOW_MIN_SIZE),
        max_size: Some(app::WINDOW_MAX_SIZE),
        // Left unset, this comes out as an empty WM_CLASS/app_id on both
        // X11 and Wayland (iced_winit passes it through verbatim, with no
        // fallback to the executable name), which breaks window-manager/
        // taskbar association with a .desktop entry's StartupWMClass.
        platform_specific: window::settings::PlatformSpecific {
            application_id: "ringboard-iced".into(),
            ..window::settings::PlatformSpecific::default()
        },
        ..window::Settings::default()
    })
    .centered()
    .run();

    if daemon {
        startup::cleanup();
    }
    result
}
