// No console window.
#![windows_subsystem = "windows"]

mod island;
mod modules;
mod settings;
mod shell;
mod theme;
mod tray;
mod welcome;

use island::{Activity, Island};

fn main() {
    // One island is enough.
    if !single_instance() {
        return;
    }
    let result = shell::window::run(|settings, hwnd| {
        let (events, inbox) = std::sync::mpsc::channel();
        let post = island::Post { tx: events, hwnd, message: shell::window::WM_EVENT };
        let weather = modules::weather::spawn(post.clone());
        modules::calendar::spawn_holidays(post.clone(), settings.region.clone());
        modules::status::watch_privacy(post.clone());
        let bluetooth = modules::battery::watch_bluetooth(post.clone());
        let notify = if settings.notifications {
            modules::notify::spawn(post.clone())
        } else {
            modules::notify::MODE.store(4, std::sync::atomic::Ordering::Relaxed);
            std::sync::mpsc::channel().0
        };
        modules::status::watch_keyboard(hwnd);
        shell::ole::accept_drops(windows::Win32::Foundation::HWND(hwnd as *mut _), post.clone());
        let mut keep: Vec<Box<dyn std::any::Any>> = Vec::new();
        let volume = modules::status::watch_volume(post.clone());
        if let Some(devices) = modules::status::watch_audio_device(post.clone()) {
            keep.push(Box::new(devices));
        }
        modules::media::COVER_BLUR.store(settings.cover_blur.min(3), std::sync::atomic::Ordering::Relaxed);
        let (media_states, media_commands) = modules::media::spawn(hwnd, shell::window::WM_MEDIA);
        // Order is the order of the mode dots.
        let activities: Vec<Box<dyn Activity>> = vec![
            Box::new(modules::media::Media::new(media_commands)),
            Box::new(modules::calendar::Calendar::new()),
            Box::new(modules::weather::Weather::new()),
            Box::new(modules::timers::Timers::new()),
            Box::new(modules::shelf::Shelf::new(post.clone())),
            Box::new(modules::battery::Battery::new(bluetooth)),
            Box::new(modules::call::Call::new()),
            Box::new(modules::hello::Hello),
            Box::new(modules::status::Status::new()),
            Box::new(modules::notify::Notifications::new()),
        ];
        let mut island = Island::new(activities);
        island.select(if settings.last_mode.is_empty() { "calendar" } else { &settings.last_mode });
        (island, shell::window::Inbox { media: media_states, events: inbox, weather, notify, post: post.clone(), volume, keep })
    });
    if let Err(err) = result {
        // No console: leave a note where it can be found.
        if let Some(dir) = settings::data_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("last-error.txt"), format!("{err:?}"));
        }
    }
}

fn single_instance() -> bool {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    // SAFETY: creates a named mutex that lives until the process exits.
    unsafe {
        let mutex = CreateMutexW(None, true, w!("Local\\GlideIslandSingleInstance"));
        let already = GetLastError() == ERROR_ALREADY_EXISTS;
        std::mem::forget(mutex);
        if already {
            shell::window::show_existing();
            false
        } else {
            true
        }
    }
}
