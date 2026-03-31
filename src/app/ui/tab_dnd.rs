use glib::Bytes;
use serde::{Deserialize, Serialize};

pub(super) const SOUND_TAB_DND_MIME: &str = "application/x-lsb-sound-tab-dnd";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SoundTabDragPayload {
    pub source_tab_id: String,
    pub sound_ids: Vec<String>,
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
