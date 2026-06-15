// Verify the ThatOpen element-identity invariant in a produced .frag:
//   localId(pick) = local_ids[ meshes_items[ Sample.item ] ]
// must equal the element's own id stored in global_transform_ids[Sample.item].
// Usage: cargo run -p fragments-core --example verify_frag -- /tmp/model.frag
use std::io::Read;

fn main() {
    let path = std::env::args().nth(1).expect("usage: verify_frag <file.frag>");
    let compressed = std::fs::read(&path).expect("read .frag");

    // .frag payload is zlib-compressed.
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&compressed[..])
        .read_to_end(&mut raw)
        .expect("zlib decompress");

    let model = fragments_schema::root_as_model(&raw).expect("parse model");
    let local_ids = model.local_ids();
    let meshes = model.meshes();
    let meshes_items = meshes.meshes_items();
    let samples = meshes.samples();
    let gt_ids = meshes.global_transform_ids().expect("global_transform_ids");

    // guid lookup: express_id -> guid string
    let guids = model.guids();
    let guids_items = model.guids_items();
    let guid_for = |id: u32| -> Option<String> {
        (0..guids_items.len())
            .find(|&i| guids_items.get(i) == id)
            .map(|i| guids.get(i).to_string())
    };

    let mut checked = 0usize;
    let mut mismatches = 0usize;
    let mut first_bad: Option<(u32, u32, u32)> = None;
    for s in 0..samples.len() {
        let item = samples.get(s).item(); // = gtIndex
        if item as usize >= meshes_items.len() || item as usize >= gt_ids.len() {
            continue;
        }
        let item_index = meshes_items.get(item as usize); // index into local_ids
        if item_index as usize >= local_ids.len() {
            mismatches += 1;
            continue;
        }
        let resolved = local_ids.get(item_index as usize); // ThatOpen-resolved localId
        let element_id = gt_ids.get(item as usize); // the element this geometry belongs to
        checked += 1;
        if resolved != element_id {
            mismatches += 1;
            if first_bad.is_none() {
                first_bad = Some((item, resolved, element_id));
            }
        }
    }

    println!("samples checked: {checked}");
    println!("mismatches:      {mismatches}");
    if let Some((item, resolved, element_id)) = first_bad {
        println!("first mismatch: Sample.item={item} resolved local_id={resolved} but element_id={element_id}");
    }

    // Spot-check: print the resolved guid for the first few distinct elements.
    println!("--- spot check (Sample.item -> element_id -> guid) ---");
    let mut seen = std::collections::HashSet::new();
    for s in 0..samples.len() {
        let item = samples.get(s).item();
        if (item as usize) >= gt_ids.len() {
            continue;
        }
        let element_id = gt_ids.get(item as usize);
        if !seen.insert(element_id) {
            continue;
        }
        let item_index = meshes_items.get(item as usize);
        let resolved = local_ids.get(item_index as usize);
        println!(
            "  item={item:<5} element_id={element_id:<8} resolved={resolved:<8} guid={:?}",
            guid_for(resolved)
        );
        if seen.len() >= 8 {
            break;
        }
    }

    if mismatches == 0 {
        println!("\nOK: every pick resolves to its own element (invariant holds).");
        std::process::exit(0);
    } else {
        eprintln!("\nFAIL: {mismatches} samples resolve to the wrong element.");
        std::process::exit(1);
    }
}
