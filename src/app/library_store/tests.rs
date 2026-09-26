#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_queue_prioritizes_control_over_visible_and_maintenance() {
        let queue = RequestQueue::default();
        let (maintenance_reply, _) = mpsc::sync_channel(1);
        let (visible_reply, _) = mpsc::sync_channel(1);
        let (control_reply, _) = mpsc::sync_channel(1);
        queue
            .push(Request::ApplyBatch {
                batch: LibraryBatch::Roots(Vec::new()),
                reply: maintenance_reply,
            })
            .expect("queue maintenance");
        queue
            .push(Request::Count {
                scope: LibraryScope::General,
                search: String::new(),
                query_generation: None,
                reply: visible_reply,
            })
            .expect("queue visible");
        queue
            .push(Request::SoundById {
                id: "sound".to_string(),
                reply: control_reply,
            })
            .expect("queue control");

        assert!(matches!(queue.pop(), Some(Request::SoundById { .. })));
        assert!(matches!(queue.pop(), Some(Request::Count { .. })));
        assert!(matches!(queue.pop(), Some(Request::ApplyBatch { .. })));
    }

    #[test]
    fn prefetched_page_waits_behind_visible_work() {
        let queue = RequestQueue::default();
        let (prefetch_reply, _) = mpsc::sync_channel(1);
        let (visible_reply, _) = mpsc::sync_channel(1);
        queue
            .push(Request::Page {
                scope: LibraryScope::General,
                search: String::new(),
                page: 1,
                query_generation: None,
                priority: PagePriority::Prefetch,
                reply: prefetch_reply,
            })
            .expect("queue prefetch");
        queue
            .push(Request::Count {
                scope: LibraryScope::General,
                search: String::new(),
                query_generation: None,
                reply: visible_reply,
            })
            .expect("queue visible");

        assert!(matches!(queue.pop(), Some(Request::Count { .. })));
        assert!(matches!(
            queue.pop(),
            Some(Request::Page {
                priority: PagePriority::Prefetch,
                ..
            })
        ));
    }

    #[test]
    fn request_queue_coalesces_older_search_generations() {
        let queue = RequestQueue::default();
        let (old_count_reply, old_count_response) = mpsc::sync_channel(1);
        let (old_page_reply, old_page_response) = mpsc::sync_channel(1);
        let (latest_reply, _) = mpsc::sync_channel(1);
        let old = SearchGeneration {
            owner: 7,
            generation: 1,
        };
        let latest = SearchGeneration {
            owner: 7,
            generation: 2,
        };

        queue
            .push(Request::Count {
                scope: LibraryScope::General,
                search: "old".to_string(),
                query_generation: Some(old),
                reply: old_count_reply,
            })
            .expect("queue old count");
        queue
            .push(Request::Page {
                scope: LibraryScope::General,
                search: "old".to_string(),
                page: 0,
                query_generation: Some(old),
                priority: PagePriority::Prefetch,
                reply: old_page_reply,
            })
            .expect("queue old page");
        queue
            .push(Request::Count {
                scope: LibraryScope::General,
                search: "latest".to_string(),
                query_generation: Some(latest),
                reply: latest_reply,
            })
            .expect("queue latest count");

        let state = queue.state.lock().expect("queue state");
        assert_eq!(state.visible.len(), 1);
        assert!(matches!(
            state.visible.front(),
            Some(Request::Count {
                query_generation: Some(value),
                ..
            }) if *value == latest
        ));
        drop(state);
        assert!(matches!(
            old_count_response.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert!(matches!(
            old_page_response.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));

        assert!(matches!(queue.pop(), Some(Request::Count { .. })));
        let (late_old_reply, late_old_response) = mpsc::sync_channel(1);
        queue
            .push(Request::Count {
                scope: LibraryScope::General,
                search: "late-old".to_string(),
                query_generation: Some(old),
                reply: late_old_reply,
            })
            .expect("drop late old count");
        assert!(queue.state.lock().expect("queue state").visible.is_empty());
        assert!(matches!(
            late_old_response.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn connection_uses_bounded_memory_and_durable_rollback_settings() {
        let path = std::env::temp_dir().join(format!(
            "lsb-library-pragmas-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        let connection = open_connection(&path).expect("open configured database");

        let text = |pragma| {
            connection
                .query_row(&format!("PRAGMA {pragma}"), [], |row| {
                    row.get::<_, String>(0)
                })
                .expect(pragma)
        };
        let integer = |pragma| {
            connection
                .query_row(&format!("PRAGMA {pragma}"), [], |row| row.get::<_, i64>(0))
                .expect(pragma)
        };
        assert_eq!(text("journal_mode"), "delete");
        assert_eq!(integer("synchronous"), 3);
        assert_eq!(integer("foreign_keys"), 1);
        assert_eq!(integer("cache_size"), -2048);
        assert_eq!(integer("temp_store"), 1);
        assert_eq!(integer("mmap_size"), 0);
        assert_eq!(integer("journal_size_limit"), 0);

        drop(connection);
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod idle_count_publication_tests {
    use super::*;

    fn reply<T>() -> mpsc::SyncSender<Result<T, LibraryError>> {
        mpsc::sync_channel(1).0
    }

    #[test]
    fn staged_scan_batches_do_not_dirty_the_counts() {
        let batch = Request::RootScanBatch {
            root_path: "/music".to_string(),
            generation: 1,
            folders: Vec::new(),
            sounds: Vec::new(),
            reply: reply(),
        };
        assert!(!batch.changes_library_counts());

        let finish = Request::FinishRootScan {
            root_path: "/music".to_string(),
            generation: 1,
            reply: reply(),
        };
        assert!(finish.changes_library_counts());
    }

    #[test]
    fn reads_and_loudness_writes_do_not_dirty_the_counts() {
        let page = Request::Page {
            scope: LibraryScope::General,
            search: String::new(),
            page: 0,
            query_generation: None,
            priority: PagePriority::Visible,
            reply: reply(),
        };
        assert!(!page.changes_library_counts());

        let loudness = Request::ApplyLoudnessUpdates {
            updates: Vec::new(),
            reply: reply(),
        };
        assert!(!loudness.changes_library_counts());
    }

    #[test]
    fn deleting_a_sound_dirties_the_counts() {
        let delete = Request::DeleteSound {
            id: "sound-1".to_string(),
            reply: reply(),
        };
        assert!(delete.changes_library_counts());
    }

    #[test]
    fn bulk_row_writes_do_not_dirty_the_counts() {
        let batch = Request::ApplyBatch {
            batch: LibraryBatch::Roots(Vec::new()),
            reply: reply(),
        };
        assert!(!batch.changes_library_counts());
    }

    #[test]
    fn idle_optimize_runs_once_per_interval() {
        let dir = std::env::temp_dir().join(format!("lsb-idle-optimize-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create test dir");
        let connection = open_connection(&dir.join("library.sqlite3")).expect("open connection");

        let mut optimized_at = Some(std::time::Instant::now());
        assert!(
            !run_idle_optimize_if_due(&connection, &mut optimized_at),
            "optimize must not run again right after the connection opened"
        );

        optimized_at = std::time::Instant::now().checked_sub(OPTIMIZE_INTERVAL);
        assert!(
            run_idle_optimize_if_due(&connection, &mut optimized_at),
            "optimize must run once the interval has passed"
        );
        assert!(
            !run_idle_optimize_if_due(&connection, &mut optimized_at),
            "running it must restart the interval"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_burst_of_mutations_does_not_recount_per_drain() {
        let dir = std::env::temp_dir().join(format!("lsb-idle-burst-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create test dir");
        let connection = open_connection(&dir.join("library.sqlite3")).expect("open connection");

        let mut dirty = true;
        let mut published_at = None;
        assert!(
            publish_library_counts_if_dirty(&connection, &mut dirty, &mut published_at).is_some()
        );

        dirty = true;
        assert!(
            publish_library_counts_if_dirty(&connection, &mut dirty, &mut published_at).is_none(),
            "recount must be rate limited"
        );
        assert!(
            dirty,
            "a rate-limited recount must stay pending, not be dropped"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn counts_are_published_once_per_idle_period_and_only_when_dirty() {
        let dir = std::env::temp_dir().join(format!("lsb-idle-counts-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create test dir");
        let connection = open_connection(&dir.join("library.sqlite3")).expect("open connection");

        let mut dirty = true;
        let mut published_at = None;
        let published = publish_library_counts_if_dirty(&connection, &mut dirty, &mut published_at)
            .expect("counts published");
        assert_eq!(published.sounds, 0);
        assert!(!dirty, "publishing must clear the dirty flag");
        assert!(published_at.is_some());

        assert!(
            publish_library_counts_if_dirty(&connection, &mut dirty, &mut published_at).is_none(),
            "a clean idle period must not re-query the database"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn database_file_is_private() {
        let dir = std::env::temp_dir().join(format!("lsb-private-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create db dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("loosen dir");
        let path = dir.join("library.sqlite3");
        drop(open_connection(&path).expect("open database"));
        assert_eq!(
            std::fs::metadata(&path)
                .expect("db metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("loosen db");
        drop(open_connection(&path).expect("reopen database"));
        assert_eq!(
            std::fs::metadata(&path)
                .expect("repaired db metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        std::fs::remove_dir_all(dir).expect("cleanup db dir");
    }
}
