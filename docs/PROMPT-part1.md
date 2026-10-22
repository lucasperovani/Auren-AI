# Project: Self-Hosted Autonomous AI Agent

## 1. Project Overview

Build a complete, self-hosted autonomous AI agent platform that runs inside a single Docker container and exposes a web interface.

The application must allow users to connect external LLM providers, install skills, authorize external services, create autonomous agents, schedule recurring tasks using natural language, manage persistent memory, and approve sensitive operations.

The system should provide a user experience comparable in concept to modern personal AI agents, while remaining self-hosted, extensible, modular, and easy to deploy.

The application must not be a simple chatbot. It must be capable of performing multi-step tasks, maintaining execution state, recovering interrupted jobs, monitoring conditions, and acting autonomously within user-defined permissions.

### Core principles

1. The entire application must run in a single Docker container.
2. The backend and all source code must be written in English.
3. All identifiers, variable names, function names, class names, database fields, API routes, comments, logs, and internal documentation must use English.
4. Functions and code blocks must have concise, meaningful comments explaining their purpose or non-obvious behavior.
5. Never use single-letter variable names unless required by a mathematical convention or a well-established language idiom.
6. Use descriptive names that clearly communicate their meaning.
7. Keep functions focused and modules well separated.
8. Prefer simple, maintainable solutions over unnecessary abstraction.
9. Use persistent storage for tasks, schedules, approvals, memory, and execution state.
10. Make the application usable immediately after deployment, even before any LLM or external integration has been configured.
11. Implement skills incrementally. Users must be able to install and enable additional skills without rebuilding the application.
12. Enforce security policies in the backend, independently of the LLM's decisions.

The application must be functional, not merely a UI prototype or a collection of placeholder implementations.

---

## 2. Technology Stack

Use the following technologies unless a documented technical limitation justifies an alternative.

### Backend

- Rust.
- Axum for HTTP and WebSocket endpoints.
- Tokio for asynchronous execution.
- SQLx for database access.
- SQLite for persistent storage.
- Serde for serialization.
- Tracing for structured logs.
- A maintained HTTP client for external API requests.
- A maintained OAuth library where appropriate.
- OpenAPI for API documentation.

### Frontend

Build a responsive web application using HTML, CSS, and JavaScript or TypeScript.

Choose a maintainable frontend architecture that works well with the Rust backend.

The interface must support desktop and mobile browsers.

### Deployment

- Docker.
- Docker Compose.
- A multi-stage Dockerfile.
- A non-root runtime user.
- Persistent Docker volumes.
- A container health check.
- No external database requirement.
- No external queue or scheduler requirement.

Use one application container containing the web server, scheduler, task workers, integration runtime, and all required application components.

Do not introduce additional containers for the database, scheduler, frontend, or worker.

External services such as LLM APIs remain external dependencies, not application containers.

---

## 3. Single-Container Architecture

The container must include:

- The Rust application.
- The web frontend.
- The HTTP API.
- The agent runtime.
- The task scheduler.
- The persistent job queue.
- The skill registry.
- The integration framework.
- The approval engine.
- The notification service.
- The memory subsystem.
- The SQLite database.
- The execution-state manager.

Use Tokio to run the application components concurrently.

A single process may host the HTTP server, scheduler, and workers, provided their responsibilities are separated into independent modules and their execution is coordinated safely.

The scheduler and task workers must not depend on the HTTP request lifecycle.

Long-running tasks must continue when the user closes the browser or disconnects from WebSocket.

Persist all important state in SQLite so that the application can recover after a container restart.

Implement graceful shutdown, cancellation, and recovery of interrupted jobs.

The architecture should permit future extraction of the scheduler or workers into separate processes, but this must not be required for the initial deployment.

---

## 4. Configurable Workspace Directory

The application must provide a configurable workspace where the agent can store files, generated artifacts, scripts, and task-specific working data.

### Default workspace

Use a workspace directory inside the application's persistent data directory by default:

`/data/workspace`

This is the default path inside the container, not a path on the host.

