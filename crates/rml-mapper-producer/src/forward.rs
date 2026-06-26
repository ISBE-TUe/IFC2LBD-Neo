//! Forward raw `Vec<Triple>` batches as `TaggedBatch` through a channel.
//!
//! This mirrors the `forward_as_tagged` helper in `ifc2lbd-wasm/src/plugins.rs`.
//! Standalone plugin crates can't use that one (it's private to the runner),
//! so we define our own.

use crossbeam::channel::Receiver;
use lbd_ontology::Triple;
use lbd_pipeline::{BatchKind, TaggedBatch};

/// Spawn a rayon task that drains `raw_receiver`, wraps each `Vec<Triple>` into
/// a `TaggedBatch` with the given `graph_iri`, and forwards it to `sender`.
///
/// The task exits when the receiver is exhausted (sender dropped).
pub fn forward_as_tagged(
    raw_receiver: Receiver<Vec<Triple>>,
    graph_iri: BatchKind,
    sender: crossbeam::channel::Sender<TaggedBatch>,
) {
    rayon::spawn(move || {
        while let Ok(triples) = raw_receiver.recv() {
            if sender
                .send(TaggedBatch {
                    kind: graph_iri.clone(),
                    triples,
                })
                .is_err()
            {
                break; // downstream closed
            }
        }
    });
}
