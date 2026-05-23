# Oxigraph Loading

This page documents loading IFC2LBD-Neo N-Quads output into Oxigraph.

## Chunked Output Load

If you run with chunking, IFC2LBD-Neo writes manifest files per stream (`lbd`, `ifcowl`).

Load each stream directly from its manifest:

```bash
jq -r '.files[].file' out-ifcowl.manifest.json | while read -r f; do
  oxigraph_server load --file "$f" --format nquads
done

jq -r '.files[].file' out-lbd.manifest.json | while read -r f; do
  oxigraph_server load --file "$f" --format nquads
done
```

## Notes

- IfcOWL is emitted only when `neo-ifcowl-producer` is enabled.
- Use `--module-opt neo-nquads-serializer.chunking=cores` for practical default parallel chunking.
