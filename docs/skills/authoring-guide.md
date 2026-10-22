# Skill Authoring Guide

This guide walks through creating a declarative skill package.

## 1. Plan the tools

Decide which existing registered tools your skill composes. A declarative skill references validated tools (for example, a Gmail search tool and a text summarization capability); it cannot define arbitrary new executable behavior.

## 2. Create the package layout

```text
gmail-monitor/
├── skill.json
├── SKILL.md
├── tools/
│   ├── search_messages.json
│   └── summarize_messages.json
├── resources/
│   └── classification-guidelines.md
└── tests/
    └── examples.json
```

## 3. Write the manifest

`skill.json` must validate against [manifest-schema.json](manifest-schema.json). Complete example for a declarative `gmail-monitor` skill:

```json
{
  "schema_version": "1",
  "id": "gmail-monitor",
  "name": "Gmail Monitor",
  "version": "1.0.0",
  "description": "Searches Gmail messages and produces summaries of recent activity.",
  "entrypoint": "tools/",
  "implementation_type": "declarative",
  "author": "Auren Team",
  "required_integrations": ["gmail"],
  "required_permissions": ["gmail.read"],
  "tools": [
    {
      "tool_name": "search_messages",
      "description": "Search Gmail messages with a query.",
      "input_schema": {
        "type": "object",
        "properties": {
          "query": { "type": "string" },
          "max_results": { "type": "integer", "minimum": 1, "maximum": 50 }
        },
        "required": ["query"]
      },
      "risk_class": "low",
      "implementation_type": "builtin_ref",
      "implementation_ref": "gmail.search"
    },
    {
      "tool_name": "summarize_messages",
      "description": "Summarize a list of message bodies.",
      "input_schema": {
        "type": "object",
        "properties": {
          "messages": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["messages"]
      },
      "risk_class": "medium",
      "implementation_type": "builtin_ref",
      "implementation_ref": "text.summarize"
    }
  ],
  "dependencies": []
}
```

Notes:

- `id` must be lowercase kebab-case; it becomes the installation directory name.
- `version` is semver-like.
- The manifest cannot grant itself permissions; `required_permissions` are requests validated against the security policy.
- Every `implementation_ref` must exist in the tool registry at validation time.

## 4. Write tool definitions

Each file in `tools/` describes one tool with a JSON Schema `input_schema`. Keep schemas strict: required fields, bounded values, and explicit types. The risk class drives the approval behavior — see [permissions](permissions.md).

## 5. Declare integrations and permissions

List every provider the skill needs in `required_integrations` (for example `"gmail"`). If an integration is not connected, installation may succeed but the skill remains partially configured and unavailable for execution; the UI makes this state explicit.

## 6. Add tests

`tests/examples.json` contains example invocations and expected outcomes used during validation:

```json
{
  "examples": [
    {
      "tool": "search_messages",
      "input": { "query": "from:team@example.com", "max_results": 10 },
      "expect": "success"
    }
  ]
}
```

## 7. Package and upload

Zip the top-level directory (no absolute paths, no `..` entries, no symlinks) and upload it through the web frontend or:

```sh
curl -X POST http://localhost:8080/api/v1/skills/uploads \
  -H 'authorization: ******' \
  -F 'file=@gmail-monitor.zip'
```

Poll `GET /api/v1/skills/uploads/{upload_id}` (or listen for `skill.*` WebSocket events) until the state is `installed` or `failed`.

## 8. Enable for an agent

After installation, enable the skill for an agent and configure per-tool permissions. Missing required integrations leave the skill partially configured — the UI makes this state explicit.

## Validation checklist

Your package must pass every pipeline stage: archive rules, manifest schema, tool schema validity, resolvable tool references, compatible permissions, and bounded sizes. See [installation lifecycle](installation-lifecycle.md) and [package format](package-format.md).