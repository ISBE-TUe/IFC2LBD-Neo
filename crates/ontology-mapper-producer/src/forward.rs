//! Forward raw `Vec<Triple>` batches as `TaggedBatch` through a channel.

use crossbeam::channel::Receiver;
use lbd_ontology::Triple;
use lbd_pipeline::{BatchKind, TaggedBatch};

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
                break;
            }
        }
    });
}