The workspace must be created automatically when the application starts.

### Host directory configuration

Allow the user to specify a directory on the host computer to use as the agent's workspace.

For example, the user might configure:

`/home/user/ai-workspace`

The Docker deployment must mount this host directory into the container at a configurable location.

Example configuration:

`AI_WORKSPACE_HOST_PATH=/home/user/ai-workspace`

`AI_WORKSPACE_CONTAINER_PATH=/workspace`

The container should use `/workspace` as its active workspace when a host directory is configured.

If no host directory is configured, use `/data/workspace`.

Document the required volume configuration and ensure the configured host directory exists or can be created safely.

Support a relative path only when its resolution against the deployment directory is explicit and documented.

### Docker volume configuration

Provide a convenient Docker Compose configuration that uses environment variables for the host path.

For example:

`AI_WORKSPACE_HOST_PATH=./workspace`

The corresponding Compose volume should mount the configured directory into the container.

Document the differences between:

- The application data directory.
- The agent workspace.
- Temporary execution directories.
- Skill installation directories.

These directories must have distinct purposes.

### Workspace security

The workspace is the default location for agent-generated files, but it must not automatically grant the agent unrestricted access to the host filesystem.

The application must:

- Restrict file tools to explicitly authorized directories.
- Reject path traversal outside authorized directories.
- Resolve and validate symbolic links where relevant.
- Avoid exposing Docker sockets or host system directories.
- Apply configurable file-size limits.
- Log important file modifications.
- Require approval for destructive or sensitive file operations according to the security policy.

Do not execute scripts found in the workspace automatically.

Code execution must be an explicit, separately permissioned capability.

---

## 5. Web Interface

Create a modern, responsive interface with the following sections.

### Dashboard

Display:

- Agent status.
- Active tasks.
- Scheduled tasks.
- Pending approvals.
- Recent results.
- Failed executions.
- Installed skills.
- Connected integrations.
- LLM provider status.
- Notifications.
- Resource usage where available.

### Chat

Provide a conversational interface where the user can:

- Ask questions.
- Request tasks.
- Schedule recurring work.
- Install or enable skills.
- Authorize integrations.
- Review execution plans.
- Ask the agent to modify existing tasks.
- Pause or cancel work.
- Inspect task results.

The user must be able to express goals naturally without manually filling out scheduling forms.

### Tasks

Provide a task management interface that displays:

- Task name.
- Natural-language objective.
- Structured execution plan.
- Current status.
- Schedule.
- Conditions.
- Configured actions.
- Required skills.
- Required integrations.
- Approval policy.
- Last execution.
- Next execution.
- Execution history.
- Recent results.

Allow the user to edit tasks manually.

Support pausing, resuming, cancelling, deleting, and executing a task immediately.

### Skills

Provide a catalog of installed skills.

Display each skill's name, description, version, permissions, tools, dependencies, and installation status.

Allow the user to install, enable, disable, update, and uninstall skills when permitted.

### Integrations

Allow users to connect multiple accounts for the same provider.

Examples include multiple Google accounts or multiple supported social-media accounts.

Display each connection's authorized scopes, available capabilities, and connection status.

### Approvals

Provide a persistent approval inbox showing operations that require user authorization.

Display the exact operation, its arguments, affected account, expected consequences, and expiration time.

### Settings

Provide configuration for:

- LLM providers.
- Default model.
- Agent limits.
- Workspace directory.
- Installed skills.
- Integration connections.
- Security policies.
- Notifications.
- Task scheduling.
- Data retention.
- Application appearance.

---

## 6. Natural-Language Task Creation

This is one of the most important features.

The user must be able to create, modify, and manage scheduled tasks through ordinary conversation.

The user must not need to know cron syntax, scheduling APIs, database fields, or the internal task schema.

### Example 1: Daily email monitoring

User:

"Every day, check my email and tell me if there is an important message."

The agent should:

