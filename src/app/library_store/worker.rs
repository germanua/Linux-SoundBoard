fn run_idle_optimize_if_due(
    connection: &Connection,
    optimized_at: &mut Option<std::time::Instant>,
) -> bool {
    if optimized_at.is_some_and(|at| at.elapsed() < OPTIMIZE_INTERVAL) {
        return false;
    }
    *optimized_at = Some(std::time::Instant::now());
    if let Err(error) = connection.execute_batch("PRAGMA optimize;") {
        log::warn!("Idle PRAGMA optimize failed: {error}");
    }
    true
}

const COUNTS_REPUBLISH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

impl Request {
    fn changes_library_counts(&self) -> bool {
        match self {
            Request::FinishRootScan { .. }
            | Request::CancelRootScan { .. }
            | Request::RemoveRoot { .. }
            | Request::Edit { .. }
            | Request::UpdateSound { .. }
            | Request::DeleteSound { .. }
            | Request::SetHotkeyBinding { .. }
            | Request::DeleteHotkeyBinding { .. }
            | Request::SetSoundHotkeys { .. } => true,
            Request::Count { .. }
            | Request::Page { .. }
            | Request::SoundById { .. }
            | Request::SoundByPath { .. }
            | Request::SoundForBinding { .. }
            | Request::Adjacent { .. }
            | Request::PositionForSound { .. }
            | Request::HotkeyPage { .. }
            | Request::HotkeyBindingsAfter { .. }
            | Request::HotkeyBinding { .. }
            | Request::HotkeyGroup { .. }
            | Request::HotkeyBindingsForSound { .. }
            | Request::HotkeyConflict { .. }
            | Request::LoudnessStats { .. }
            | Request::LibraryStats { .. }
            | Request::LoudnessBackfillAfter { .. }
            | Request::LoudnessRefinementCandidates { .. }
            | Request::ApplyLoudnessUpdates { .. }
            | Request::BeginRootScan { .. }
            | Request::RootScanBatch { .. }
            | Request::ApplyBatch { .. }
            | Request::Roots { .. }
            | Request::FolderChildren { .. }
            | Request::HiddenFolders { .. }
            | Request::ManualTabs { .. } => false,
        }
    }
}

fn publish_library_counts_if_dirty(
    connection: &Connection,
    dirty: &mut bool,
    published_at: &mut Option<std::time::Instant>,
) -> Option<LibraryStats> {
    if !*dirty {
        return None;
    }
    if published_at.is_some_and(|at| at.elapsed() < COUNTS_REPUBLISH_INTERVAL) {
        return None;
    }
    *dirty = false;
    *published_at = Some(std::time::Instant::now());
    match load_library_stats(connection) {
        Ok(stats) => {
            crate::diagnostics::set_library_counts(
                stats.sounds,
                stats.manual_tabs,
                stats.roots,
                stats.active_hotkeys,
            );
            Some(stats)
        }
        Err(error) => {
            log::warn!("Failed to refresh library diagnostics counts: {error}");
            None
        }
    }
}

