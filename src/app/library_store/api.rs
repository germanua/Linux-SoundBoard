impl LibraryStore {
    pub fn open(path: PathBuf) -> Result<Self, LibraryError> {
        Self::open_inner(path, None)
    }

    pub fn open_authoritative(path: PathBuf, library_id: &str) -> Result<Self, LibraryError> {
        Self::open_inner(path, Some(library_id.to_string()))
    }

    fn open_inner(
        path: PathBuf,
        expected_library_id: Option<String>,
    ) -> Result<Self, LibraryError> {
        let queue = Arc::new(RequestQueue::default());
        let worker_queue = Arc::clone(&queue);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("library-db".to_string())
            .spawn(move || {
                let mut connection = match open_connection(&path) {
                    Ok(connection) => {
                        if let Some(expected) = expected_library_id.as_deref() {
                            let identity = connection
                                .query_row(
                                    "SELECT value FROM meta WHERE key = 'library_id'",
                                    [],
                                    |row| row.get::<_, String>(0),
                                )
                                .optional();
                            let ready = connection
                                .query_row(
                                    "SELECT value FROM meta WHERE key = 'database_ready'",
                                    [],
                                    |row| row.get::<_, String>(0),
                                )
                                .optional();
                            if !matches!(identity, Ok(Some(ref value)) if value == expected)
                                || !matches!(ready, Ok(Some(ref value)) if value == "1")
                            {
                                let _ =
                                    ready_tx
                                        .send(Err("library database identity changed before open"
                                            .to_string()));
                                return;
                            }
                        }
                        let _ = ready_tx.send(Ok(()));
                        connection
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                };
                let mut counts_dirty = false;
                let mut counts_published_at: Option<std::time::Instant> = None;

                let mut optimized_at = Some(std::time::Instant::now());
                loop {
                    let request = match worker_queue.try_pop() {
                        Some(request) => request,
                        None => {
                            publish_library_counts_if_dirty(
                                &connection,
                                &mut counts_dirty,
                                &mut counts_published_at,
                            );
                            run_idle_optimize_if_due(&connection, &mut optimized_at);
                            match worker_queue.pop() {
                                Some(request) => request,
                                None => break,
                            }
                        }
                    };
                    counts_dirty |= request.changes_library_counts();
                    handle_request(&mut connection, request);
                }
            })
            .map_err(|_| LibraryError::WorkerUnavailable)?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self(Arc::new(LibraryStoreInner {
                queue,
                worker: Mutex::new(Some(worker)),
            }))),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(LibraryError::InvalidData(error))
            }
            Err(_) => {
                let _ = worker.join();
                Err(LibraryError::WorkerUnavailable)
            }
        }
    }

    pub fn apply_batch(&self, batch: LibraryBatch) -> LibraryResponse<()> {
        if batch.row_count() > MAX_BATCH_ROWS {
            return LibraryResponse::ready(Err(LibraryError::InvalidData(format!(
                "library batches are limited to {MAX_BATCH_ROWS} rows"
            ))));
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::ApplyBatch { batch, reply }, response)
    }

    pub fn count(&self, scope: LibraryScope, search: &str) -> LibraryResponse<usize> {
        self.count_request(scope, search, None)
    }

    pub(crate) fn count_coalesced(
        &self,
        owner: u64,
        generation: u64,
        scope: LibraryScope,
        search: &str,
    ) -> LibraryResponse<usize> {
        self.count_request(scope, search, Some(SearchGeneration { owner, generation }))
    }

    fn count_request(
        &self,
        scope: LibraryScope,
        search: &str,
        query_generation: Option<SearchGeneration>,
    ) -> LibraryResponse<usize> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::Count {
                scope,
                search: search.to_lowercase(),
                query_generation,
                reply,
            },
            response,
        )
    }

    pub fn page(
        &self,
        scope: LibraryScope,
        search: &str,
        page: usize,
    ) -> LibraryResponse<SoundPage> {
        self.page_request(scope, search, page, None, PagePriority::Visible)
    }

    pub(crate) fn page_coalesced(
        &self,
        owner: u64,
        generation: u64,
        scope: LibraryScope,
        search: &str,
        page: usize,
    ) -> LibraryResponse<SoundPage> {
        self.page_request(
            scope,
            search,
            page,
            Some(SearchGeneration { owner, generation }),
            PagePriority::Visible,
        )
    }

    pub(crate) fn prefetch_page_coalesced(
        &self,
        owner: u64,
        generation: u64,
        scope: LibraryScope,
        search: &str,
        page: usize,
    ) -> LibraryResponse<SoundPage> {
        self.page_request(
            scope,
            search,
            page,
            Some(SearchGeneration { owner, generation }),
            PagePriority::Prefetch,
        )
    }

    fn page_request(
        &self,
        scope: LibraryScope,
        search: &str,
        page: usize,
        query_generation: Option<SearchGeneration>,
        priority: PagePriority,
    ) -> LibraryResponse<SoundPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::Page {
                scope,
                search: search.to_lowercase(),
                page,
                query_generation,
                priority,
                reply,
            },
            response,
        )
    }

    pub fn sound_by_id(&self, id: &str) -> LibraryResponse<Option<Sound>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::SoundById {
                id: id.to_string(),
                reply,
            },
            response,
        )
    }

    pub fn sound_by_path(&self, path: &str) -> LibraryResponse<Option<Sound>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::SoundByPath {
                path: path.to_string(),
                reply,
            },
            response,
        )
    }

    pub fn sound_for_binding(&self, binding_id: &str) -> LibraryResponse<Option<Sound>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::SoundForBinding {
                binding_id: binding_id.to_string(),
                reply,
            },
            response,
        )
    }

    pub fn adjacent(
        &self,
        scope: LibraryScope,
        search: &str,
        position: usize,
        offset: i32,
    ) -> LibraryResponse<Option<Sound>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::Adjacent {
                scope,
                search: search.to_lowercase(),
                position,
                offset,
                reply,
            },
            response,
        )
    }

    pub fn position_for_sound(
        &self,
        scope: LibraryScope,
        search: &str,
        sound_id: &str,
    ) -> LibraryResponse<Option<usize>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::PositionForSound {
                scope,
                search: search.to_lowercase(),
                sound_id: sound_id.to_string(),
                reply,
            },
            response,
        )
    }

    pub fn hotkey_page(&self, page: usize) -> LibraryResponse<SoundPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::HotkeyPage { page, reply }, response)
    }

    pub fn hotkey_bindings_after(&self, after: Option<&str>) -> LibraryResponse<HotkeyBindingPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::HotkeyBindingsAfter {
                after: after.map(str::to_string),
                reply,
            },
            response,
        )
    }

    pub fn hotkey_binding(&self, binding_id: &str) -> LibraryResponse<Option<HotkeyBindingRecord>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::HotkeyBinding {
                binding_id: binding_id.to_string(),
                reply,
            },
            response,
        )
    }

    pub fn hotkey_bindings_for_sound(
        &self,
        sound_id: &str,
    ) -> LibraryResponse<Vec<HotkeyBindingRecord>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::HotkeyBindingsForSound {
                sound_id: sound_id.to_string(),
                reply,
            },
            response,
        )
    }

    pub fn hotkey_group(&self, binding_id: &str) -> LibraryResponse<Vec<HotkeyGroupMember>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::HotkeyGroup {
                binding_id: binding_id.to_string(),
                reply,
            },
            response,
        )
    }

    pub fn set_hotkey_binding(&self, binding: HotkeyBindingRecord) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::SetHotkeyBinding { binding, reply }, response)
    }

    pub fn delete_hotkey_binding(&self, binding_id: &str) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::DeleteHotkeyBinding {
                binding_id: binding_id.to_string(),
                reply,
            },
            response,
        )
    }

    pub fn hotkey_conflict(
        &self,
        binding_id: &str,
        normalized: &str,
        sounds_may_share: bool,
        tab_scope: Option<&str>,
    ) -> LibraryResponse<Option<String>> {
        self.hotkey_conflict_request(
            binding_id.to_string(),
            normalized,
            sounds_may_share,
            tab_scope,
            Vec::new(),
        )
    }

    pub fn hotkey_conflict_excluding_sounds(
        &self,
        normalized: &str,
        excluded_sound_ids: &[String],
        sounds_may_share: bool,
        tab_scope: Option<&str>,
    ) -> LibraryResponse<Option<String>> {
        self.hotkey_conflict_request(
            String::new(),
            normalized,
            sounds_may_share,
            tab_scope,
            excluded_sound_ids.to_vec(),
        )
    }

    fn hotkey_conflict_request(
        &self,
        binding_id: String,
        normalized: &str,
        sounds_may_share: bool,
        tab_scope: Option<&str>,
        excluded_sound_ids: Vec<String>,
    ) -> LibraryResponse<Option<String>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::HotkeyConflict {
                binding_id,
                normalized: normalized.to_string(),
                sounds_may_share,
                tab_scope: tab_scope.map(str::to_string),
                excluded_sound_ids,
                reply,
            },
            response,
        )
    }

    pub fn set_sound_hotkeys(
        &self,
        sound_ids: Vec<String>,
        accelerator: Option<String>,
        tab_scope: Option<String>,
    ) -> LibraryResponse<usize> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::SetSoundHotkeys {
                sound_ids,
                accelerator,
                tab_scope,
                reply,
            },
            response,
        )
    }

    pub fn loudness_stats(&self) -> LibraryResponse<LoudnessStats> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::LoudnessStats { reply }, response)
    }

    pub fn stats(&self) -> LibraryResponse<LibraryStats> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::LibraryStats { reply }, response)
    }

    pub fn loudness_backfill_after(&self, after: Option<&str>) -> LibraryResponse<SoundPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::LoudnessBackfillAfter {
                after: after.map(str::to_string),
                reply,
            },
            response,
        )
    }

    pub fn loudness_refinement_candidates(
        &self,
        force: bool,
        after: Option<&str>,
        limit: usize,
    ) -> LibraryResponse<SoundPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::LoudnessRefinementCandidates {
                force,
                after: after.map(str::to_string),
                limit: limit.min(MAX_BATCH_ROWS),
                reply,
            },
            response,
        )
    }

    pub fn apply_loudness_updates(&self, updates: Vec<LoudnessUpdate>) -> LibraryResponse<usize> {
        if updates.len() > MAX_BATCH_ROWS {
            return LibraryResponse::ready(Err(LibraryError::InvalidData(format!(
                "loudness update exceeds {MAX_BATCH_ROWS} rows"
            ))));
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::ApplyLoudnessUpdates { updates, reply }, response)
    }

    pub fn begin_root_scan(&self, root_path: &str, position: usize) -> LibraryResponse<i64> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::BeginRootScan {
                root_path: root_path.to_string(),
                position,
                reply,
            },
            response,
        )
    }

    pub fn apply_root_scan_batch(
        &self,
        root_path: &str,
        generation: i64,
        folders: Vec<FolderRecord>,
        sounds: Vec<SoundRecord>,
    ) -> LibraryResponse<()> {
        let row_count = folders
            .iter()
            .map(|folder| Path::new(&folder.relative_path).components().count().max(1))
            .fold(0_usize, usize::saturating_add)
            .saturating_add(
                sounds
                    .iter()
                    .map(|sound| 1_usize.saturating_add(sound.locations.len()))
                    .sum(),
            );
        if row_count > MAX_BATCH_ROWS {
            return LibraryResponse::ready(Err(LibraryError::InvalidData(format!(
                "root scan batches are limited to {MAX_BATCH_ROWS} rows"
            ))));
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::RootScanBatch {
                root_path: root_path.to_string(),
                generation,
                folders,
                sounds,
                reply,
            },
            response,
        )
    }

    pub fn finish_root_scan(&self, root_path: &str, generation: i64) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::FinishRootScan {
                root_path: root_path.to_string(),
                generation,
                reply,
            },
            response,
        )
    }

    pub fn cancel_root_scan(&self, root_path: &str, generation: i64) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::CancelRootScan {
                root_path: root_path.to_string(),
                generation,
                reply,
            },
            response,
        )
    }

    pub fn remove_root(&self, root_path: &str) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::RemoveRoot {
                root_path: root_path.to_string(),
                reply,
            },
            response,
        )
    }

    pub fn roots(&self, page: usize) -> LibraryResponse<RootPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::Roots { page, reply }, response)
    }

    pub fn folder_children(
        &self,
        root_path: &str,
        parent_relative_path: Option<&str>,
        page: usize,
    ) -> LibraryResponse<FolderPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::FolderChildren {
                root_path: root_path.to_string(),
                parent_relative_path: parent_relative_path.map(str::to_string),
                page,
                reply,
            },
            response,
        )
    }

    pub fn manual_tabs(&self, page: usize) -> LibraryResponse<ManualTabPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::ManualTabs { page, reply }, response)
    }

    pub fn upsert_manual_tab(&self, tab: ManualTabRecord) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::UpsertManualTab(tab))
    }

    pub fn delete_manual_tab(&self, public_id: &str) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::DeleteManualTab(public_id.to_string()))
    }

    pub fn set_manual_membership(
        &self,
        membership: ManualMembershipRecord,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::SetManualMembership(membership))
    }

    pub fn remove_manual_membership(
        &self,
        tab_public_id: &str,
        sound_public_id: &str,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::RemoveManualMembership {
            tab_public_id: tab_public_id.to_string(),
            sound_public_id: sound_public_id.to_string(),
        })
    }

    pub fn remove_manual_memberships(
        &self,
        tab_public_id: &str,
        sound_public_ids: Vec<String>,
    ) -> LibraryResponse<bool> {
        self.apply_manual_memberships(
            Vec::new(),
            sound_public_ids
                .into_iter()
                .map(|sound_public_id| (tab_public_id.to_string(), sound_public_id))
                .collect(),
        )
    }

    pub fn apply_manual_memberships(
        &self,
        additions: Vec<ManualMembershipRecord>,
        removals: Vec<(String, String)>,
    ) -> LibraryResponse<bool> {
        if additions.len().saturating_add(removals.len()) > MAX_BATCH_ROWS {
            return LibraryResponse::ready(Err(LibraryError::InvalidData(format!(
                "manual membership batches are limited to {MAX_BATCH_ROWS} rows"
            ))));
        }
        self.edit(LibraryEdit::ApplyManualMemberships {
            additions,
            removals,
        })
    }

    pub fn set_folder_override(&self, record: FolderOverrideRecord) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::SetFolderOverride(record))
    }

    pub fn clear_folder_override(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        sound_public_id: &str,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::ClearFolderOverride {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            sound_public_id: sound_public_id.to_string(),
        })
    }

    pub fn clear_folder_overrides(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        sound_public_ids: Vec<String>,
    ) -> LibraryResponse<bool> {
        if sound_public_ids.len() > MAX_BATCH_ROWS {
            return LibraryResponse::ready(Err(LibraryError::InvalidData(format!(
                "folder override batches are limited to {MAX_BATCH_ROWS} rows"
            ))));
        }
        self.edit(LibraryEdit::ClearFolderOverrides {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            sound_public_ids,
        })
    }

    pub fn set_folder_preferences(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        display_name: Option<&str>,
        sibling_position: Option<usize>,
        expanded: bool,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::SetFolderPreferences {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            display_name: display_name.map(str::to_string),
            sibling_position,
            expanded,
        })
    }

    pub fn set_folder_expanded(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        expanded: bool,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::SetFolderExpanded {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            expanded,
        })
    }

    pub fn set_folder_hidden(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        hidden: bool,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::SetFolderHidden {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            hidden,
        })
    }

    pub fn hidden_folders(&self, page: usize) -> LibraryResponse<HiddenFolderPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::HiddenFolders { page, reply }, response)
    }

    pub fn set_folder_display_name(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        display_name: Option<&str>,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::SetFolderDisplayName {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            display_name: display_name.map(str::to_string),
        })
    }

    pub fn move_folder(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        direction: i32,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::MoveFolder {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            direction,
        })
    }

    pub fn reorder_folder(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        target_index: usize,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::ReorderFolder {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            target_index,
        })
    }

    pub fn update_sound(&self, sound: Sound) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::UpdateSound { sound, reply }, response)
    }

    pub fn delete_sound(&self, id: &str) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::DeleteSound {
                id: id.to_string(),
                reply,
            },
            response,
        )
    }

    fn edit(&self, edit: LibraryEdit) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::Edit { edit, reply }, response)
    }

    fn enqueue<T>(
        &self,
        request: Request,
        response: mpsc::Receiver<Result<T, LibraryError>>,
    ) -> LibraryResponse<T> {
        match self.0.queue.push(request) {
            Ok(()) => LibraryResponse(response),
            Err(error) => LibraryResponse::ready(Err(error)),
        }
    }
}

const OPTIMIZE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30 * 60);
