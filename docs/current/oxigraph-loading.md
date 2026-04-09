# Oxigraph Loading

This page documents loading IFC2LBD-Neo N-Quads output into Oxigraph.

## Chunked Output Load

If you run with chunking, IFC2LBD-Neo writes manifest files per stream (`lbd`, `ifcowl`, `topology`).

Load each stream directly from its manifest:

```bash
jq -r '.files[].file' out-ifcowl.manifest.json | while read -r f; do
  oxigraph_server load --file "$f" --format nquads
done

jq -r '.files[].file' out-lbd.manifest.json | while read -r f; do
  oxigraph_server load --file "$f" --format nquads
done

jq -r '.files[].file' out-topology.manifest.json | while read -r f; do
  oxigraph_server load --file "$f" --format nquads
done
```

## Notes

- In `nquads` mode, IfcOWL is emitted automatically.
- If topology is not enabled, there will be no topology manifest.
- Use `--quad-chunking cores` for practical default parallel chunking.
