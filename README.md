# alx

`alx` is a native, local task artifact store. It keeps tasks, arbitrary artifact types, and text annotations in SQLite. The CLI and the optional web UI use the same storage layer. No daemon is required.

## Install

```bash
cargo install --path .
```

The database is stored at `~/.local/share/alx/alx.db` on a default Linux setup and `~/Library/Application Support/alx/alx.db` on macOS. Platform directory overrides still apply. Set `ALX_DB=/path/to/alx.db` to use a specific file. Parent directories and the schema are created automatically.

## Agent skill

`alx` includes an agent-compatible skill with its CLI reference and workflows.

```bash
alx skill read
alx skill install
```

`skill read` prints the embedded `SKILL.md`. `skill install` writes the same file to the global `~/.agents/skills/alx/SKILL.md` path and prints that path. These commands do not create or open the `alx` database.

## Tasks

```bash
alx task create ARE-1175 <<'EOF'
Investigate the Meilisearch units index.
EOF

alx task read ARE-1175
alx task read 019...
alx task update ARE-1175 <<'EOF'
Revised canonical task statement.
EOF
alx task list
alx task list --json
alx task archive ARE-1175
alx task list --archived
alx task delete ARE-1175 --confirm
alx task search meilisearch
alx task search meilisearch --json
alx task context ARE-1175
```

`task create` prints only the new UUID. Task IDs must not conflict with any existing task UUID. `task read` prints only the stored body. UUID inputs accept equivalent UUID text forms, such as uppercase UUIDs. `task update` replaces the stored body from stdin and refreshes `updated_at`. It has no stdout on success and keeps the task UUID and id. Archive has no stdout and removes the task from the default list and search results. Use `task list --archived` to list archived tasks. Delete permanently removes the task, its artifacts, and its annotations. It requires `--confirm` and has no stdout. Plain `task list` and `task search` print one tab-separated record per line with these columns:

```text
uuid<TAB>id<TAB>updated_at
```

`task context` prints an aggregate Markdown view. Duplicate artifact types remain separate and each heading includes its artifact UUID. Backslashes, tabs, and line breaks in plain tab-separated text fields are escaped as `\\`, `\\t`, `\\r`, and `\\n`.

## Artifacts

```bash
alx artifact create ARE-1175 research \
  --name meilisearch-index-findings.md < research.md
alx artifact read "$ARTIFACT_UUID"
alx artifact update "$ARTIFACT_UUID" < revised.md
alx artifact rename "$ARTIFACT_UUID" pwa-filter-semantics.md
alx artifact list ARE-1175
alx artifact list ARE-1175 --type research
alx artifact list ARE-1175 --json
alx artifact feedback "$ARTIFACT_UUID"
alx artifact feedback "$ARTIFACT_UUID" --json
alx artifact review "$ARTIFACT_UUID"
```

Create prints only the UUID. Read prints only the body. Update and rename have no stdout on success. If `--name` is omitted, create stores a fallback display name such as `research--01a03e73.md`. Plain artifact lists use:

```text
uuid<TAB>type<TAB>name<TAB>updated_at
```

Names are presentation metadata. UUIDs remain the canonical identity for reads, updates, renames, annotations, and references. Duplicate names are allowed. Renaming does not change the UUID, type, body, or references. Artifact types are arbitrary non-empty strings and remain independent from names. More than one artifact of the same type is allowed for a task. `artifact feedback` prints raw unresolved annotation data. `artifact review` prints artifact-neutral review instructions followed by the same canonical unresolved feedback formatting.

Agent instructions generated from task context include this guidance:

> When creating an artifact, provide a short descriptive filename with `--name` when there is an obvious one. Use the artifact UUID for all subsequent operations.

## Annotations

Supported kinds are `comment`, `question`, `scratch`, and `good`.