1. Identify the requested recurrence.
2. Determine that the Gmail skill is needed.
3. Check whether an appropriate Google account is connected.
4. Determine whether email-reading permissions are available.
5. Ask the user to connect Google if necessary.
6. Ask a clarifying question only if an essential detail cannot reasonably be inferred.
7. Choose a sensible default time if the user did not specify one.
8. Clearly communicate the selected time and timezone.
9. Convert the request into a structured task.
10. Save the task and its schedule.
11. Confirm that the task is configured.
12. Execute the task when scheduled.

The task should retrieve authorized email metadata and message contents, classify potentially important messages, and notify the user.

The agent should not send replies or modify emails unless separately requested and authorized.

### Example 2: Flight-price monitoring and conditional purchase

User:

"Check every weekday at 9 AM whether the flight price has dropped below R$1,000. If it has, buy the ticket for me."

The agent should identify:

- Recurrence: weekdays.
- Time: 9 AM.
- Timezone: the user's configured timezone.
- Condition: ticket price below R$1,000.
- Action: purchase the ticket.
- Required capability: a supported flight-search and booking integration.
- Financial risk: high.
- Approval requirement: explicit authorization for the purchase.

The agent must not invent the departure airport, destination, dates, passenger details, baggage requirements, or payment method.

If essential booking information is missing, ask the user for it before enabling the purchase action.

If the user has not connected a supported booking service, explain that the monitoring or purchase cannot yet be performed through an authorized integration.

Once sufficient information is available, the agent should create a task with a monitoring step and a conditional action.

When the condition is met, the agent should prepare the purchase and request approval before completing the transaction.

The purchase approval must identify the itinerary, passenger, final price, currency, taxes, fees, and other material terms.

If the price changes after approval or the booking details change, invalidate the approval and request a new one.

Never interpret a general request to buy a ticket as permission to bypass the application's required financial-operation safeguards.

### Example 3: Existing task modification

User:

"Change the email check to 7:30 AM."

The agent should identify the existing task and update its schedule.

It should not create a duplicate task.

### Example 4: Pause a task

User:

"Stop checking email while I'm on vacation."

The agent should identify the relevant task, ask a clarification if multiple tasks match, and pause the selected task.

### Example 5: Conditional notification

User:

"Check my calendar every morning and notify me if I have more than three meetings."

The agent should create a recurring task with a calendar-query step and a notification condition.

The notification should only be sent when the condition is satisfied, unless the user requests daily summaries.

---

## 7. Natural-Language Scheduling Engine

Implement a dedicated module that transforms natural-language requests into structured task definitions.

The LLM may interpret the request, but the backend must validate the resulting task before saving it.

The task definition should include:

- Task identifier.
- Name.
- Objective.
- Agent identifier.
- Recurrence rule.
- Timezone.
- Start time.
- Optional end time.
- Conditions.
- Actions.
- Required skills.
- Required integrations.
- Execution limits.
- Retry policy.
- Approval requirements.
- Notification policy.
- Current status.

Represent recurring schedules using a validated scheduling format.

Support:

- One-time execution.
- Daily schedules.
- Weekday schedules.
- Selected weekdays.
- Weekly schedules.
- Monthly schedules.
- Intervals.
- Natural-language dates and times.
- Conditional monitoring.
- Conditional actions.
- User-requested schedule modifications.

Allow cron expressions internally if useful, but do not require the user to write them.

Use a reliable date/time library and store scheduled timestamps consistently.

Use the user's configured timezone rather than assuming the server timezone.

When a request is ambiguous, distinguish between details that can safely receive defaults and details that must be clarified.

For example:

- "Check my email every day" can use a disclosed default time.
- "Buy the cheapest ticket" requires sufficient travel and passenger details.
- "Delete old files" requires a clear definition of old files and an appropriate deletion policy.

After generating a structured task, validate it and save it transactionally.

The agent should provide a concise confirmation summarizing the interpretation, schedule, conditions, and actions.

---

## 8. Persistent Scheduler and Job Execution

Implement a persistent scheduler inside the Rust application.

Do not depend on an external cron service or a separate container.

