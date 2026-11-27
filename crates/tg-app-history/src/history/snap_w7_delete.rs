//! Before/after captures of the Delete Messages box (TGB-3052). Ignored; run with
//! `W7_SNAP_DIR=<dir> W7_SNAP_STAGE=before|after` and `--ignored`.

use egui_kittest::kittest::Queryable;
use tg_app_core::stand_in::types::{MsgId, PeerId};

use crate::history::backend::HistoryBackend;
use crate::history::compose_boxes::DeleteConfirmBoxState;
use crate::history::mock::MockHistoryStore;
use crate::history::tests_w5_delete::{DeleteHarness, open_peer_in, regular_messages};

fn save(h: &mut DeleteHarness, name: &str) {
    let Some(dir) = std::env::var_os("W7_SNAP_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    let stage = std::env::var("W7_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = dir.join(stage);
    std::fs::create_dir_all(&dir).expect("snap dir");
    let img = h.render().expect("wgpu render");
    img.save(dir.join(format!("{name}.png"))).expect("save png");
}

fn open_box(h: &mut DeleteHarness, peer: PeerId, ids: Vec<MsgId>) {
    let store = MockHistoryStore::new();
    let info = store.peer_info(peer).expect("mock peer");
    let summary = &info.summary;
    let chat = crate::history::delete_box::DeleteChat::from_peer(
        summary.id,
        summary.kind,
        &summary.title,
        summary.is_saved_messages,
        false,
        info.can_delete,
    );
    let messages = h.state().widget.inner().messages().to_vec();
    h.state_mut().widget.delete_box = Some(DeleteConfirmBoxState::for_messages(
        ids,
        &chat,
        &messages,
        1_800_000_000,
    ));
}

#[test]
#[ignore = "captures the TGB-3052 delete box PNGs under W7_SNAP_DIR"]
fn capture_w7_delete_box() {
    for (name, peer) in [
        ("private", PeerId::from_user(1002)),
        ("group", PeerId::from_chat(2001)),
    ] {
        let mut h = open_peer_in(MockHistoryStore::new(), peer);
        let ids = regular_messages(&h);
        let picked: Vec<MsgId> = ids.iter().rev().take(2).copied().collect();
        open_box(&mut h, peer, picked);
        h.run_steps(3);
        save(&mut h, &format!("delete_box_{name}"));
        h.get_by_label("Delete").click();
        h.run_steps(20);
        save(&mut h, &format!("after_delete_{name}"));
    }
}
