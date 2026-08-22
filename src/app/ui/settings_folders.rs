use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::app_state::AppState;
use crate::commands;

use super::icons;

pub(super) type FolderRowRefs = Rc<RefCell<Vec<gtk4::glib::WeakRef<adw::ActionRow>>>>;
pub(super) type RebuildPending = Rc<Cell<bool>>;

/// Reloads the removed-folder list.
pub(super) type HiddenFolderRefresh = Rc<dyn Fn()>;
/// Lets the restore button trigger the reload that creates it.
type HiddenFolderRefreshHolder = Rc<RefCell<Option<HiddenFolderRefresh>>>;

fn try_set_rebuild_pending(rebuild_pending: &Cell<bool>) -> bool {
    if rebuild_pending.get() {
        return false;
    }
    rebuild_pending.set(true);
    true
}

fn clear_rebuild_pending(rebuild_pending: &Cell<bool>) {
    rebuild_pending.set(false);
}

fn should_attach_add_folder_row(has_parent: bool) -> bool {
    !has_parent
}

fn build_sound_folder_row(
    folder: String,
    state: Arc<AppState>,
    folders_group: &adw::PreferencesGroup,
    add_folder_row: &adw::ActionRow,
    folder_rows: FolderRowRefs,
    rebuild_pending: RebuildPending,
    on_library_changed: Option<Rc<dyn Fn() + 'static>>,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(&folder).build();

    let remove_btn = gtk4::Button::builder()
        .css_classes(vec!["flat", "settings-folder-remove-btn"])
        .has_frame(false)
        .width_request(28)
        .height_request(28)
        .valign(gtk4::Align::Center)
        .tooltip_text("Remove folder")
        .build();
    icons::apply_button_icon(&remove_btn, icons::DELETE);

    {
        let folder_owned = folder.clone();
        let state2 = Arc::clone(&state);
        let folders_group2 = folders_group.downgrade();
        let add_folder_row2 = add_folder_row.downgrade();
        let folder_rows2 = Rc::clone(&folder_rows);
        let rebuild_pending2 = Rc::clone(&rebuild_pending);
        let on_library_changed2 = on_library_changed.clone();
        remove_btn.connect_clicked(move |button| {
            log::info!("Remove folder button clicked: {}", folder_owned);
            button.set_sensitive(false);
            let button_done = button.clone();
            let folders_group_done = folders_group2.clone();
            let add_folder_row_done = add_folder_row2.clone();
            let state_done = Arc::clone(&state2);
            let folder_rows_done = Rc::clone(&folder_rows2);
            let rebuild_pending_done = Rc::clone(&rebuild_pending2);
            let on_library_changed_done = on_library_changed2.clone();
            if let Err(e) = commands::remove_sound_folder_with_store_async(
                folder_owned.clone(),
                state2.library.clone(),
                state2.hotkey_projection.clone(),
                move |result| match result {
                    Ok(()) => {
                        log::info!("Remove folder command succeeded");
                        let Some(folders_group2) = folders_group_done.upgrade() else {
                            log::warn!("folders_group2 weak ref failed to upgrade");
                            return;
                        };
                        let Some(add_folder_row2) = add_folder_row_done.upgrade() else {
                            log::warn!("add_folder_row2 weak ref failed to upgrade");
                            return;
                        };
                        schedule_rebuild_sound_folder_rows(
                            &folders_group2,
                            &add_folder_row2,
                            Arc::clone(&state_done),
                            Rc::clone(&folder_rows_done),
                            Rc::clone(&rebuild_pending_done),
                            on_library_changed_done.clone(),
                        );
                        if let Some(cb) = on_library_changed_done.as_ref() {
                            cb();
                        }
                    }
                    Err(e) => {
                        button_done.set_sensitive(true);
                        log::warn!("Remove folder failed: {e}");
                    }
                },
            ) {
                button.set_sensitive(true);
                log::warn!("Failed to dispatch folder removal: {e}");
            }
        });
    }

    row.add_suffix(&remove_btn);
    row
}