The scheduler must:

- Read schedules from SQLite.
- Identify due tasks.
- Enqueue task executions.
- Avoid duplicate scheduling.
- Respect paused and cancelled tasks.
- Recover after restarts.
- Apply retry policies.
- Record execution results.
- Support timezone-aware schedules.
- Handle missed executions according to an explicit policy.
- Enforce concurrency limits.

Use a persistent job queue backed by SQLite.

Use transactions and database constraints to protect against duplicate execution claims.

Define how the scheduler behaves when multiple instances accidentally start against the same database, and implement a safe locking or lease mechanism if multi-instance execution is supported.

Do not assume exactly-once execution is universally possible for external operations.

Use idempotency keys when supported and reconcile uncertain outcomes before retrying external writes.

A task may create or modify its own future schedule through the agent's scheduling tools, but every schedule change must be validated and persisted by the backend.

The LLM must never be responsible for keeping the scheduler alive.

---

## 9. Skills Architecture

Treat skills as installable capability packages.

A skill provides the agent with a set of tools, instructions, metadata, and optionally resources needed to perform a specific category of tasks.

Skills must be independent of the core agent runtime.

### Skill package structure

Use a structure similar to:

`skills/skill-name/`

Each skill should contain:

- `skill.json`: machine-readable metadata and permissions.
- `SKILL.md`: English instructions describing when and how the skill should be used.
- Tool definitions.
- Implementation code or references to trusted built-in tools.
- Tests.
- Optional templates and supporting resources.

A manifest should define:

- Skill identifier.
- Name.
- Version.
- Description.
- Required application version.
- Dependencies.
- Required permissions.
- Required integrations.
- Available tools.
- Installation instructions.
- Compatibility information.

Use a documented, versioned skill-manifest schema.

### Skill lifecycle

Support:

- Discovery.
- Installation.
- Validation.
- Enabling.
- Disabling.
- Updating.
- Uninstallation.
- Dependency checks.
- Permission review.

The system must work without optional skills installed.

Users should be able to add skills incrementally.

Installing a skill must not require rebuilding the main Docker image.

Persist installed skills in the application data directory.

### Skill security

A skill must declare its capabilities and required permissions.

The application must validate those declarations and enforce permissions at runtime.

Do not assume that a skill is safe because it has a valid manifest or because the LLM recommends it.

Treat skill instructions as untrusted input unless the skill has been explicitly trusted.

Do not allow a skill to silently grant itself new permissions.

Installing executable code must require an explicit user action and appropriate security checks.

Prefer declarative skills that compose existing tools before introducing arbitrary executable plugins.

For the first version, support a stable skill interface and built-in implementations.

Later, add a sandboxed plugin runtime if dynamic third-party executable skills become necessary.

Do not implement unrestricted dynamic code execution inside the main application process.

---

## 10. Initial Built-In Skills

Implement the following skills progressively.

### 10.1. File Management Skill

Capabilities:

- List files within authorized directories.
- Read files.
- Create files.
- Edit files.
- Search file contents.
- Organize workspace files.
- Produce reports and summaries.

Enforce path restrictions and file-size limits.

Require approval for destructive operations when appropriate.

### 10.2. Web Research Skill

Capabilities:

- Search the web through a configurable provider.
- Open permitted public URLs.
- Extract relevant information.
- Summarize results.
- Compare sources.
- Produce research reports.
- Store reports in the workspace.

Do not bypass authentication, access controls, or website restrictions.

Treat retrieved content as untrusted input.

Apply SSRF protections and restrict access to private or internal network destinations.

### 10.3. Gmail Skill

Capabilities:

- Search authorized messages.
- Read authorized message contents.
- Identify potentially important messages.
- Summarize conversations.
- Create drafts.
- Send messages when supported and authorized.

Support recurring email monitoring.

Do not require sending or modifying messages for read-only tasks.

### 10.4. Google Calendar Skill

Capabilities:

- Read events.
- Search upcoming events.
- Identify scheduling conflicts.
- Create events.
- Update events.
- Cancel events.

