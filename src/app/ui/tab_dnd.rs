use glib::Bytes;
use serde::{Deserialize, Serialize};

pub(super) const SOUND_TAB_DND_MIME: &str = "application/x-lsb-sound-tab-dnd";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SoundTabDragPayload {
    pub source_tab_id: String,
    #[serde(default)]
    pub source_folder: Option<FolderDragContext>,
    pub sound_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FolderDragContext {
    pub root_path: String,
    pub relative_path: String,
}

impl SoundTabDragPayload {
    pub fn normalized(self) -> Option<Self> {
        let mut deduped = Vec::new();
        for sound_id in self.sound_ids {
            let trimmed = sound_id.trim();
            if trimmed.is_empty() {
                continue;
            }
            if deduped.iter().any(|existing: &String| existing == trimmed) {
                continue;
            }
            deduped.push(trimmed.to_string());
        }

        if deduped.is_empty() {
            return None;
        }

        Some(Self {
            source_tab_id: self.source_tab_id,
            source_folder: self.source_folder,
            sound_ids: deduped,
        })
    }
}

pub(super) fn encode_drag_payload(payload: &SoundTabDragPayload) -> Option<Bytes> {
    let json = serde_json::to_vec(payload).ok()?;
    Some(Bytes::from_owned(json))
}

pub(super) fn decode_drag_payload(bytes: &Bytes) -> Option<SoundTabDragPayload> {
    let payload: SoundTabDragPayload = serde_json::from_slice(bytes.as_ref()).ok()?;
    payload.normalized()
}

/// Folder drags get their own MIME type instead of a flag inside the sound
/// payload. A folder drop rewrites whole memberships, so mistaking one for a
/// sound drop would silently scramble the library — disjoint types make that
/// impossible instead of just unlikely.
pub(super) const FOLDER_DND_MIME: &str = "application/x-lsb-folder-dnd";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FolderDragPayload {
    pub root_path: String,
    pub relative_path: String,
    /// Parent of the dragged folder, `None` at top level. Reordering is only
    /// offered among siblings, so the drop side compares this instead of
    /// guessing from the tree.
    #[serde(default)]
    pub parent_relative_path: Option<String>,
}

pub(super) fn encode_folder_drag(payload: &FolderDragPayload) -> Option<Bytes> {
    let json = serde_json::to_vec(payload).ok()?;
    Some(Bytes::from_owned(json))
}

pub(super) fn decode_folder_drag(bytes: &Bytes) -> Option<FolderDragPayload> {
    let payload: FolderDragPayload = serde_json::from_slice(bytes.as_ref()).ok()?;
    if payload.root_path.trim().is_empty() || payload.relative_path.trim().is_empty() {
        return None;
    }
    Some(payload)
}

/// Where a pointer sits within a folder row during a drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FolderDropZone {
    /// Insert above this row.
    Before,
    /// Merge into this row's folder.
    Into,
    /// Insert below this row.
    After,
}

/// Splits a row into insert / merge / insert bands: middle half merges, outer
/// quarters insert. Aim at a row and you merge, hover near the gap and you
/// reorder.
pub(super) fn folder_drop_zone(y: f64, height: f64) -> FolderDropZone {
    if height <= 0.0 {
        return FolderDropZone::Into;
    }
    let ratio = (y / height).clamp(0.0, 1.0);
    if ratio < 0.25 {
        FolderDropZone::Before
    } else if ratio > 0.75 {
        FolderDropZone::After
    } else {
        FolderDropZone::Into
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outer_quarters_insert_and_the_middle_merges() {
        let height = 40.0;
        assert_eq!(folder_drop_zone(0.0, height), FolderDropZone::Before);
        assert_eq!(folder_drop_zone(9.0, height), FolderDropZone::Before);
        assert_eq!(folder_drop_zone(20.0, height), FolderDropZone::Into);
        assert_eq!(folder_drop_zone(31.0, height), FolderDropZone::After);
        assert_eq!(folder_drop_zone(40.0, height), FolderDropZone::After);
    }

    #[test]
    fn the_merge_band_is_the_middle_half() {
        // Aim at the row and it merges; only the edges reorder. The boundaries
        // themselves still merge, so a slightly off aim never reorders.
        let height = 100.0;
        assert_eq!(folder_drop_zone(25.0, height), FolderDropZone::Into);
        assert_eq!(folder_drop_zone(75.0, height), FolderDropZone::Into);
        assert_eq!(folder_drop_zone(24.0, height), FolderDropZone::Before);
        assert_eq!(folder_drop_zone(76.0, height), FolderDropZone::After);
    }

    #[test]
    fn a_zero_height_row_merges_rather_than_guessing() {
        assert_eq!(folder_drop_zone(0.0, 0.0), FolderDropZone::Into);
    }

    #[test]
    fn a_folder_payload_round_trips_and_rejects_empty_identity() {
        let payload = FolderDragPayload {
            root_path: "/music".to_string(),
            relative_path: "albumA".to_string(),
            parent_relative_path: Some("sounds".to_string()),
        };
        let encoded = encode_folder_drag(&payload).expect("encode");
        assert_eq!(decode_folder_drag(&encoded).as_ref(), Some(&payload));

        let empty = FolderDragPayload {
            root_path: "  ".to_string(),
            relative_path: "albumA".to_string(),
            parent_relative_path: None,
        };
        let encoded = encode_folder_drag(&empty).expect("encode");
        assert!(decode_folder_drag(&encoded).is_none());
    }
}
