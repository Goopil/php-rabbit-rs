# AGENTS.md Repository Guide Design

**Date:** 2026-07-30

## Objective

Create one root `AGENTS.md` that gives coding agents enough repository-specific context to work safely and efficiently in Rabbit RS without duplicating the detailed product design or implementation roadmap.

## Structure

The guide will contain:

1. The inherited RTK instruction so every shell command uses the repository's required command proxy.
2. A short project summary covering the Rust core, native PHP extension, and planned Laravel adapter.
3. A map of the current workspace and links to the authoritative design and implementation plans.
4. The canonical build, formatting, linting, testing, and Composer validation commands, with `scripts/check.sh` as the full quality gate.
5. Rust implementation conventions inferred from the workspace: Rust 2024, Rust 1.96, forbidden unsafe code, workspace Clippy pedantic lints, explicit public documentation, typed errors, bounded queues, and deterministic asynchronous tests.
6. Testing and workflow expectations: test-driven changes, narrow test commands during development, the full gate before completion, logical commits, and preservation of unrelated user changes.
7. Domain invariants that must not regress: at-least-once delivery, bounded publisher buffering and replay, original publish deadlines, fork-safe per-process runtimes, generation-safe acknowledgements, secret redaction, and Lapin isolation behind `Transport`.

## Scope Boundaries

- Keep detailed requirements and milestone status in the existing documents under `docs/plans/`; link to them rather than copying them.
- Describe only commands and paths that exist in the current repository.
- Avoid per-crate `AGENTS.md` files until crate-specific workflows diverge enough to justify them.
- Do not prescribe future Laravel or PHP layouts as though they already exist; distinguish the current scaffold from planned work.
- Do not modify application code, dependencies, or project behavior.

## Verification

Verify that:

- `AGENTS.md` exists at the repository root.
- Every referenced path exists.
- The documented full validation command matches `scripts/check.sh`.
- The guide includes the RTK requirement and the key safety and reliability invariants.
- `git diff --check` reports no whitespace errors.