Support scheduled reminders and calendar monitoring.

### 10.5. Google Photos Skill

Implement capabilities supported by the current official API and its applicable access restrictions.

Do not promise unrestricted access to all existing photos unless the API actually provides it.

### 10.6. Flight Monitoring Skill

Separate flight-price monitoring from purchasing.

Monitoring capabilities:

- Search authorized travel providers.
- Query itineraries.
- Retrieve current prices.
- Compare results.
- Check user-defined price conditions.
- Notify the user.

Purchase capabilities:

- Prepare a booking.
- Validate passenger details.
- Present the complete itinerary and final price.
- Request explicit approval.
- Complete the purchase through a supported provider after approval.
- Record the outcome.

Do not scrape private booking sessions or store payment credentials without a specifically designed, secure payment integration.

If no supported booking integration exists, provide monitoring and notification only.

### 10.7. Instagram Skill

Use official Meta APIs and implement only operations supported by the applicable API, account type, and permissions.

Support authorized account information and content operations where available.

Publishing must use the approval engine by default.

Do not implement credential scraping or mechanisms that bypass platform restrictions.

---

## 11. LLM Provider Architecture

Support multiple configurable LLM providers.

Initial implementations:

- OpenAI API.
- Google Gemini API.
- OpenAI-compatible APIs.
- Ollama.

Create a common provider interface for:

- Sending messages.
- Receiving structured responses.
- Tool calling.
- Streaming where supported.
- Token-usage reporting.
- Error handling.
- Timeout handling.
- Retry behavior.

Allow each agent to select a provider and model.

Do not assume all models support tool calling or structured output equally.

Validate model capabilities and provide clear errors when required features are unavailable.

Support provider-specific configuration without leaking provider-specific logic into the core agent runtime.

Never expose API keys to the frontend after storage or to the LLM through prompts or tool outputs.

---

## 12. Agent Runtime

Implement a bounded agent execution loop.

The runtime should:

1. Load the task.
2. Load the agent's instructions.
3. Retrieve relevant memory.
4. Identify available skills.
5. Identify authorized tools.
6. Generate or update an execution plan.
7. Request the next step from the LLM.
8. Validate the requested action.
9. Apply the permission policy.
10. Request approval if required.
11. Execute the authorized tool.
12. Persist the result.
13. Evaluate whether the task is complete.
14. Continue until completion or a defined stopping condition.

Persist the state after important transitions.

Implement limits for:

- Iterations.
- Execution time.
- Tool calls.
- LLM calls.
- Concurrency.
- Retry attempts.
- Resource consumption where measurable.

The runtime must support pause, resume, cancellation, failure, timeout, and recovery.

Prevent infinite loops and uncontrolled tool invocation.

A failed tool call must not automatically imply that the whole task should be repeated.

---

## 13. Approval and Risk Engine

Implement a centralized authorization engine.

Classify operations using deterministic policies based on tool identity, arguments, affected resources, and configured permissions.

The LLM may provide an explanation of the expected risk, but it must not decide whether its own actions are authorized.

Default policies:

- Authorized read operations: automatic.
- Local reversible operations: automatic when permitted.
- Draft creation: automatic when permitted.
- Sending external messages: approval required by default.
- Social-media publishing: approval required by default.
- Destructive remote changes: approval required.
- Purchases and financial transactions: explicit approval required.
- Access-control changes: restricted.
- Prohibited operations: blocked.

Allow users to configure appropriate low-risk permissions.

Require fresh approval when material parameters change.

Approval records must include:

- Operation identifier.
- Task identifier.
- Tool identifier.
- Canonical operation arguments.
- Affected account.
- Estimated consequences.
- Creation time.
- Expiration time.
- Approval status.
- Approving user.
- Execution result.

An approval must apply only to the operation that was presented to the user.

If the final price, recipient, destination, or other critical parameters change, invalidate the approval.

Never treat silence, a timeout, or a failed notification as approval.

---

## 14. External Integrations and Credentials

