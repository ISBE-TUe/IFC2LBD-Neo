fn main() {
    let args: Vec<String> = std::env::args().collect();
    let ifc_path = args.get(1).map(String::as_str).unwrap_or("web/wasm-prototype/public/sample.ifc");
    let out_path = args.get(2).map(String::as_str).unwrap_or("/tmp/ifc_lite_output.bos");

    eprintln!("Reading {}...", ifc_path);
    let content = std::fs::read_to_string(ifc_path).expect("read IFC");
    use ifc_lite_core::{build_entity_index, EntityDecoder, EntityScanner};
    use ifc_lite_geometry::router::GeometryRouter;
    let index = build_entity_index(&content);
    let mut decoder = EntityDecoder::with_index(&content, index);
    let router = GeometryRouter::with_units(&content, &mut decoder);
    let element_types = ["IFCWALL","IFCWALLSTANDARDCASE","IFCSLAB","IFCSLABSTANDARDCASE","IFCCOLUMN","IFCCOLUMNSTANDARDCASE","IFCDOOR","IFCDOORSTANDARDCASE","IFCWINDOW","IFCWINDOWSTANDARDCASE","IFCSPACE","IFCBUILDINGELEMENTPROXY","IFCMEMBER","IFCMEMBERSTANDARDCASE","IFCCOVERING","IFCPLATE","IFCPLATESTANDARDCASE","IFCSTAIR","IFCSTAIRFLIGHT","IFCBEAM","IFCBEAMSTANDARDCASE"];
    let mut element_ids: Vec<u32> = Vec::new();
    let mut scanner = EntityScanner::new(&content);
    while let Some((id, type_name, _, _)) = scanner.next_entity() {
        if element_types.contains(&type_name) { element_ids.push(id); }
    }
    eprintln!("Found {} elements", element_ids.len());
    let mut all_x: Vec<f32> = Vec::new(); let mut all_y: Vec<f32> = Vec::new(); let mut all_z: Vec<f32> = Vec::new();
    let mut all_nx: Vec<f32> = Vec::new(); let mut all_ny: Vec<f32> = Vec::new(); let mut all_nz: Vec<f32> = Vec::new();
    let mut all_i0: Vec<u32> = Vec::new(); let mut all_i1: Vec<u32> = Vec::new(); let mut all_i2: Vec<u32> = Vec::new();
    struct MeshRow { express_id: i64, vs: i64, vc: i64, is: i64, ic: i64 }
    let mut mesh_rows: Vec<MeshRow> = Vec::new();
    let mut elements_with_geom = 0usize;
    let mut base_v: u32 = 0;
    for eid in &element_ids {
        let Ok(entity) = decoder.decode_by_id(*eid) else { continue; };
        let world = router.resolve_scaled_placement(&entity, &mut decoder).unwrap_or([1.0,0.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,0.0,1.0]);
        let Ok(subs) = router.process_element_submeshes_in_definition_space(&entity, &mut decoder) else { continue; };
        if subs.sub_meshes.is_empty() { continue; }
        elements_with_geom += 1;
        let vs = all_x.len() as i64; let is = all_i0.len() as i64;
        let m = &world;
        for sub in &subs.sub_meshes {
            let pos = &sub.mesh.positions; let nrm = &sub.mesh.normals; let n = pos.len()/3;
            for i in 0..n {
                let (lx,ly,lz)=(pos[i*3] as f64,pos[i*3+1] as f64,pos[i*3+2] as f64);
                all_x.push((m[0]*lx+m[4]*ly+m[8]*lz+m[12]) as f32);
                all_y.push((m[1]*lx+m[5]*ly+m[9]*lz+m[13]) as f32);
                all_z.push((m[2]*lx+m[6]*ly+m[10]*lz+m[14]) as f32);
                if nrm.len()==pos.len() {
                    let (nx,ny,nz)=(nrm[i*3] as f64,nrm[i*3+1] as f64,nrm[i*3+2] as f64);
                    let wnx=m[0]*nx+m[4]*ny+m[8]*nz; let wny=m[1]*nx+m[5]*ny+m[9]*nz; let wnz=m[2]*nx+m[6]*ny+m[10]*nz;
                    let len=(wnx*wnx+wny*wny+wnz*wnz).sqrt().max(1e-12);
                    all_nx.push((wnx/len) as f32); all_ny.push((wny/len) as f32); all_nz.push((wnz/len) as f32);
                } else { all_nx.push(0.0); all_ny.push(0.0); all_nz.push(1.0); }
            }
            for tri in sub.mesh.indices.chunks_exact(3) { all_i0.push(tri[0]+base_v); all_i1.push(tri[1]+base_v); all_i2.push(tri[2]+base_v); }
            base_v += (pos.len()/3) as u32;
        }
        mesh_rows.push(MeshRow { express_id: *eid as i64, vs, vc: all_x.len() as i64-vs, is, ic: all_i0.len() as i64-is });
    }
    eprintln!("Processed: {} elements, {} vertices, {} triangles", elements_with_geom, all_x.len(), all_i0.len()/3);
    use parquet::arrow::ArrowWriter; use parquet::basic::Compression; use parquet::file::properties::WriterProperties;
    use arrow_array::{Float32Array,Int64Array,UInt32Array,RecordBatch};
    use arrow_schema::{DataType,Field,Schema};
    use std::io::{Cursor,Write as IoWrite}; use std::sync::Arc;
    use zip::write::{FileOptions,ZipWriter}; use zip::CompressionMethod;
    let props = WriterProperties::builder().set_compression(Compression::SNAPPY).build();
    let write_pq = |schema: Arc<Schema>, cols: Vec<Arc<dyn arrow_array::Array>>| -> Vec<u8> {
        let mut buf=Vec::new(); let mut w=ArrowWriter::try_new(Cursor::new(&mut buf),schema.clone(),Some(props.clone())).unwrap();
        w.write(&RecordBatch::try_new(schema,cols).unwrap()).unwrap(); w.close().unwrap(); buf
    };
    let vb_schema = Arc::new(Schema::new(vec![Field::new("X",DataType::Float32,false),Field::new("Y",DataType::Float32,false),Field::new("Z",DataType::Float32,false),Field::new("NormalX",DataType::Float32,false),Field::new("NormalY",DataType::Float32,false),Field::new("NormalZ",DataType::Float32,false)]));
    let vb = write_pq(vb_schema,vec![Arc::new(Float32Array::from(all_x)),Arc::new(Float32Array::from(all_y)),Arc::new(Float32Array::from(all_z)),Arc::new(Float32Array::from(all_nx)),Arc::new(Float32Array::from(all_ny)),Arc::new(Float32Array::from(all_nz))]);
    let ib_schema = Arc::new(Schema::new(vec![Field::new("Index0",DataType::UInt32,false),Field::new("Index1",DataType::UInt32,false),Field::new("Index2",DataType::UInt32,false)]));
    let ib = write_pq(ib_schema,vec![Arc::new(UInt32Array::from(all_i0)),Arc::new(UInt32Array::from(all_i1)),Arc::new(UInt32Array::from(all_i2))]);
    let m_schema = Arc::new(Schema::new(vec![Field::new("ExpressId",DataType::Int64,false),Field::new("VertexStart",DataType::Int64,false),Field::new("VertexCount",DataType::Int64,false),Field::new("IndexStart",DataType::Int64,false),Field::new("IndexCount",DataType::Int64,false)]));
    let mp = write_pq(m_schema,vec![Arc::new(Int64Array::from(mesh_rows.iter().map(|r|r.express_id).collect::<Vec<_>>())),Arc::new(Int64Array::from(mesh_rows.iter().map(|r|r.vs).collect::<Vec<_>>())),Arc::new(Int64Array::from(mesh_rows.iter().map(|r|r.vc).collect::<Vec<_>>())),Arc::new(Int64Array::from(mesh_rows.iter().map(|r|r.is).collect::<Vec<_>>())),Arc::new(Int64Array::from(mesh_rows.iter().map(|r|r.ic).collect::<Vec<_>>()))]);
    let mut zip_buf: Vec<u8>=Vec::new();
    { let opts:FileOptions<()>=FileOptions::default().compression_method(CompressionMethod::Deflated);
      let mut zip=ZipWriter::new(Cursor::new(&mut zip_buf));
      zip.start_file("VertexBuffer.parquet",opts).unwrap(); zip.write_all(&vb).unwrap();
      zip.start_file("IndexBuffer.parquet",opts).unwrap(); zip.write_all(&ib).unwrap();
      zip.start_file("Meshes.parquet",opts).unwrap(); zip.write_all(&mp).unwrap();
      zip.finish().unwrap(); }
    std::fs::write(out_path,&zip_buf).unwrap();
    println!("=== ifc-lite geometry output for {} ===", ifc_path);
    println!("Written to: {}", out_path);
    println!("Elements with geometry: {}", elements_with_geom);
    println!("Total vertices:         {}", mesh_rows.iter().map(|r|r.vc).sum::<i64>());
    println!("Total triangles:        {}", mesh_rows.iter().map(|r|r.ic).sum::<i64>()/3);
    println!("VertexBuffer.parquet:   {:>8} bytes  ({:.2} MB)", vb.len(), vb.len() as f64/1e6);
    println!("IndexBuffer.parquet:    {:>8} bytes  ({:.2} MB)", ib.len(), ib.len() as f64/1e6);
    println!("Meshes.parquet:         {:>8} bytes  ({:.2} MB)", mp.len(), mp.len() as f64/1e6);
    println!("ZIP geometry only:      {:>8} bytes  ({:.2} MB)", zip_buf.len(), zip_buf.len() as f64/1e6);
    println!();
    println!("Our parquet STRIPPED (geometry only):  5,021,498 bytes (5.02 MB)");
    println!("Our parquet FULL (+ metadata tables):  5,346,659 bytes (5.35 MB)");
}
