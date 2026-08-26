# alx

`alx` is a native, local task artifact store. It keeps tasks, arbitrary artifact types, and text annotations in SQLite. The CLI and the optional web UI use the same storage layer. No daemon is required.

## Install

```bash
cargo install --path .
```

The database is stored at `~/.local/share/alx/alx.db` on a default Linux setup and `~/Library/Application Support/alx/alx.db` on macOS. Platform directory overrides still apply. Set `ALX_DB=/path/to/alx.db` to use a specific file. Parent directories and the schema are created automatically.

## Tasks

```bash
alx task create ARE-1175 <<'EOF'
Investigate the Meilisearch units index.
EOF

alx task read ARE-1175
alx task read 019...
alx task list
alx task list --json
alx task search meilisearch
alx task search meilisearch --json
alx task context ARE-1175
```

`task create` prints only the new UUID. Task IDs must not conflict with any existing task UUID. `task read` prints only the stored body. UUID inputs accept equivalent UUID text forms, such as uppercase UUIDs. Plain `task list` and `task search` print one tab-separated record per line with these columns:

```text
uuid<TAB>id<TAB>updated_at
```

`task context` prints an aggregate Markdown view. Duplicate artifact types remain separate and each heading includes its artifact UUID. Backslashes, tabs, and line breaks in plain tab-separated text fields are escaped as `\\`, `\\t`, `\\r`, and `\\n`.

## Artifacts

```bash
alx artifact create ARE-1175 research < research.md
alx artifact read "$ARTIFACT_UUID"
alx artifact update "$ARTIFACT_UUID" < revised.md
alx artifact list ARE-1175
alx artifact list ARE-1175 --type research
alx artifact list ARE-1175 --json
alx artifact feedback "$ARTIFACT_UUID"
alx artifact feedback "$ARTIFACT_UUID" --json
```

Create prints only the UUID. Read prints only the body. Update has no stdout on success. Plain artifact lists use:

```text
uuid<TAB>type<TAB>updated_at
```

Artifact types are arbitrary non-empty strings. More than one artifact of the same type is allowed for a task. Feedback includes unresolved annotations only.

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

## Web UI

```bash
alx serve
alx serve --bind 192.168.1.10:8080
alx serve --tailscale
```

The default is `127.0.0.1:3000`. `--bind` accepts an explicit `IP:PORT`. Port `0` asks the operating system to select a free port; the listening message shows the selected port. `--tailscale` runs `tailscale ip -4` and binds the first valid IPv4 address on port 3000. The options conflict.

Remote binding has no user authentication in v1. Anyone who can reach the selected IP and port can read stored content and manage annotations. Use a trusted private network and firewall rules. The server rejects non-IP Host headers and cross-origin browser requests to reduce DNS-rebinding and CSRF risk.

The embedded UI browses tasks and artifacts, renders sanitized Markdown, creates annotations from selected rendered text, resolves annotations, and shows unresolved feedback. Rendered Markdown cannot load images. Browser editing is not part of v1.

## Pipeline behavior

Successful data output goes to stdout. Errors and the `serve` listening address go to stderr. There are no table headers, labels, colors, or decorative status lines on stdout. JSON output is a stable serialized array of the complete stored records.

Examples:

```bash
alx artifact read "$ARTIFACT_UUID" | glow
alx artifact feedback "$ARTIFACT_UUID" | pbcopy
alx task context ARE-1175 | nvim -
```
