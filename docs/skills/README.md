# Skills

Skills extend the agent with tools. The first release supports **declarative skills** that compose existing, validated tools and registered integrations. Arbitrary executable code inside a skill package is never executed.

## Contents

- [Package format](package-format.md) — ZIP layout and installation rules.
- [Manifest schema](manifest-schema.json) — versioned JSON Schema for `skill.json`.
- [Installation lifecycle](installation-lifecycle.md) — upload pipeline and state machine.
- [Permissions](permissions.md) — how skill permissions are declared and enforced.
- [Authoring guide](authoring-guide.md) — how to build a skill package.

## Quick facts

- Skills are installed through the web frontend by uploading a ZIP; no shell access required.
- Installation is asynchronous: the upload returns an identifier, and progress is exposed via the status endpoint and WebSocket.
- Installed skills become available without rebuilding the Docker image or restarting the application.
- Packages are installed into versioned directories (`/data/skills/<skill-id>/<version>/`) and are immutable after validation.
- A skill's availability does not automatically authorize every agent; agent-level permissions and global security rules are evaluated at invocation time.