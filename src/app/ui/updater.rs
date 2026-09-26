use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStatus {
    Downloading {
        version: String,
        downloaded: u64,
        total: u64,
    },
    Cancelled,
    Restarting,
    Failed(String),
}

fn open_release_page(parent: &gtk4::Window, tag: &str) {
    let uri = format!(
        "https://github.com/{}/releases/tag/{tag}",
        crate::app_meta::UPDATE_REPO
    );
    gtk4::UriLauncher::new(&uri).launch(Some(parent), None::<&gio::Cancellable>, |result| {
        if let Err(error) = result {
            log::warn!("Could not open the release page: {error}");
        }
    });
}

fn installer_command() -> &'static str {
    crate::app_meta::INSTALLER_COMMAND
}

fn progress_text(downloaded: u64, total: u64) -> String {
    let downloaded_mib = downloaded as f64 / 1024.0 / 1024.0;
    let total_mib = total as f64 / 1024.0 / 1024.0;
    format!("{downloaded_mib:.1} MiB / {total_mib:.1} MiB")
}

fn start_update_download(
    parent: &gtk4::Window,
    info: crate::update::UpdateInfo,
    on_status: Option<Rc<dyn Fn(InstallStatus) + 'static>>,
) {
    let version = info.metadata.version.to_string();
    let total = info.metadata.appimage.size;
    if let Some(callback) = on_status.as_ref() {
        callback(InstallStatus::Downloading {
            version: version.clone(),
            downloaded: 0,
            total,
        });
    }

    let progress_bar = gtk4::ProgressBar::builder()
        .show_text(true)
        .hexpand(true)
        .build();
    progress_bar.set_fraction(0.0);
    progress_bar.set_text(Some(&progress_text(0, total)));
    let progress_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(8)
        .build();
    progress_box.append(&progress_bar);

    let download_dialog = adw::AlertDialog::new(
        Some("Downloading update"),
        Some(&format!("{} {version}", crate::app_meta::APP_TITLE)),
    );
    download_dialog.set_extra_child(Some(&progress_box));
    download_dialog.add_response("cancel", "Cancel");
    download_dialog.set_close_response("cancel");

    let cancelled = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    {
        let cancelled = Arc::clone(&cancelled);
        let finished = Arc::clone(&finished);
        download_dialog
            .clone()
            .choose(parent, None::<&gio::Cancellable>, move |_| {
                if !finished.load(Ordering::Relaxed) {
                    cancelled.store(true, Ordering::Relaxed);
                }
            });
    }

    let (progress_sender, progress_receiver) = mpsc::channel::<crate::update::DownloadProgress>();
    let progress_source = Rc::new(RefCell::new(Some(glib::timeout_add_local(
        Duration::from_millis(50),
        {
            let progress_bar = progress_bar.clone();
            let version = version.clone();
            let on_status = on_status.clone();
            move || {
                while let Ok(progress) = progress_receiver.try_recv() {
                    let fraction = if progress.total == 0 {
                        0.0
                    } else {
                        progress.downloaded as f64 / progress.total as f64
                    };
                    progress_bar.set_fraction(fraction.clamp(0.0, 1.0));
                    progress_bar
                        .set_text(Some(&progress_text(progress.downloaded, progress.total)));
                    if let Some(callback) = on_status.as_ref() {
                        callback(InstallStatus::Downloading {
                            version: version.clone(),
                            downloaded: progress.downloaded,
                            total: progress.total,
                        });
                    }
                }
                glib::ControlFlow::Continue
            }
        },
    ))));

    let parent_done = parent.clone();
    let status_done = on_status.clone();
    let progress_source_done = Rc::clone(&progress_source);
    let dialog_done = download_dialog.clone();
    let finished_done = Arc::clone(&finished);
    let cancelled_worker = Arc::clone(&cancelled);
    let task = move || {
        crate::update::stage_controlled(&info, &cancelled_worker, move |progress| {
            let _ = progress_sender.send(progress);
        })
    };
    if let Err(error) =
        crate::commands::dispatch_async_result("stage_update", task, move |result| {
            finished_done.store(true, Ordering::Relaxed);
            if let Some(source) = progress_source_done.borrow_mut().take() {
                source.remove();
            }
            dialog_done.close();
            match result {
                Ok(path) => match crate::update::launch_staged_update(&path) {
                    Ok(()) => {
                        if let Some(callback) = status_done.as_ref() {
                            callback(InstallStatus::Restarting);
                        }
                        crate::ui_event_bridge::mark_quit_requested();
                        parent_done.close();
                    }
                    Err(error) => {
                        let message = format!("Verified update could not be started: {error}");
                        log::warn!("{message}");
                        if let Some(callback) = status_done.as_ref() {
                            callback(InstallStatus::Failed(message.clone()));
                        } else {
                            crate::ui_event_bridge::post_toast(message);
                        }
                    }
                },
                Err(crate::update::UpdateError::Cancelled) => {
                    if let Some(callback) = status_done.as_ref() {
                        callback(InstallStatus::Cancelled);
                    } else {
                        crate::ui_event_bridge::post_toast("Update download cancelled".to_string());
                    }
                }
                Err(error) => {
                    let message = format!("Update download or verification failed: {error}");
                    log::warn!("{message}");
                    if let Some(callback) = status_done.as_ref() {
                        callback(InstallStatus::Failed(message.clone()));
                    } else {
                        crate::ui_event_bridge::post_toast(message);
                    }
                }
            }
        })
    {
        finished.store(true, Ordering::Relaxed);
        if let Some(source) = progress_source.borrow_mut().take() {
            source.remove();
        }
        download_dialog.close();
        let message = format!("Could not start the update download: {error}");
        log::warn!("{message}");
        if let Some(callback) = on_status.as_ref() {
            callback(InstallStatus::Failed(message.clone()));
        } else {
            crate::ui_event_bridge::post_toast(message);
        }
    }
}

