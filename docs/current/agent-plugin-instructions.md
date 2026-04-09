# Agent Instructions for Building Plugins

Use this when delegating plugin implementation to an AI coding agent.

## Required Inputs

- Plugin id in kebab-case, e.g. `voxels`.
- Stage: currently `produce`.
- Output contract, e.g. `topology-triples`.
- Failure policy: `optional` or `required`.
- Parallelism mode target.

## Agent Prompt Template

```text
You are implementing a new IFC2LBD-Neo producer plugin.

Plugin id: custom-<id>-producer
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
5. Ensure plugin can be activated with `--enable-plugin custom-<id>-producer`.
6. Run:
   - cargo check -p ifc2lbd-cli
   - cargo test -p ifc2lbd-cli pipeline_plugins
   - model A smoke benchmark in nquads mode.
7. Report wall time and max RSS versus baseline.

Constraints:
- Do not commit benchmark payloads or virtualenv directories.
- Keep plugin failure isolated if policy is Optional.
- Keep execution parallel; do not serialize producer plugins.
```

## Activation Checklist

1. `target/release/ifc2lbd-neo --list-plugins`
2. `target/release/ifc2lbd-neo <ifc> --enable-plugin <id> --show-pipeline-plan`
3. Full run in `nquads` mode with model A.
4. Optional-failure simulation if plugin policy is `Optional`.
