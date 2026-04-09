use anyhow::Context;
use ifc_model::IfcModel;
use ifc_step::StepFile;
use lbd_converter::{stream_step_and_model, stream_topology_model, ConvertOptions};
use lbd_ontology::Triple;

pub(crate) fn run_core_conversion_plugin(
    step: &StepFile,
    model: &IfcModel,
    options: &ConvertOptions,
    lbd_sender: &crossbeam::channel::Sender<Vec<Triple>>,
    ifcowl_sender: Option<&crossbeam::channel::Sender<Vec<Triple>>>,
) -> anyhow::Result<()> {
    stream_step_and_model(step, model, options, lbd_sender, ifcowl_sender)
        .context("failed to stream core conversion output")
}

pub(crate) fn run_topology_producer_plugin(
    model: &IfcModel,
    options: &ConvertOptions,
    topology_sender: &crossbeam::channel::Sender<Vec<Triple>>,
) -> anyhow::Result<()> {
    stream_topology_model(model, options, topology_sender).context("failed to stream topology output")
}