pub fn prompt_update(
    parent: &gtk4::Window,
    info: crate::update::UpdateInfo,
    on_status: Option<Rc<dyn Fn(InstallStatus) + 'static>>,
) {
    if !crate::update::can_apply_in_app() {
        let detail = format!(
            "{} {} is available. This installation is managed outside the AppImage user install, so it will not modify system package files. Use the authenticated AppImage installer to migrate or update safely.\n\n{}",
            crate::app_meta::APP_TITLE,
            info.metadata.version,
            installer_command()
        );
        let dialog = adw::AlertDialog::new(Some("AppImage installer required"), Some(&detail));
        dialog.add_responses(&[("close", "Close"), ("open", "Open Release Page")]);
        dialog.set_close_response("close");
        dialog.set_default_response(Some("open"));
        dialog.set_response_appearance("open", adw::ResponseAppearance::Suggested);
        let parent = parent.clone();
        let tag = info.metadata.tag.clone();
        dialog.choose(
            &parent.clone(),
            None::<&gio::Cancellable>,
            move |response| {
                if response == "open" {
                    open_release_page(&parent, &tag);
                }
            },
        );
        return;
    }
    if info.metadata.requires_helper_update {
        let detail = format!(
            "{} {} also changes the privileged Wayland hotkey helper. Use the authenticated one-command installer for this release so the application and root-owned helper are upgraded together.\n\n{}",
            crate::app_meta::APP_TITLE,
            info.metadata.version,
            installer_command()
        );
        let dialog = adw::AlertDialog::new(Some("Authenticated installer required"), Some(&detail));
        dialog.add_response("close", "Close");
        dialog.set_close_response("close");
        dialog.set_default_response(Some("close"));
        dialog.choose(parent, None::<&gio::Cancellable>, |_| {});
        return;
    }
    let mut detail = if info.metadata.summary.is_empty() {
        format!(
            "{} {} is ready to download and install.",
            crate::app_meta::APP_TITLE,
            info.metadata.version
        )
    } else {
        format!(
            "{} {} is available.\n\n• {}",
            crate::app_meta::APP_TITLE,
            info.metadata.version,
            info.metadata.summary.join("\n• ")
        )
    };
    detail.push_str(&format!(
        "\n\n{} will restart and current playback will stop.",
        crate::app_meta::APP_TITLE
    ));
    let dialog = adw::AlertDialog::new(Some("Install update?"), Some(&detail));
    dialog.add_responses(&[("cancel", "Cancel"), ("update", "Restart & Update")]);
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("update"));
    dialog.set_response_appearance("update", adw::ResponseAppearance::Suggested);
    let parent = parent.clone();
    dialog.choose(
        &parent.clone(),
        None::<&gio::Cancellable>,
        move |response| {
            if response == "update" {
                start_update_download(&parent, info, on_status);
            }
        },
    );
}
