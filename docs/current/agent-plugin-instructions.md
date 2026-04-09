# Agent Instructions for Building Modules

Use this when delegating module implementation to an AI coding agent.

## Required Inputs

- Module id in kebab-case, e.g. `voxels`.
- Stage: currently `produce`.
- Output contract, e.g. `topology-triples`.
- Failure policy: `optional` or `required`.
- Parallelism mode target.

## Agent Prompt Template

```text
You are implementing a new IFC2LBD-Neo producer module.

Module id: custom-<id>-producer
Display name: <Display Name>
Stage: Produce
Outputs: topology-triples
Failure policy: Optional
Parallelism: ParallelByPartition

Tasks:
1. Run `python3 scripts/scaffold_producer_plugin.py --id <id> --display-name "<Display Name>"`.
2. Add the new crate to workspace Cargo.toml members.
3. Register the manifest in crates/ifc2lbd-cli/src/pipeline_plugins.rs.
4. Add one executor entry in crates/ifc2lbd-cli/src/topology_plugin.rs (TOPOLOGY_EXECUTORS).
5. Ensure module can be activated with `--module custom-<id>-producer`.
   - Add typed config parsing and validation for any new `--module-opt <module-id>.<key>=<value>` keys.
6. Run:
   - cargo check -p ifc2lbd-cli
   - cargo test -p ifc2lbd-cli pipeline_plugins
   - DigitalHub smoke benchmark with explicit N-Quads serializer module.
7. Report wall time and max RSS versus baseline.

Constraints:
- Do not commit benchmark payloads or virtualenv directories.
- Keep module failure isolated if policy is Optional.
- Keep execution parallel; do not serialize producer modules.
```

## Activation Checklist

1. `target/release/ifc2lbd-neo --list-modules`
2. `target/release/ifc2lbd-neo <ifc> --module <id> --show-module-plan`
3. Full run with `--module neo-nquads-serializer` on DigitalHub.
4. Optional-failure simulation if module policy is `Optional`.
