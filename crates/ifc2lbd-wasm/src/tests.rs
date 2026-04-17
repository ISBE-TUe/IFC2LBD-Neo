#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::plugins::browser_registry;
    use crate::runner::{convert_ifc_impl, resolve_plan_impl};
    use crate::types::ConversionRequest;
    use lbd_pipeline::{
        BBOX_ENRICHER_ID, FILE_EXPORT_ID, IFCOWL_PRODUCER_ID, LBD_PRODUCER_ID,
        NQUADS_SERIALIZER_ID, IFC_TOPOLOGY_PRODUCER_ID, TURTLE_SERIALIZER_ID,
    };

    fn tiny_ifc() -> Vec<u8> {
        b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC2X3'));\nENDSEC;\nDATA;\n#1=IFCPROJECT('0001',$,$,$,$,$,$,$,$);\nENDSEC;\nEND-ISO-10303-21;\n".to_vec()
    }

    #[test]
    fn list_modules_exposes_curated_browser_set() {
        let ids: HashSet<String> = browser_registry()
            .manifests()
            .into_iter()
            .map(|m| m.id.to_string())
            .collect();
        assert!(ids.contains(LBD_PRODUCER_ID));
        assert!(ids.contains(IFCOWL_PRODUCER_ID));
        assert!(ids.contains(TURTLE_SERIALIZER_ID));
        assert!(ids.contains(NQUADS_SERIALIZER_ID));
        assert!(ids.contains(FILE_EXPORT_ID));
        assert!(ids.contains(IFC_TOPOLOGY_PRODUCER_ID));
        assert!(ids.contains(BBOX_ENRICHER_ID));
        assert!(!ids.contains("neo-topology-full-producer"));
    }

    #[test]
    fn resolve_plan_rejects_unknown_module() {
        let result = resolve_plan_impl(vec!["neo-nonexistent-module".to_string()], Vec::new());
        assert!(result.is_err());
    }

    #[test]
    fn resolve_plan_accepts_ifc_topology() {
        let result = resolve_plan_impl(
            vec![
                IFC_TOPOLOGY_PRODUCER_ID.to_string(),
                LBD_PRODUCER_ID.to_string(),
                TURTLE_SERIALIZER_ID.to_string(),
                FILE_EXPORT_ID.to_string(),
            ],
            Vec::new(),
        );
        assert!(result.is_ok());
        assert!(result
            .unwrap()
            .enabled_ids
            .contains(&IFC_TOPOLOGY_PRODUCER_ID.to_string()));
    }

    #[test]
    fn convert_turtle_exports_ttl_file() {
        let bundle = convert_ifc_impl(
            &tiny_ifc(),
            ConversionRequest {
                module_ids: vec![
                    LBD_PRODUCER_ID.to_string(),
                    TURTLE_SERIALIZER_ID.to_string(),
                    FILE_EXPORT_ID.to_string(),
                ],
                module_options: Vec::new(),
                base_uri: Some("https://example.test/base/".to_string()),
                output_stem: Some("model".to_string()),
                execution_mode: None,
                memory_feasibility_mb: None,
                stream_batch_size: None,
                ifcowl_max_workers: None,
                sink_chunk_size_bytes: None,
                sink_max_pending_bytes: None,
            },
        )
        .expect("conversion should succeed");
        assert_eq!(bundle.exported_files.len(), 1);
        assert_eq!(bundle.exported_files[0].filename, "model.ttl");
        assert_eq!(
            bundle.exported_files[0].mime_type,
            "text/turtle;charset=utf-8"
        );
        assert!(!bundle.exported_files[0].payload.is_empty());
    }

    #[test]
    fn convert_nquads_ifcowl_exports_single_nq_file() {
        let bundle = convert_ifc_impl(
            &tiny_ifc(),
            ConversionRequest {
                module_ids: vec![
                    LBD_PRODUCER_ID.to_string(),
                    IFCOWL_PRODUCER_ID.to_string(),
                    NQUADS_SERIALIZER_ID.to_string(),
                    FILE_EXPORT_ID.to_string(),
                ],
                module_options: Vec::new(),
                base_uri: Some("https://example.test/base/".to_string()),
                output_stem: Some("model".to_string()),
                execution_mode: None,
                memory_feasibility_mb: None,
                stream_batch_size: None,
                ifcowl_max_workers: None,
                sink_chunk_size_bytes: None,
                sink_max_pending_bytes: None,
            },
        )
        .expect("conversion should succeed");
        assert_eq!(bundle.exported_files.len(), 1);
        assert_eq!(bundle.exported_files[0].filename, "model.nq");
        assert_eq!(bundle.exported_files[0].mime_type, "application/n-quads");
        assert!(!bundle.exported_files[0].payload.is_empty());
    }
}
