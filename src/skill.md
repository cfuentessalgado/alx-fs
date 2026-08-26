---
name: alx
description: Manage local tasks, artifacts, and review annotations with the alx CLI. Use when an agent must read task context, store work products, or process artifact feedback in alx.
---

# alx

Use `alx` as a local, persistent task artifact store. Commands write data to stdout and errors to stderr.

## Start a task

Read the complete task context before work:

```bash
alx task context TASK_ID_OR_UUID
```

Create a task only when requested:

```bash
printf '%s\n' 'Task instructions' | alx task create TASK_ID
```

Task creation prints the task UUID. Use `alx task read`, `alx task list`, or `alx task search QUERY` for narrower queries. Add `--json` to list and search commands when structured output is useful.

Archive a completed task only when requested:

```bash
alx task archive TASK_ID_OR_UUID
alx task list --archived
```

Permanent deletion also removes all artifacts and annotations. Run it only when the user explicitly requests permanent deletion:

```bash
alx task delete TASK_ID_OR_UUID --confirm
```

## Manage artifacts

Create an artifact from a file or pipeline:

```bash
alx artifact create TASK_ID_OR_UUID TYPE --name descriptive-name.md < FILE
```

When creating an artifact, provide a short descriptive filename with `--name` when there is an obvious one. Use the artifact UUID for all subsequent operations. Creation prints the artifact UUID.

```bash
alx artifact read ARTIFACT_UUID
alx artifact update ARTIFACT_UUID < REVISED_FILE
alx artifact rename ARTIFACT_UUID new-name.md
alx artifact list TASK_ID_OR_UUID
alx artifact list TASK_ID_OR_UUID --type TYPE --json
```

Artifact types are arbitrary non-empty strings. Multiple artifacts can have the same type or name. The UUID is the canonical identity.

## Process feedback

Get agent-ready review instructions before revising an artifact:

```bash
alx artifact review ARTIFACT_UUID
```

Use `alx artifact feedback ARTIFACT_UUID` for raw unresolved annotation output. Add `--json` when exact annotation fields or offsets are needed. After handling an annotation, resolve it:

```bash
alx annotation resolve ANNOTATION_UUID
```

List annotations with `alx annotation list ARTIFACT_UUID`. Add `--all` to include resolved annotations.

Create feedback only when requested:

```bash
printf '%s' 'Comment text' | alx annotation create ARTIFACT_UUID comment
```

Supported annotation kinds are `comment`, `question`, `scratch`, and `good`. If text offsets are used, supply both `--start-offset` and `--end-offset`.
