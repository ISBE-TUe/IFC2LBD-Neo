use std::collections::HashSet;

use crossbeam::channel::Sender;
use ifc_step::{StepFile, StepSchema};
use lbd_ontology::Triple;

use crate::{ifcowl_entity_subjects, ifcowl_lookup, ifcowl_namespace, IfcOwlEmitter, StreamError};

pub(crate) fn convert_ifcowl(step: &StepFile, base: &str, schema: StepSchema) -> Vec<Triple> {
    let mut ids: Vec<_> = step.entities.keys().copied().collect();
    ids.sort_unstable();
    let namespace = ifcowl_namespace(schema);
    let lookup = ifcowl_lookup(schema);
    let max_entity_id = ids.iter().copied().max().unwrap_or(0);
    let entity_subjects = ifcowl_entity_subjects(step, base, lookup);
    let mut emitter = IfcOwlEmitter::new(
        base,
        &namespace,
        lookup,
        max_entity_id,
        &entity_subjects,
        true,
    );

    for id in ids {
        let entity = &step.entities[&id];
        emitter.emit_entity(id, entity);
    }

    deduplicate_triples(emitter.finish())
}

pub fn stream_ifcowl(
    step: &StepFile,
    base: &str,
    schema: StepSchema,
    sender: &Sender<Vec<Triple>>,
    _stream_batch_size: usize,
    max_workers_override: usize,
) -> Result<(), StreamError> {
    let mut ids: Vec<_> = step.entities.keys().copied().collect();
    ids.sort_unstable();
    let namespace = ifcowl_namespace(schema);
    let lookup = ifcowl_lookup(schema);
    let max_entity_id = ids.iter().copied().max().unwrap_or(0);
    let entity_subjects = ifcowl_entity_subjects(step, base, lookup);
    #[cfg(not(target_arch = "wasm32"))]
    let available_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    #[cfg(target_arch = "wasm32")]
    let available_cores = rayon::current_num_threads().max(1);
    let max_workers = available_cores.clamp(1, max_workers_override.clamp(1, IFCOWL_MAX_WORKERS));
    let workers_by_size = (ids.len() / IFCOWL_MIN_ENTITIES_PER_WORKER).max(1);
    let worker_count = max_workers.min(workers_by_size);

    if worker_count <= 1 {
        let mut emitter = IfcOwlEmitter::new(
            base,
            &namespace,
            lookup,
            max_entity_id,
            &entity_subjects,
            true,
        );
        for id in ids {
            let entity = &step.entities[&id];
            emitter.emit_entity(id, entity);
            // Flush after every entity so that large geometry entities
            // (e.g. IfcCartesianPointList3D with 100k+ coordinates) cannot
            // accumulate hundreds of MB of triples before the first send.
            let batch = emitter.take_triples();
            if !batch.is_empty() {
                sender.send(batch).map_err(|_| StreamError::ChannelClosed)?;
            }
        }
        return Ok(());
    }

    let chunk_size = ids.len().div_ceil(worker_count);
    let (result_sender, result_receiver) =
        crossbeam::channel::bounded::<Result<(), StreamError>>(worker_count);
    rayon::scope(|scope| {
        for (worker_index, chunk) in ids.chunks(chunk_size).enumerate() {
            let step_ref = step;
            let namespace_ref = namespace.as_str();
            let lookup_ref = lookup;
            let base_ref = base;
            let subjects_ref = &entity_subjects;
            let out_sender = sender.clone();
            let result_sender = result_sender.clone();
            scope.spawn(move |_| {
                let result = (|| -> Result<(), StreamError> {
                    let node_start = max_entity_id
                        .saturating_add((worker_index as u64 + 1) * IFCOWL_NODE_COUNTER_STRIDE);
                    let mut emitter = IfcOwlEmitter::new(
                        base_ref,
                        namespace_ref,
                        lookup_ref,
                        node_start,
                        subjects_ref,
                        worker_index == 0,
                    );
                    for id in chunk {
                        let entity = &step_ref.entities[id];
                        emitter.emit_entity(*id, entity);
                        // Flush after every entity so that large geometry entities
                        // cannot accumulate hundreds of MB before the first send.
                        let batch = emitter.take_triples();
                        if !batch.is_empty() {
                            out_sender
                                .send(batch)
                                .map_err(|_| StreamError::ChannelClosed)?;
                        }
                    }
                    Ok(())
                })();
                let _ = result_sender.send(result);
            });
        }
    });
    drop(result_sender);
    for result in result_receiver {
        result?;
    }
    Ok(())
}

const IFCOWL_MIN_ENTITIES_PER_WORKER: usize = 50_000;
const IFCOWL_MAX_WORKERS: usize = 16;
const IFCOWL_NODE_COUNTER_STRIDE: u64 = 1_000_000_000;

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