fn handle_request(connection: &mut Connection, request: Request) {
    match request {
        Request::Count {
            scope,
            search,
            query_generation: _,
            reply,
        } => {
            let _ = reply.send(count_sounds(connection, &scope, &search));
        }
        Request::Page {
            scope,
            search,
            page,
            query_generation: _,
            priority: _,
            reply,
        } => {
            let _ = reply.send(load_page(connection, &scope, &search, page));
        }
        Request::SoundById { id, reply } => {
            let _ = reply.send(load_sound(connection, &id));
        }
        Request::SoundByPath { path, reply } => {
            let _ = reply.send(load_sound_by_path(connection, &path));
        }
        Request::SoundForBinding { binding_id, reply } => {
            let _ = reply.send(load_sound_for_binding(connection, &binding_id));
        }
        Request::Adjacent {
            scope,
            search,
            position,
            offset,
            reply,
        } => {
            let _ = reply.send(load_adjacent_sound(
                connection, &scope, &search, position, offset,
            ));
        }
        Request::PositionForSound {
            scope,
            search,
            sound_id,
            reply,
        } => {
            let _ = reply.send(load_sound_position(connection, &scope, &search, &sound_id));
        }
        Request::HotkeyPage { page, reply } => {
            let _ = reply.send(load_hotkey_page(connection, page));
        }
        Request::HotkeyBindingsAfter { after, reply } => {
            let _ = reply.send(load_hotkey_bindings_after(connection, after.as_deref()));
        }
        Request::HotkeyBinding { binding_id, reply } => {
            let _ = reply.send(load_hotkey_binding(connection, &binding_id));
        }
        Request::HotkeyGroup { binding_id, reply } => {
            let _ = reply.send(load_hotkey_group(connection, &binding_id));
        }
        Request::HotkeyBindingsForSound { sound_id, reply } => {
            let _ = reply.send(load_hotkey_bindings_for_sound(connection, &sound_id));
        }
        Request::SetHotkeyBinding { binding, reply } => {
            let _ = reply.send(set_hotkey_binding(connection, binding));
        }
        Request::DeleteHotkeyBinding { binding_id, reply } => {
            let _ = reply.send(delete_hotkey_binding(connection, &binding_id));
        }
        Request::HotkeyConflict {
            binding_id,
            normalized,
            sounds_may_share,
            tab_scope,
            excluded_sound_ids,
            reply,
        } => {
            let _ = reply.send(load_hotkey_conflict(
                connection,
                &binding_id,
                &normalized,
                sounds_may_share,
                tab_scope.as_deref(),
                &excluded_sound_ids,
            ));
        }
        Request::SetSoundHotkeys {
            sound_ids,
            accelerator,
            tab_scope,
            reply,
        } => {
            let _ = reply.send(set_sound_hotkeys(
                connection,
                &sound_ids,
                accelerator.as_deref(),
                tab_scope.as_deref(),
            ));
        }
        Request::LoudnessStats { reply } => {
            let _ = reply.send(load_loudness_stats(connection));
        }
        Request::LibraryStats { reply } => {
            let _ = reply.send(load_library_stats(connection));
        }
        Request::LoudnessBackfillAfter { after, reply } => {
            let _ = reply.send(load_loudness_backfill_after(connection, after.as_deref()));
        }
        Request::LoudnessRefinementCandidates {
            force,
            after,
            limit,
            reply,
        } => {
            let _ = reply.send(load_loudness_refinement_candidates(
                connection,
                force,
                after.as_deref(),
                limit,
            ));
        }
        Request::ApplyLoudnessUpdates { updates, reply } => {
            let _ = reply.send(apply_loudness_updates(connection, updates));
        }
        Request::BeginRootScan {
            root_path,
            position,
            reply,
        } => {
            let _ = reply.send(begin_root_scan(connection, &root_path, position));
        }
        Request::RootScanBatch {
            root_path,
            generation,
            folders,
            sounds,
            reply,
        } => {
            let _ = reply.send(apply_root_scan_batch(
                connection, &root_path, generation, folders, sounds,
            ));
        }
        Request::FinishRootScan {
            root_path,
            generation,
            reply,
        } => {
            let _ = reply.send(finish_root_scan(connection, &root_path, generation));
        }
        Request::CancelRootScan {
            root_path,
            generation,
            reply,
        } => {
            let _ = reply.send(cancel_root_scan(connection, &root_path, generation));
        }
        Request::RemoveRoot { root_path, reply } => {
            let _ = reply.send(remove_root(connection, &root_path));
        }
        Request::Roots { page, reply } => {
            let _ = reply.send(load_roots(connection, page));
        }
        Request::FolderChildren {
            root_path,
            parent_relative_path,
            page,
            reply,
        } => {
            let _ = reply.send(load_folder_children(
                connection,
                &root_path,
                parent_relative_path.as_deref(),
                page,
            ));
        }
        Request::HiddenFolders { page, reply } => {
            let _ = reply.send(load_hidden_folders(connection, page));
        }
        Request::ManualTabs { page, reply } => {
            let _ = reply.send(load_manual_tabs(connection, page));
        }
        Request::Edit { edit, reply } => {
            let _ = reply.send(apply_edit(connection, edit));
        }
        Request::UpdateSound { sound, reply } => {
            let _ = reply.send(update_sound(connection, sound));
        }
        Request::DeleteSound { id, reply } => {
            let _ = reply.send(delete_sound(connection, &id));
        }
        Request::ApplyBatch { batch, reply } => {
            let _ = reply.send(apply_batch(connection, batch));
        }
    }
}
