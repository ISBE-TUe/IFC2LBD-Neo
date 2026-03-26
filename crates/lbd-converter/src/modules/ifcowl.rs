use std::collections::HashSet;

use crossbeam::channel::Sender;
use ifc_step::{StepFile, StepSchema};
use lbd_ontology::Triple;

use crate::{
    ifcowl_entity_subjects, ifcowl_lookup, ifcowl_namespace, IfcOwlEmitter, StreamError,
    STREAM_BATCH_SIZE,
};

pub(crate) fn convert_ifcowl(step: &StepFile, base: &str, schema: StepSchema) -> Vec<Triple> {
    let mut ids: Vec<_> = step.entities.keys().copied().collect();
    ids.sort_unstable();
    let namespace = ifcowl_namespace(schema);
    let lookup = ifcowl_lookup(schema);
    let max_entity_id = ids.iter().copied().max().unwrap_or(0);
    let entity_subjects = ifcowl_entity_subjects(step, base, lookup);
    let mut emitter = IfcOwlEmitter::new(base, &namespace, lookup, max_entity_id, entity_subjects);

    for id in ids {
        let entity = &step.entities[&id];
        emitter.emit_entity(id, entity);
    }

    deduplicate_triples(emitter.finish())
}

pub(crate) fn stream_ifcowl(
    step: &StepFile,
    base: &str,
    schema: StepSchema,
    sender: &Sender<Vec<Triple>>,
) -> Result<(), StreamError> {
    let mut ids: Vec<_> = step.entities.keys().copied().collect();
    ids.sort_unstable();
    let namespace = ifcowl_namespace(schema);
    let lookup = ifcowl_lookup(schema);
    let max_entity_id = ids.iter().copied().max().unwrap_or(0);
    let entity_subjects = ifcowl_entity_subjects(step, base, lookup);
    let mut emitter = IfcOwlEmitter::new(base, &namespace, lookup, max_entity_id, entity_subjects);

    for id in ids {
        let entity = &step.entities[&id];
        emitter.emit_entity(id, entity);
        if emitter.pending_len() >= STREAM_BATCH_SIZE {
            sender
                .send(emitter.take_triples())
                .map_err(|_| StreamError::ChannelClosed)?;
        }
    }

    let remaining = emitter.take_triples();
    if !remaining.is_empty() {
        sender
            .send(remaining)
            .map_err(|_| StreamError::ChannelClosed)?;
    }
    Ok(())
}

fn deduplicate_triples(triples: Vec<Triple>) -> Vec<Triple> {
    let mut seen = HashSet::new();
    let mut unique = Vec::with_capacity(triples.len());
    for triple in triples {
        if seen.insert(triple.clone()) {
            unique.push(triple);
        }
    }
    unique
}