pub(super) fn schedule_rebuild_sound_folder_rows(
    folders_group: &adw::PreferencesGroup,
    add_folder_row: &adw::ActionRow,
    state: Arc<AppState>,
    folder_rows: FolderRowRefs,
    rebuild_pending: RebuildPending,
    on_library_changed: Option<Rc<dyn Fn() + 'static>>,
) {
    if !try_set_rebuild_pending(rebuild_pending.as_ref()) {
        log::debug!("schedule_rebuild_sound_folder_rows: Rebuild already pending");
        return;
    }

    log::info!("schedule_rebuild_sound_folder_rows: Scheduling rebuild");
    let folders_group = folders_group.clone();
    let add_folder_row = add_folder_row.clone();
    let folder_rows = Rc::clone(&folder_rows);
    let rebuild_pending = Rc::clone(&rebuild_pending);

    gtk4::glib::idle_add_local_once(move || {
        log::info!("schedule_rebuild_sound_folder_rows: Idle callback executing");
        rebuild_sound_folder_rows(
            &folders_group,
            &add_folder_row,
            state,
            folder_rows,
            Rc::clone(&rebuild_pending),
            on_library_changed,
        );
    });
}

pub(super) fn rebuild_sound_folder_rows(
    folders_group: &adw::PreferencesGroup,
    add_folder_row: &adw::ActionRow,
    state: Arc<AppState>,
    folder_rows: FolderRowRefs,
    rebuild_pending: RebuildPending,
    on_library_changed: Option<Rc<dyn Fn() + 'static>>,
) {
    log::info!("rebuild_sound_folder_rows: Starting rebuild");
    let library = state.library.clone();
    let folders_group = folders_group.clone();
    let add_folder_row = add_folder_row.clone();
    let rebuild_pending_done = Rc::clone(&rebuild_pending);
    if let Err(error) = commands::dispatch_async_result(
        "load_settings_sound_folders",
        move || {
            let mut folders = Vec::new();
            let mut page_index = 0_usize;
            loop {
                let page = library.roots(page_index).recv()?;
                folders.extend(page.roots.into_iter().map(|root| root.path));
                if folders.len() >= page.total {
                    return Ok::<_, crate::library_store::LibraryError>(folders);
                }
                page_index = page_index.saturating_add(1);
            }
        },
        move |result| {
            let folders = match result {
                Ok(folders) => folders,
                Err(error) => {
                    log::warn!("Failed to load sound folders: {error}");
                    clear_rebuild_pending(rebuild_pending_done.as_ref());
                    return;
                }
            };
            let existing_rows = {
                let mut tracked = folder_rows.borrow_mut();
                std::mem::take(&mut *tracked)
            };
            for row_weak in existing_rows {
                if let Some(row) = row_weak.upgrade().filter(|row| row.parent().is_some()) {
                    folders_group.remove(&row);
                }
            }
            for folder in folders {
                let row = build_sound_folder_row(
                    folder,
                    Arc::clone(&state),
                    &folders_group,
                    &add_folder_row,
                    Rc::clone(&folder_rows),
                    Rc::clone(&rebuild_pending_done),
                    on_library_changed.clone(),
                );
                folders_group.add(&row);
                folder_rows.borrow_mut().push(row.downgrade());
            }
            if should_attach_add_folder_row(add_folder_row.parent().is_some()) {
                folders_group.add(&add_folder_row);
            }
            clear_rebuild_pending(rebuild_pending_done.as_ref());
        },
    ) {
        log::warn!("Failed to dispatch sound-folder load: {error}");
        clear_rebuild_pending(rebuild_pending.as_ref());
    }
}

