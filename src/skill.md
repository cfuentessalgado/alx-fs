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

Task creation prints the task UUID. Use `alx task context` as the preferred broad read. Use `alx task read TASK_ID_OR_UUID` as the narrow source of truth for the task body. Use `alx task list` and `alx task search QUERY` to find tasks. Add `--json` to list and search commands when structured output is useful.

Update a task body only when the task instructions change:

```bash
alx task update TASK_ID_OR_UUID < REVISED_FILE
```

Update has no stdout on success. The task UUID and id stay the same.

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

Use `alx artifact feedback ARTIFACT_UUID` for raw unresolved annotation output. Add `--json` when exact annotation fields or offsets are needed. Resolve an annotation only after its feedback has been addressed or deliberately declined:

```bash
alx annotation resolve ANNOTATION_UUID
```

List annotations with `alx annotation list ARTIFACT_UUID`. Add `--all` to include resolved annotations.

Create feedback only when requested:

```bash
printf '%s' 'Comment text' | alx annotation create ARTIFACT_UUID comment
```

Supported annotation kinds are `comment`, `question`, `scratch`, and `good`. If text offsets are used, supply both `--start-offset` and `--end-offset`.

## Export

Write a readable file tree for backup, inspection, or leaving `alx`:

```bash
alx dump ./export
alx dump ARE-1175 ./export
```

The last argument is always the target path; an optional task key before it limits the dump to that task. Without a task key, all tasks are dumped, including archived ones. Each task gets `TARGET/<task id>/task.md` and one file per artifact under `TARGET/<task id>/<type>/<artifact name>`. Annotations are not exported. Add `--zip` to write one zip archive at the target path instead of a directory tree:

```bash
alx dump --zip ./export.zip
```

Dump has no stdout and does not modify the database.
