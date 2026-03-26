# IFC-OWL Converter: Incomplete Export Report

**Date:** 2026-03-18
**Namespace:** `cx_demo_project_a4afcfcef22f`
**Named graph:** `http://localhost:8080/model_279c519d256c/ifcowl`

---

## Problem

Double-clicking IFC elements in the viewer shows only "LBD Properties" (flat key/value pairs like `nameIfcRoot`, `ifcClassification`). Structured property sets (`Pset_WallCommon`, custom psets, etc.) are missing for most elements.

---

## Root Cause

The IFC-OWL converter produced an **incomplete named graph**. Most IFC elements are either entirely absent from the graph or present as bare resources without their property set relations.

The viewer pipeline (svc-graph) works correctly:
1. Resolves clicked GUID → BOT element via LBD graph ✓
2. Follows `owl:sameAs` → IFC-OWL element ✓
3. Queries `IfcRelDefinesByProperties` → property sets — **finds nothing** ✗
4. Falls back to LBD flat properties (always present, always shown) ✓

---

## Numbers

| Metric | Count |
|--------|-------|
| BOT elements in LBD graph | 1,674 |
| BOT elements with `owl:sameAs` to IFC-OWL | 1,674 (100%) |
| BOT elements with reachable IFC property sets | **238 (~14%)** |
| `IfcWall` instances in IFC-OWL graph | 139 |
| `IfcWall` instances with property sets | **55 (~40%)** |
| `IfcWall` instances with zero triples at all | 84+ (e.g. `IfcWall_65669`) |
| IFC-OWL graph total triples | ~13.5M |
| LBD graph total triples | ~347K |

---

## Confirmed Example

**Wall GUID:** `36JfFyTo91oAzncChM$rFv`
**BOT element:** `http://localhost:8080/model_279c519d256c/wall_c64e93fc-7722-41c8-af71-98cad6ff53f9` ← exists, has LBD props
**IFC-OWL element:** `http://localhost:8080/model_279c519d256c/IfcWall_65669` ← **0 triples in the ifcowl graph**

The `owl:sameAs` triple exists in the LBD graph pointing to `IfcWall_65669`, but that resource was never written to the IFC-OWL named graph by the converter.

---

## IFC Version in Graph

The graph uses the **IFC4 ADD2** ontology prefix:
`https://standards.buildingsmart.org/IFC/DEV/IFC4/ADD2/OWL#`

Confirmed from a sample triple:
```
<IfcBuildingElementProxy_10764>
  ifc4add2:globalId_IfcRoot <IfcGloballyUniqueId_587350> .
```

---

## GlobalID Predicates in Use

Three predicates store GlobalIDs across the two graphs:

| Predicate | Graph | Format |
|-----------|-------|--------|
| `http://lbd.arch.rwth-aachen.de/props#globalIdIfcRoot` | LBD | Compressed IFC GUID (e.g. `36JfFyTo91oAzncChM$rFv`) |
| `http://lbd.arch.rwth-aachen.de/props#ifcGlobalIdUncompressed` | LBD | UUID form (e.g. `c64e93fc-7722-41c8-af71-98cad6ff53f9`) |
| `https://standards.buildingsmart.org/IFC/DEV/IFC4/ADD2/OWL#globalId_IfcRoot` | IFC-OWL | Via `express:hasString` blank node |

---

## Fix

Only the IFC-OWL named graph needs to be regenerated. **Do not touch the LBD graph** — it is complete.

### Steps

1. **Re-run the IFC→OWL converter** on the original `.ifc` file with property set export enabled
2. **Drop the old named graph** before re-importing:
   ```sparql
   DROP GRAPH <http://localhost:8080/model_279c519d256c/ifcowl>
   ```
3. **Upload** the new IFC-OWL turtle/RDF into the same named graph IRI

### Verify After Re-import

Run this query — the count should be close to **1,674** (one per element), not 238:

```sparql
PREFIX ifc: <https://standards.buildingsmart.org/IFC/DEV/IFC4/ADD2/OWL#>

SELECT (COUNT(DISTINCT ?el) AS ?withPsets)
FROM <http://localhost:8080/model_279c519d256c/ifcowl>
WHERE {
  ?relDef a ifc:IfcRelDefinesByProperties ;
          ifc:relatedObjects_IfcRelDefinesByProperties ?el .
}
```

### Verify a Specific Element

Check that the previously-broken wall now has triples:

```sparql
SELECT ?p ?o
FROM <http://localhost:8080/model_279c519d256c/ifcowl>
WHERE {
  <http://localhost:8080/model_279c519d256c/IfcWall_65669> ?p ?o
}
```

Should return multiple triples (globalId, name, property set relations, etc.).

### Check Cross-Graph Coverage

Confirm the LBD↔IFC-OWL linkage is working end-to-end:

```sparql
PREFIX ifc: <https://standards.buildingsmart.org/IFC/DEV/IFC4/ADD2/OWL#>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
PREFIX bot: <https://w3id.org/bot#>

SELECT (COUNT(DISTINCT ?el) AS ?total) (COUNT(DISTINCT ?withPset) AS ?withPsets)
FROM <http://localhost:8080/model_279c519d256c/lbd>
FROM <http://localhost:8080/model_279c519d256c/ifcowl>
WHERE {
  ?el a bot:Element ;
      owl:sameAs ?ifcEl .
  OPTIONAL {
    ?relDef ifc:relatedObjects_IfcRelDefinesByProperties ?ifcEl .
    BIND(?el AS ?withPset)
  }
}
```

Target: `withPsets` ≈ `total` (1,674).