Implement integrations as independent modules.

Use OAuth where appropriate.

Support multiple connected accounts per provider.

Use least-privilege scopes and request additional permissions only when required.

Store credentials using authenticated encryption.

Keep encryption keys separate from encrypted records.

Use secure session management and validate OAuth state.

Use PKCE when appropriate to the selected OAuth flow.

Support token refresh and revocation handling.

Never log secrets.

Never return tokens to the frontend or LLM.

Allow users to disconnect integrations and remove stored credentials.

Document which integrations require manually configured developer applications.

Do not claim that an integration works until its real authentication and API operations have been implemented and tested.

---

## 15. Security Requirements

Implement:

- Local administrator authentication.
- Secure password hashing.
- Session expiration.
- Login rate limiting.
- CSRF protection where applicable.
- Secure cookies.
- Input validation.
- Output encoding.
- OAuth protections.
- Encrypted credential storage.
- Path traversal prevention.
- SSRF protections.
- Tool permission enforcement.
- Approval enforcement.
- Resource limits.
- Audit logging.
- Secure error handling.

Do not expose the Docker socket.

Do not run the application as root.

Do not execute arbitrary shell commands by default.

Do not permit skills to bypass the authorization engine.

Do not trust instructions contained in emails, documents, web pages, or other external data.

Do not allow the agent to read arbitrary host files simply because a host workspace is mounted.

Require explicit configuration before exposing the application beyond localhost.

Document reverse-proxy and HTTPS requirements for public deployments.

---

## 16. Database and Persistence

Use SQLite with versioned migrations.

Create tables for:

- Users and sessions.
- Agents and instructions.
- Conversations and messages.
- Tasks and schedules.
- Task runs and execution steps.
- Persistent queued jobs.
- LLM providers and models.
- Installed skills.
- Skill permissions.
- Integrations and connected accounts.
- Encrypted credentials.
- Tool definitions.
- Tool permissions.
- Approval requests and decisions.
- Memory records.
- Notifications.
- Audit events.
- Application settings.

Use foreign keys, appropriate indexes, and database constraints.

Use transactions for related state changes.

Persist task definitions, schedules, approvals, and execution progress.

Provide backup and restoration procedures.

Store all application data in a persistent Docker volume.

---

## 17. Notifications

Implement persistent in-app notifications and real-time updates.

Notifications should cover:

- Task completion.
- Task failure.
- Approval requests.
- Important email findings.
- Triggered monitoring conditions.
- Price thresholds.
- Expiring authorizations.
- Integration failures.

Prepare interfaces for email and Web Push notifications.

Notification delivery must not determine whether an action is authorized.

An approval must be recorded by the backend through an authenticated operation.

---

## 18. Docker and Deployment

Provide a complete repository with:

- `Dockerfile`.
- `docker-compose.yml`.
- `.dockerignore`.
- `.env.example`.
- `README.md`.
- Backend source code.
- Frontend source code.
- Database migrations.
- Built-in skills.
- Automated tests.
- Installation scripts where appropriate.

Use a multi-stage Docker build.

Run the application using a non-root user.

Serve the frontend and API from the same application server.

Use a single container for the initial deployment.

Persist application data and skills through Docker volumes.

Configure the workspace using the documented host-path mechanism.

Provide a health check.

Support graceful shutdown and recovery after restart.

Do not store secrets in the image or commit them to the repository.

The default installation must work without an LLM API key.

The user should be able to configure providers through the web interface after starting the container.

---

## 19. Testing Requirements

Implement automated tests for:

- Application startup.
- Database migrations.
- Authentication.
- LLM configuration.
- Tool execution.
- Natural-language task creation.
- Schedule validation.
- Task modification.
- Task cancellation.
- Persistent scheduling.
- Recovery after restart.
- Duplicate execution prevention.
- Skill installation and validation.
- Skill permission enforcement.
- OAuth state validation.
- Credential encryption.
- Approval expiration.
- Approval invalidation after parameter changes.
- Purchase approval enforcement.
- Integration disconnection.
- Workspace path restrictions.
- SSRF protections.
- Notification persistence.
- Failure recovery.

