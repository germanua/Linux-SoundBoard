#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_folder_drop_builds_sparse_move_overrides() {
        let payload = tab_dnd::SoundTabDragPayload {
            source_tab_id: "folder:source".to_string(),
            source_folder: Some(tab_dnd::FolderDragContext {
                root_path: "/music".to_string(),
                relative_path: "source".to_string(),
            }),
            sound_ids: vec!["one".to_string(), "two".to_string()],
        };
        let target = tab_dnd::FolderDragContext {
            root_path: "/music".to_string(),
            relative_path: "target".to_string(),
        };

        let overrides = folder_drop_overrides(&payload, &target);
        assert_eq!(overrides.len(), 4);
        assert_eq!(
            overrides
                .iter()
                .map(|record| (
                    record.folder_relative_path.as_str(),
                    record.sound_public_id.as_str(),
                    record.action
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "target",
                    "one",
                    crate::library_store::FolderOverrideAction::Include
                ),
                (
                    "source",
                    "one",
                    crate::library_store::FolderOverrideAction::Exclude
                ),
                (
                    "target",
                    "two",
                    crate::library_store::FolderOverrideAction::Include
                ),
                (
                    "source",
                    "two",
                    crate::library_store::FolderOverrideAction::Exclude
                ),
            ]
        );
        assert!(folder_drop_overrides(
            &payload,
            payload.source_folder.as_ref().expect("source folder")
        )
        .is_empty());
    }

    #[test]
    fn should_request_next_sibling_page_triggers_near_loaded_end() {
        assert!(should_request_next_sibling_page(224, 256, true, false));
    }

    #[test]
    fn should_request_next_sibling_page_does_not_trigger_far_from_end() {
        assert!(!should_request_next_sibling_page(0, 256, true, false));
    }

    #[test]
    fn should_request_next_sibling_page_never_triggers_without_more_pages() {
        assert!(!should_request_next_sibling_page(255, 256, false, false));
    }

    #[test]
    fn should_request_next_sibling_page_never_triggers_while_in_flight() {
        assert!(!should_request_next_sibling_page(224, 256, true, true));
    }

    #[test]
    fn should_request_next_sibling_page_boundary_exactly_at_margin() {
        assert!(should_request_next_sibling_page(224, 256, true, false));
        assert!(!should_request_next_sibling_page(223, 256, true, false));
    }

    fn node_with_loaded_children(child_count: usize, grandchildren: usize) -> BoxedAnyObject {
        let parent = FolderNode::folder_at(
            "/music".to_string(),
            crate::library_store::FolderItem {
                id: 0,
                relative_path: "albums".to_string(),
                name: "albums".to_string(),
                expanded: true,
                has_children: true,
            },
            0,
            None,
        );
        for index in 0..child_count {
            let child = FolderNode::folder_at(
                "/music".to_string(),
                crate::library_store::FolderItem {
                    id: index as i64 + 1,
                    relative_path: format!("albums/Album {index}"),
                    name: format!("Album {index}"),
                    expanded: false,
                    has_children: grandchildren > 0,
                },
                index,
                None,
            );
            for deep in 0..grandchildren {
                child
                    .children()
                    .append(&BoxedAnyObject::new(FolderNode::folder_at(
                        "/music".to_string(),
                        crate::library_store::FolderItem {
                            id: 1_000 + deep as i64,
                            relative_path: format!("albums/Album {index}/Disc {deep}"),
                            name: format!("Disc {deep}"),
                            expanded: false,
                            has_children: false,
                        },
                        deep,
                        None,
                    )));
            }
            parent.children().append(&BoxedAnyObject::new(child));
        }
        BoxedAnyObject::new(parent)
    }

    #[test]
    fn counts_directly_loaded_child_rows() {
        let node = node_with_loaded_children(5, 0);
        let store = gio::ListStore::new::<BoxedAnyObject>();
        store.append(&node);

        assert_eq!(count_loaded_child_rows(&store), 6);
    }

    #[test]
    fn counts_rows_loaded_under_nested_folders() {
        let node = node_with_loaded_children(3, 4);
        let store = gio::ListStore::new::<BoxedAnyObject>();
        store.append(&node);

        assert_eq!(count_loaded_child_rows(&store), 16);
    }

    #[test]
    fn counts_nothing_for_an_empty_tree() {
        let store = gio::ListStore::new::<BoxedAnyObject>();
        assert_eq!(count_loaded_child_rows(&store), 0);
    }

    #[test]
    fn a_failed_page_load_leaves_the_node_reloadable() {
        let temp_dir =
            std::env::temp_dir().join(format!("lsb-reloadable-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("create test dir");
        let library = crate::library_store::LibraryStore::open(temp_dir.join("library.sqlite3"))
            .expect("open disposable library store");

        let children_requested = Rc::new(Cell::new(true));
        let pager = Rc::new(SiblingPager {
            library,
            children: gio::ListStore::new::<BoxedAnyObject>(),
            root_path: "/music".to_string(),
            parent_relative_path: Some("albums".to_string()),
            loaded: Cell::new(0),
            next_page: Cell::new(0),
            has_more: Cell::new(false),
            in_flight: Cell::new(false),
            loaded_pages: RefCell::new(std::collections::BTreeSet::new()),
            focus_page: Cell::new(0),
            pending_pages: RefCell::new(std::collections::BTreeSet::new()),
            children_requested: Rc::clone(&children_requested),
        });

        pager.mark_reloadable();

        assert!(
            !children_requested.get(),
            "a failed load left the request latch set, so the create-closure \
             will never start a new pager and the folder stays empty"
        );
    }

    #[test]
    fn a_page_requested_while_busy_is_remembered() {
        let temp_dir =
            std::env::temp_dir().join(format!("lsb-pending-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("create test dir");
        let library = crate::library_store::LibraryStore::open(temp_dir.join("library.sqlite3"))
            .expect("open disposable library store");
        let pager = Rc::new(SiblingPager {
            library,
            children: gio::ListStore::new::<BoxedAnyObject>(),
            root_path: "/music".to_string(),
            parent_relative_path: None,
            loaded: Cell::new(0),
            next_page: Cell::new(0),
            has_more: Cell::new(true),
            in_flight: Cell::new(true),
            loaded_pages: RefCell::new(std::collections::BTreeSet::new()),
            focus_page: Cell::new(0),
            pending_pages: RefCell::new(std::collections::BTreeSet::new()),
            children_requested: Rc::new(Cell::new(true)),
        });

        TabsInner::load_sibling_page(Rc::clone(&pager), 5);

        assert!(
            pager.pending_pages.borrow().contains(&5),
            "a page requested while busy was dropped, so its rows stay blank"
        );
    }

    #[test]
    fn placeholder_rows_do_not_break_the_retention_walk() {
        let store = gio::ListStore::new::<BoxedAnyObject>();
        store.append(&node_with_loaded_children(2, 0));
        store.append(&BoxedAnyObject::new(PlaceholderRow {
            sibling_index: 512,
            pager: std::rc::Weak::new(),
        }));

        assert_eq!(count_loaded_child_rows(&store), 4);
    }

    #[test]
    fn keeps_every_page_while_within_the_window() {
        let pages: std::collections::BTreeSet<usize> = (0..4).collect();
        assert_eq!(page_to_evict(&pages, 2, 6), None);
    }

    #[test]
    fn drops_the_page_farthest_from_the_viewport() {
        let pages: std::collections::BTreeSet<usize> = [0, 5, 6, 7, 8, 9, 10].into_iter().collect();
        assert_eq!(page_to_evict(&pages, 7, 6), Some(0));
    }

    #[test]
    fn drops_pages_ahead_when_they_are_farther_than_pages_behind() {
        let pages: std::collections::BTreeSet<usize> = [0, 1, 2, 3, 4, 5, 20].into_iter().collect();
        assert_eq!(page_to_evict(&pages, 1, 6), Some(20));
    }

    #[test]
    fn breaks_distance_ties_by_dropping_the_lower_page() {
        let pages: std::collections::BTreeSet<usize> = [3, 4, 5, 6, 7].into_iter().collect();
        assert_eq!(page_to_evict(&pages, 5, 4), Some(3));
    }

    #[test]
    fn leaf_folders_allocate_no_child_store() {
        let leaf = FolderNode::folder_at(
            "/music".to_string(),
            crate::library_store::FolderItem {
                id: 1,
                relative_path: "albums/Album 1".to_string(),
                name: "Album 1".to_string(),
                expanded: false,
                has_children: false,
            },
            0,
            None,
        );
        assert!(
            leaf.loaded_children().is_none(),
            "leaf folder allocated a child store it can never use"
        );
    }

    #[test]
    fn folders_with_children_still_get_a_store_on_demand() {
        let parent = FolderNode::folder_at(
            "/music".to_string(),
            crate::library_store::FolderItem {
                id: 1,
                relative_path: "albums".to_string(),
                name: "albums".to_string(),
                expanded: false,
                has_children: true,
            },
            0,
            None,
        );
        let store = parent.children();
        store.append(&BoxedAnyObject::new(1u8));
        assert_eq!(
            parent.children().n_items(),
            1,
            "the lazily created store must be retained, not rebuilt per call"
        );
    }

    #[test]
    fn ignores_the_expansion_notification_storm_from_a_collapsing_parent() {
        assert!(!should_handle_expansion_change(false, false));
        assert!(!should_handle_expansion_change(true, true));
    }

    #[test]
    fn handles_a_real_expansion_change() {
        assert!(should_handle_expansion_change(true, false));
        assert!(should_handle_expansion_change(false, true));
    }

    #[test]
    fn dropping_below_a_later_row_does_not_overshoot() {
        assert_eq!(folder_reorder_target_index(1, 3, true), 3);
        assert_eq!(folder_reorder_target_index(1, 3, false), 2);
    }

    #[test]
    fn dropping_above_an_earlier_row_keeps_the_slot() {
        assert_eq!(folder_reorder_target_index(3, 1, false), 1);
        assert_eq!(folder_reorder_target_index(3, 1, true), 2);
    }

    #[test]
    fn a_folders_parent_comes_from_its_relative_path() {
        assert_eq!(folder_parent_relative_path("albumA"), None);
        assert_eq!(
            folder_parent_relative_path("albumA/disc1").as_deref(),
            Some("albumA")
        );
        assert_eq!(folder_parent_relative_path("a/b/c").as_deref(), Some("a/b"));
    }

    fn merge_payload(root: &str, relative: &str) -> tab_dnd::FolderDragPayload {
        tab_dnd::FolderDragPayload {
            root_path: root.to_string(),
            relative_path: relative.to_string(),
            parent_relative_path: folder_parent_relative_path(relative),
        }
    }

    #[test]
    fn folders_can_be_combined_across_different_parents() {
        let request =
            folder_merge_request(&merge_payload("/music", "albumA/disc1"), "/music", "albumB")
                .expect("a folder should combine into an unrelated folder");
        assert_eq!(request.source_relative_path, "albumA/disc1");
        assert_eq!(request.destination_relative_path, "albumB");
    }

    #[test]
    fn a_folder_can_be_combined_into_its_own_ancestor() {
        assert!(
            folder_merge_request(&merge_payload("/music", "albumA/disc1"), "/music", "albumA")
                .is_some()
        );
    }

    #[test]
    fn a_folder_cannot_be_combined_into_itself_or_its_own_subtree() {
        assert!(
            folder_merge_request(&merge_payload("/music", "albumA"), "/music", "albumA").is_none()
        );
        assert!(
            folder_merge_request(&merge_payload("/music", "albumA"), "/music", "albumA/disc1")
                .is_none()
        );

        assert!(
            folder_merge_request(&merge_payload("/music", "album"), "/music", "albumA").is_some()
        );
    }

    #[test]
    fn folders_from_different_roots_cannot_be_combined() {
        assert!(
            folder_merge_request(&merge_payload("/music", "albumA"), "/other", "albumB").is_none()
        );
    }

    #[test]
    fn tearing_the_tree_down_does_not_persist_collapse() {
        assert!(
            !should_persist_expansion_change(true, true),
            "a collapse caused by rebuilding the tree is not user intent"
        );
        assert!(
            should_persist_expansion_change(true, false),
            "a real user collapse must still be saved"
        );
        assert!(
            !should_persist_expansion_change(false, false),
            "an unchanged row writes nothing"
        );
    }

    #[test]
    fn collapsing_retains_children_while_under_the_row_cap() {
        assert!(!should_release_collapsed_children(1_000, 4_096));
    }

    #[test]
    fn collapsing_releases_children_once_over_the_row_cap() {
        assert!(should_release_collapsed_children(4_097, 4_096));
    }

    #[test]
    fn row_cap_boundary_retains_at_exactly_the_cap() {
        assert!(!should_release_collapsed_children(4_096, 4_096));
        assert!(should_release_collapsed_children(4_097, 4_096));
    }

    fn pump_until(done: impl Fn() -> bool) -> bool {
        let context = glib::MainContext::default();
        for _ in 0..2_000 {
            while context.iteration(false) {}
            if done() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        done()
    }

    #[test]
    #[ignore = "drives the shared GTK main context: needs a display and must \
                run alone, e.g. cargo test --lib -- --ignored --exact \
                ui::tabs_sidebar::tests::releasing_a_collapsed_node_reloads_its_children_on_reexpand"]
    #[allow(clippy::print_stderr)]
    fn releasing_a_collapsed_node_reloads_its_children_on_reexpand() {
        if gtk4::init().is_err() {
            eprintln!("skipped: no display available");
            return;
        }
        let temp_dir =
            std::env::temp_dir().join(format!("lsb-reexpand-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("create test dir");
        let library = crate::library_store::LibraryStore::open(temp_dir.join("library.sqlite3"))
            .expect("open disposable library store");
        let root_path = temp_dir.to_string_lossy().into_owned();

        library
            .apply_batch(crate::library_store::LibraryBatch::Roots(vec![
                crate::library_store::RootRecord {
                    path: root_path.clone(),
                    position: 0,
                },
            ]))
            .recv()
            .expect("insert root");
        let folders = (0..8usize)
            .map(|index| crate::library_store::FolderRecord {
                root_path: root_path.clone(),
                relative_path: format!("Album {index}"),
                parent_relative_path: None,
                name: format!("Album {index}"),
                position: index,
            })
            .collect();
        library
            .apply_batch(crate::library_store::LibraryBatch::Folders(folders))
            .recv()
            .expect("insert folders");

        let roots = gio::ListStore::new::<BoxedAnyObject>();
        let node = FolderNode::root(root_path.clone());
        let children = node.children();
        let children_pager = Rc::clone(&node.children_pager);
        let children_requested = Rc::clone(&node.children_requested);
        roots.append(&BoxedAnyObject::new(node));

        let library_for_children = library.clone();
        let tree = TreeListModel::new(roots.clone(), false, false, move |item| {
            let boxed = item.downcast_ref::<BoxedAnyObject>()?;
            let node = boxed.borrow::<FolderNode>();
            if !node.has_children {
                return None;
            }
            if !node.children_requested.replace(true) {
                TabsInner::start_children_pager(
                    library_for_children.clone(),
                    node.children(),
                    node.root_path.clone(),
                    node.relative_path.clone(),
                    &node.children_pager,
                    Rc::clone(&node.children_requested),
                );
            }
            Some(node.children().upcast())
        });

        let row = tree.row(0).expect("root row");
        row.set_expanded(true);
        assert!(
            pump_until(|| children.n_items() == 8),
            "first expand never loaded children, got {}",
            children.n_items()
        );

        row.set_expanded(false);
        children.remove_all();
        children_pager.replace(None);
        children_requested.set(false);

        row.set_expanded(true);
        assert!(
            pump_until(|| children.n_items() == 8),
            "re-expanding a released node did not reload its children, got {}",
            children.n_items()
        );
    }

    #[test]
    #[ignore = "drives the shared GTK main context: needs a display and must \
                run alone, e.g. cargo test --lib -- --ignored --exact \
                ui::tabs_sidebar::tests::scrolling_a_wide_folder_bounds_loaded_pages"]
    #[allow(clippy::print_stderr)]
    fn scrolling_a_wide_folder_bounds_loaded_pages() {
        if gtk4::init().is_err() {
            eprintln!("skipped: no display available");
            return;
        }
        const PAGE: usize = crate::library_store::PAGE_SIZE;
        const FOLDERS: usize = PAGE * (MAX_LOADED_SIBLING_PAGES + 4);

        let temp_dir =
            std::env::temp_dir().join(format!("lsb-window-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("create test dir");
        let library = crate::library_store::LibraryStore::open(temp_dir.join("library.sqlite3"))
            .expect("open disposable library store");
        let root_path = temp_dir.to_string_lossy().into_owned();
        library
            .apply_batch(crate::library_store::LibraryBatch::Roots(vec![
                crate::library_store::RootRecord {
                    path: root_path.clone(),
                    position: 0,
                },
            ]))
            .recv()
            .expect("insert root");
        for chunk_start in (0..FOLDERS).step_by(500) {
            let chunk = (chunk_start..(chunk_start + 500).min(FOLDERS))
                .map(|index| crate::library_store::FolderRecord {
                    root_path: root_path.clone(),
                    relative_path: format!("Album {index:05}"),
                    parent_relative_path: None,
                    name: format!("Album {index:05}"),
                    position: index,
                })
                .collect();
            library
                .apply_batch(crate::library_store::LibraryBatch::Folders(chunk))
                .recv()
                .expect("insert folders");
        }

        let node = FolderNode::root(root_path.clone());
        let children = node.children();
        let pager_slot = Rc::clone(&node.children_pager);
        let requested = Rc::clone(&node.children_requested);
        TabsInner::start_children_pager(
            library,
            children.clone(),
            root_path,
            None,
            &pager_slot,
            requested,
        );
        assert!(
            pump_until(|| children.n_items() as usize == PAGE),
            "first page never arrived"
        );
        let pager = pager_slot.borrow().clone().expect("pager installed");

        for page in 1..(FOLDERS / PAGE) {
            pager.focus_page.set(page);
            TabsInner::load_folder_children_async(Rc::clone(&pager));
            assert!(
                pump_until(|| !pager.in_flight.get()),
                "page {page} never settled"
            );
        }

        assert_eq!(
            children.n_items() as usize,
            FOLDERS,
            "row count must stay at the full folder count so indices are stable"
        );
        assert!(
            pager.loaded_pages.borrow().len() <= MAX_LOADED_SIBLING_PAGES,
            "loaded pages {} exceeded the window of {}",
            pager.loaded_pages.borrow().len(),
            MAX_LOADED_SIBLING_PAGES
        );

        let first = children
            .item(0)
            .and_downcast::<BoxedAnyObject>()
            .expect("row 0");
        assert!(
            first.try_borrow::<PlaceholderRow>().is_ok(),
            "row 0 should have been evicted to a placeholder"
        );
    }

    #[test]
    fn restores_persisted_expansion_only_on_first_bind() {
        assert!(should_restore_expansion(true, false));
        assert!(!should_restore_expansion(true, true));
    }

    #[test]
    fn never_forces_collapse_from_bind() {
        assert!(!should_restore_expansion(false, false));
        assert!(!should_restore_expansion(false, true));
    }

    #[test]
    fn expansion_latch_only_restores_on_first_bind_of_a_node() {
        let already_restored = Cell::new(false);
        let first_bind = should_restore_expansion(true, already_restored.replace(true));
        let second_bind = should_restore_expansion(true, already_restored.replace(true));
        assert_eq!([first_bind, second_bind], [true, false]);
    }

    #[test]
    fn sibling_pager_is_released_when_parent_drops() {
        let temp_dir =
            std::env::temp_dir().join(format!("lsb-tabs-sidebar-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("create test dir");
        let library = crate::library_store::LibraryStore::open(temp_dir.join("library.sqlite3"))
            .expect("open disposable library store");

        let children = gio::ListStore::new::<BoxedAnyObject>();
        let pager = Rc::new(SiblingPager {
            library,
            children: children.clone(),
            root_path: "/music".to_string(),
            parent_relative_path: None,
            loaded: Cell::new(3),
            next_page: Cell::new(1),
            has_more: Cell::new(false),
            in_flight: Cell::new(false),
            loaded_pages: RefCell::new(std::collections::BTreeSet::new()),
            focus_page: Cell::new(0),
            pending_pages: RefCell::new(std::collections::BTreeSet::new()),
            children_requested: Rc::new(Cell::new(true)),
        });

        for index in 0..3usize {
            let item = crate::library_store::FolderItem {
                id: index as i64,
                relative_path: format!("albums/Album {index:05}"),
                name: format!("Album {index:05}"),
                expanded: false,
                has_children: false,
            };
            children.append(&BoxedAnyObject::new(FolderNode::folder_at(
                "/music".to_string(),
                item,
                index,
                Some(Rc::downgrade(&pager)),
            )));
        }

        let weak = Rc::downgrade(&pager);
        drop(pager);
        drop(children);

        assert!(
            weak.upgrade().is_none(),
            "SiblingPager was still reachable after all outside owners dropped it; \
             pager<->children<->FolderNode reference cycle is leaking"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    fn read_pss_kib() -> usize {
        let smaps = std::fs::read_to_string("/proc/self/smaps_rollup").expect("read smaps_rollup");
        smaps
            .lines()
            .find_map(|line| line.strip_prefix("Pss:"))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse::<usize>().ok())
            .expect("parse PSS")
    }

    #[test]
    #[ignore = "manual measurement: needs a display, run under xvfb-run"]
    #[allow(clippy::print_stdout)]
    fn measure_retained_folder_child_row_cost() {
        if gtk4::init().is_err() {
            println!(
                "retained folder child row cost: skipped, gtk4::init() failed (no display available)"
            );
            return;
        }

        let root_path = "/home/flinux/Музика".to_string();

        for count in [1_000usize, 10_000, 50_000] {
            let before_kib = read_pss_kib();

            let store = gio::ListStore::new::<BoxedAnyObject>();
            for index in 0..count {
                let item = crate::library_store::FolderItem {
                    id: index as i64,
                    relative_path: format!("albums/Album {index:05}"),
                    name: format!("Album {index:05}"),
                    expanded: false,
                    has_children: false,
                };
                store.append(&BoxedAnyObject::new(FolderNode::folder(
                    root_path.clone(),
                    item,
                )));
            }

            let after_kib = read_pss_kib();
            drop(store);

            let delta_kib = after_kib.saturating_sub(before_kib);
            let bytes_per_row = (delta_kib * 1024) as f64 / count as f64;
            println!(
                "retained folder child row cost: n={count} delta_kib={delta_kib} bytes_per_row={bytes_per_row:.1}"
            );
        }
    }
}

#[cfg(test)]
mod tab_hotkey_tests {
    use super::tab_scope_key;
    use crate::app_meta::GENERAL_TAB_ID;

    #[test]
    fn general_and_manual_tabs_use_separate_scope_namespaces() {
        assert_eq!(tab_scope_key(GENERAL_TAB_ID), "general");
        assert_eq!(
            tab_scope_key("41d0f0a4-6a1e-4a0e-9d2e-0b0f8a1d2c3e"),
            "tab:41d0f0a4-6a1e-4a0e-9d2e-0b0f8a1d2c3e"
        );
    }
}