```bash
alx annotation create "$ARTIFACT_UUID" question \
  --start-offset 10 --end-offset 28 \
  --selected-text 'the selected text' <<'EOF'
Is this assumption valid?
EOF

alx annotation list "$ARTIFACT_UUID"
alx annotation list "$ARTIFACT_UUID" --json
alx annotation list "$ARTIFACT_UUID" --all
alx annotation resolve "$ANNOTATION_UUID"
```

Annotation create reads its optional body from stdin and prints only its UUID. Start and end offsets must be supplied together. Web UI offsets are UTF-16 text positions in the rendered artifact, which matches browser string indexing. Plain annotation lists use `\\N` for absent values and these columns:

```text
uuid<TAB>kind<TAB>start_offset<TAB>end_offset<TAB>resolved_at
```

Resolved annotations are hidden unless `--all` is used. Resolve has no stdout on success.

## Export

```bash
alx dump ./export
alx dump ARE-1175 ./export
alx dump --zip ./export.zip
alx dump ARE-1175 --zip ./export.zip
```

`dump` writes a readable file tree for handoff or backup. Each task gets `TARGET/<task id>/task.md` and one file per artifact under `TARGET/<task id>/<artifact type>/<artifact name>`. The last argument is always the target path; an optional task key before it limits the dump to that task. Without a task key, all tasks are dumped, including archived ones. Annotations are not exported. Dump has no stdout and does not modify the database. Existing files at the target are overwritten.

Ids and names that are unsafe as path components have unsafe characters replaced with `_`, and duplicate names in the same folder get a `--<UUID prefix>` suffix. `--zip` writes one zip archive at the target path instead of a directory tree.

## Web UI

```bash
alx serve
alx serve --bind 192.168.1.10:8080
alx serve --tailscale
```

The default is `127.0.0.1:3000`. `--bind` accepts an explicit `IP:PORT`. Port `0` asks the operating system to select a free port; the listening message shows the selected port. `--tailscale` runs `tailscale ip -4` and binds the first valid IPv4 address on port 3000. The options conflict.

Remote binding has no user authentication in v1. Anyone who can reach the selected IP and port can create and read stored content, update task bodies, manage annotations, archive tasks, and permanently delete tasks. Use a trusted private network and firewall rules. The server rejects non-IP Host headers and cross-origin browser requests to reduce DNS-rebinding and CSRF risk.

The embedded UI browses active and archived tasks and artifacts in type folders. The task list has a **New task** action. Active task pages and artifact type folders have a **New artifact** action. Active task pages also have an **Edit task** action that replaces the stored task body in a Markdown editor. Creation and edit forms accept Markdown bodies, artifact types, and optional artifact filenames. The UI uses artifact names as visible filenames. Task and artifact views have a **Copy** menu. It copies the raw Markdown body, JSON metadata with the matching CLI read command, or only the read command. Task pages include archive and permanent delete actions. Permanent delete requires browser confirmation and removes the task, its artifacts, and its annotations. The UI renders sanitized Markdown, creates annotations from selected rendered text, resolves annotations, and shows unresolved feedback. Mermaid fenced code blocks are rendered with Mermaid 11 loaded from jsDelivr. That CDN request is the only external network request the UI makes, so the UI is not fully offline; if the CDN is unreachable, the diagram source stays visible. Rendered Markdown cannot load images or other remote resources. Editing task bodies in the browser uses `PUT /api/tasks/{key}`. Updating artifact content in the browser is not part of v1.

## Pipeline behavior

Successful data output goes to stdout. Errors and the `serve` listening address go to stderr. There are no table headers, labels, colors, or decorative status lines on stdout. JSON output is a stable serialized array of the complete stored records.

Examples:

```bash
alx artifact read "$ARTIFACT_UUID" | glow
alx artifact feedback "$ARTIFACT_UUID" | pbcopy
alx artifact review "$ARTIFACT_UUID" | pbcopy
alx task context ARE-1175 | nvim -
```
