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
                Arc::clone(&state2.config),
                state2.library.clone(),
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

        clear_rebuild_pending(rebuild_pending.as_ref());
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

    let existing_rows = {
        let mut tracked = folder_rows.borrow_mut();
        std::mem::take(&mut *tracked)
    };
    log::info!(
        "rebuild_sound_folder_rows: Removing {} existing rows",
        existing_rows.len()
    );
    for row_weak in existing_rows {
        let Some(row) = row_weak.upgrade() else {
            continue;
        };

        if row.parent().is_none() {
            continue;
        }

        folders_group.remove(&row);
    }

    let folders = {
        let cfg = state.config.lock();
        log::info!(
            "rebuild_sound_folder_rows: Config has {} folders: {:?}",
            cfg.sound_folders.len(),
            cfg.sound_folders
        );
        cfg.sound_folders.clone()
    };

    let mut added_rows = 0usize;
    for folder in folders {
        let row = build_sound_folder_row(
            folder,
            Arc::clone(&state),
            folders_group,
            add_folder_row,
            Rc::clone(&folder_rows),
            Rc::clone(&rebuild_pending),
            on_library_changed.clone(),
        );
        folders_group.add(&row);
        folder_rows.borrow_mut().push(row.downgrade());
        added_rows = added_rows.saturating_add(1);
    }

    if should_attach_add_folder_row(add_folder_row.parent().is_some()) {
        folders_group.add(add_folder_row);
    } else {
        log::debug!("rebuild_sound_folder_rows: Add Folder row already attached");
    }

    log::info!(
        "rebuild_sound_folder_rows: Rebuild complete, {} rows added",
        added_rows
    );
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