pub(super) fn build_hidden_folders_group(
    state: Arc<AppState>,
    on_library_changed: Option<Rc<dyn Fn() + 'static>>,
) -> (adw::PreferencesGroup, HiddenFolderRefresh) {
    let group = adw::PreferencesGroup::builder()
        .title("Removed Folders")
        .description("Hidden from the sidebar. The files are still on disk.")
        .visible(false)
        .build();
    let rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::new(RefCell::new(Vec::new()));
    // Holder breaks the reload callback's self-reference.
    let holder: HiddenFolderRefreshHolder = Rc::new(RefCell::new(None));
    let refresh: HiddenFolderRefresh = {
        let group = group.clone();
        let rows = Rc::clone(&rows);
        let state = Arc::clone(&state);
        let holder = Rc::clone(&holder);
        let on_library_changed = on_library_changed.clone();
        Rc::new(move || {
            let library = state.library.clone();
            let group = group.clone();
            let rows = Rc::clone(&rows);
            let state = Arc::clone(&state);
            let holder = Rc::clone(&holder);
            let on_library_changed = on_library_changed.clone();
            if let Err(error) = commands::dispatch_async_result(
                "load_hidden_folders",
                move || {
                    let mut folders = Vec::new();
                    let mut page_index = 0_usize;
                    loop {
                        let page = library.hidden_folders(page_index).recv()?;
                        let total = page.total;
                        folders.extend(page.folders);
                        if folders.len() >= total {
                            return Ok::<_, crate::library_store::LibraryError>(folders);
                        }
                        page_index = page_index.saturating_add(1);
                    }
                },
                move |result| {
                    let folders = match result {
                        Ok(folders) => folders,
                        Err(error) => {
                            log::warn!("Failed to load removed folders: {error}");
                            return;
                        }
                    };
                    for row in rows.borrow_mut().drain(..) {
                        if row.parent().is_some() {
                            group.remove(&row);
                        }
                    }
                    group.set_visible(!folders.is_empty());
                    for folder in folders {
                        let row = adw::ActionRow::builder().title(&folder.name).build();
                        if folder.relative_path != folder.name {
                            row.set_subtitle(&folder.relative_path);
                        }
                        let restore = gtk4::Button::builder()
                            .label("Restore")
                            .css_classes(vec!["flat"])
                            .valign(gtk4::Align::Center)
                            .build();
                        let library = state.library.clone();
                        let holder = Rc::clone(&holder);
                        let on_library_changed = on_library_changed.clone();
                        let root_path = folder.root_path.clone();
                        let relative_path = folder.relative_path.clone();
                        restore.connect_clicked(move |button| {
                            button.set_sensitive(false);
                            let response =
                                library.set_folder_hidden(&root_path, &relative_path, false);
                            let button_done = button.clone();
                            let button_failed = button.clone();
                            let holder = Rc::clone(&holder);
                            let on_library_changed = on_library_changed.clone();
                            if let Err(error) = commands::dispatch_async_result(
                                "restore_hidden_folder",
                                move || response.recv(),
                                move |result| match result {
                                    Ok(_) => {
                                        if let Some(refresh) = holder.borrow().as_ref() {
                                            refresh();
                                        }
                                        if let Some(callback) = on_library_changed.as_ref() {
                                            callback();
                                        }
                                    }
                                    Err(error) => {
                                        button_done.set_sensitive(true);
                                        log::warn!("Failed to restore folder: {error}");
                                    }
                                },
                            ) {
                                button_failed.set_sensitive(true);
                                log::warn!("Failed to dispatch folder restore: {error}");
                            }
                        });
                        row.add_suffix(&restore);
                        group.add(&row);
                        rows.borrow_mut().push(row);
                    }
                },
            ) {
                log::warn!("Failed to dispatch removed folder load: {error}");
            }
        })
    };
    *holder.borrow_mut() = Some(Rc::clone(&refresh));
    (group, refresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuild_pending_coalesces_duplicate_schedules() {
        let pending = Cell::new(false);

        assert!(try_set_rebuild_pending(&pending));
        assert!(!try_set_rebuild_pending(&pending));
    }

    #[test]
    fn rebuild_pending_can_be_rearmed_after_clear() {
        let pending = Cell::new(false);

        assert!(try_set_rebuild_pending(&pending));
        clear_rebuild_pending(&pending);
        assert!(try_set_rebuild_pending(&pending));
    }

    #[test]
    fn add_folder_row_attach_is_idempotent() {
        assert!(should_attach_add_folder_row(false));
        assert!(!should_attach_add_folder_row(true));
    }
}
