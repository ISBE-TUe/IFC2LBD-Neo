#![allow(unused_imports)]

#[cfg(target_arch = "wasm32")]
use js_sys::Function;
use wasm_bindgen::prelude::*;

use crate::memory::{execution_mode_str, select_execution_mode};
use crate::plugins::{browser_registry, js_err, to_view};
#[cfg(target_arch = "wasm32")]
use crate::runner::convert_ifc_to_sink_impl;
use crate::runner::{
    benchmark_convert_ifc_impl, convert_ifc_impl, requested_settings_for_planning,
    resolve_plan_impl,
};
use crate::types::*;

#[wasm_bindgen(js_name = listModules)]
pub fn list_modules() -> Result<JsValue, JsValue> {
    let registry = browser_registry();
    let modules: Vec<ModuleManifestView> = registry.manifests().into_iter().map(to_view).collect();
    serde_wasm_bindgen::to_value(&modules).map_err(js_err)
}

#[wasm_bindgen(js_name = resolvePlan)]
pub fn resolve_plan(
    requested_modules: JsValue,
    module_options: JsValue,
) -> Result<JsValue, JsValue> {
    let requested: Vec<String> =
        serde_wasm_bindgen::from_value(requested_modules).map_err(js_err)?;
    let options: Vec<String> = if module_options.is_null() || module_options.is_undefined() {
        Vec::new()
    } else {
        serde_wasm_bindgen::from_value(module_options).map_err(js_err)?
    };
    let resolved = resolve_plan_impl(requested, options).map_err(js_err)?;
    serde_wasm_bindgen::to_value(&resolved).map_err(js_err)
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = initNeoThreadPool)]
pub fn init_neo_thread_pool(threads: usize) -> js_sys::Promise {
    wasm_bindgen_rayon::init_thread_pool(threads)
}

#[wasm_bindgen(js_name = convertIfc)]
pub fn convert_ifc(input: &[u8], request: JsValue) -> Result<JsValue, JsValue> {
    let request: ConversionRequest = serde_wasm_bindgen::from_value(request).map_err(js_err)?;
    let result = convert_ifc_impl(input, request).map_err(js_err)?;
    serde_wasm_bindgen::to_value(&result).map_err(js_err)
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = convertIfcToSink)]
pub fn convert_ifc_to_sink(
    input: &[u8],
    request: JsValue,
    sink: Function,
) -> Result<JsValue, JsValue> {
    let request: ConversionRequest = serde_wasm_bindgen::from_value(request).map_err(js_err)?;
    let result = convert_ifc_to_sink_impl(input, request, &sink).map_err(js_err)?;
    serde_wasm_bindgen::to_value(&result).map_err(js_err)
}

#[wasm_bindgen(js_name = benchmarkConvertIfc)]
pub fn benchmark_convert_ifc(input: &[u8], request: JsValue) -> Result<JsValue, JsValue> {
    let request: ConversionRequest = serde_wasm_bindgen::from_value(request).map_err(js_err)?;
    let result = benchmark_convert_ifc_impl(input, request).map_err(js_err)?;
    serde_wasm_bindgen::to_value(&result).map_err(js_err)
}

#[wasm_bindgen(js_name = planExecution)]
pub fn plan_execution(input_size_bytes: f64, request: JsValue) -> Result<JsValue, JsValue> {
    let request: ConversionRequest = serde_wasm_bindgen::from_value(request).map_err(js_err)?;
    let settings = requested_settings_for_planning(&request).map_err(js_err)?;
    let (mode, estimated_peak_mb, feasibility_check_mb, reason) =
        select_execution_mode(input_size_bytes.max(0.0) as u64, &request, &settings);
    let plan = ExecutionPlanView {
        selected_mode: execution_mode_str(mode).to_string(),
        estimated_peak_mb,
        feasibility_check_mb,
        reason,
    };
    serde_wasm_bindgen::to_value(&plan).map_err(js_err)
}