Use mocks for external APIs.

The core test suite must not require real Google, Meta, travel-provider, or LLM accounts.

Provide integration tests that verify the main user workflows end to end.

---

## 20. Development Plan

Implement the project in the following order.

### Phase 1: Application foundation

Create the Rust project, frontend, database, authentication, Dockerfile, Docker Compose, and configurable workspace.

Deliverable: a working web application that starts in one container.

### Phase 2: LLM integration

Implement provider configuration, model selection, chat, and the common LLM interface.

Deliverable: the user can configure an external LLM and interact with an agent.

### Phase 3: Tool Registry and agent runtime

Implement tool definitions, validation, execution, state persistence, and execution limits.

Deliverable: the agent can perform authorized operations.

### Phase 4: Natural-language tasks

Implement task interpretation, schedule generation, clarification questions, task confirmation, and manual task editing.

Deliverable: the user can request recurring work conversationally.

### Phase 5: Scheduler and recovery

Implement persistent scheduling, the job queue, recurring execution, cancellation, and recovery after restarts.

Deliverable: tasks run independently of the browser session.

### Phase 6: Approval engine

Implement risk policies, approval requests, notifications, and safe resumption of paused tasks.

Deliverable: sensitive actions cannot execute without the required authorization.

### Phase 7: Skill system

Implement skill manifests, validation, installation, enabling, disabling, dependency checks, and permission management.

Deliverable: users can extend the agent with additional skills without rebuilding the container.

### Phase 8: Initial integrations

Implement File Management and Web Research first, followed by Gmail, Calendar, supported Photos operations, and other integrations.

Implement flight monitoring before flight purchasing.

Do not simulate unsupported API operations.

### Phase 9: Security and reliability

Complete automated tests, error handling, resource limits, credential protection, recovery behavior, and documentation.

Deliverable: a fully functional, documented application suitable for controlled deployment.

---

## 21. Code Quality Requirements

All code must be written in English.

Use meaningful variable names, function names, type names, database fields, and API identifiers.

Never use single-letter variable names except for conventional mathematical notation or simple iterator variables where appropriate.

Every function and logical code block must include concise comments explaining its purpose or non-obvious behavior.

Avoid redundant comments that merely restate individual lines.

Keep functions small and focused.

Use clear module boundaries and explicit interfaces.

Avoid monolithic source files.

Do not duplicate business logic across the API, frontend, scheduler, and agent runtime.

Do not silently ignore errors.

Do not replace unimplemented functionality with fake successful responses.

Use consistent formatting and run the appropriate Rust formatter and linter.

Add tests for critical business rules and security invariants.

Document architectural decisions where they affect future development.

---

## 22. Completion Criteria

The project is complete when:

1. It starts using a single Docker container.
2. The web interface and API are served by the same application.
3. The workspace can be mapped to a host directory.
4. The user can configure an external LLM.
5. The user can create and manage agents.
6. The user can request recurring tasks in natural language.
7. The system can infer reasonable schedule defaults and ask necessary clarification questions.
8. The user can inspect and manually edit generated tasks.
9. Tasks persist and execute independently of browser sessions.
10. The scheduler recovers after container restarts.
11. The agent can use installed skills.
12. Skills can be installed incrementally.
13. Sensitive operations require the appropriate approval.
14. Credentials are encrypted and isolated from the LLM.
15. Supported integrations use real APIs and authentication.
16. Automated tests cover critical workflows.
17. The README documents installation, configuration, security, backups, integrations, and known limitations.

Begin by inspecting the development environment and creating the project structure.

Implement Phase 1 first. Compile the project, run tests, fix errors, and verify the Docker deployment before continuing.

Proceed incrementally through the remaining phases.

Do not stop after writing an architectural plan. Implement the actual files and functionality.

If a provider requires external registration or approval, implement the integration code that can be completed locally and document the exact manual configuration that remains necessary.