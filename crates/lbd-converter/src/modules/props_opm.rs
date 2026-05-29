use crossbeam::channel::Sender;
use ifc_model::IfcModel;
use lbd_ontology::Triple;

use crate::{
    emit_props_opm_inner, normalize_base_uri, ConvertOptions, StreamError,
    MIN_STREAM_BATCH_SIZE, MAX_STREAM_BATCH_SIZE,
};

/// Stream direct OPM property and quantity triples in bounded batches.
///
/// Emits flat direct links: element → props:predicate → property_node → OPM state.
/// No PropertySet or QuantitySet container nodes — those belong to the bSDD named graph.
/// This is the `neo-props-opm` named-graph producer.
pub fn stream_props_opm(
    model: &IfcModel,
    options: &ConvertOptions,
    sender: &Sender<Vec<Triple>>,
) -> Result<u64, StreamError> {
    let base = normalize_base_uri(&options.base_uri);
    let batch_size = options
        .stream_batch_size
        .clamp(MIN_STREAM_BATCH_SIZE, MAX_STREAM_BATCH_SIZE);
    let mut batch = Vec::with_capacity(batch_size);
    let mut triple_count: u64 = 0;
    emit_props_opm_inner(model, options, &base, &mut |triple| {
        triple_count += 1;
        batch.push(triple);
        if batch.len() >= batch_size {
            sender
                .send(std::mem::take(&mut batch))
                .map_err(|_| StreamError::ChannelClosed)?;
        }
        Ok::<(), StreamError>(())
    })?;
    if !batch.is_empty() {
        sender.send(batch).map_err(|_| StreamError::ChannelClosed)?;
    }
    Ok(triple_count)
}
