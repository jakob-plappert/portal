Portal is a Rust + Slint learning and production project.

Priorities:
- Keep the architecture simple and explicit.
- Avoid unnecessary abstractions, frameworks, traits, generics, or indirection.
- Prefer readable concrete Rust code.
- Preserve the existing Project -> Shot architecture.
- Preserve Portal's compile-time internal-app architecture; do not introduce a plugin system unless explicitly requested.
- Do not introduce additional databases or storage engines without explicit need.
- Reuse `NexusStore` and the existing SQLite architecture for Nexus-owned relational data.
- New persistent apps should normally own storage under `portaldata/apps/<app-id>` through `PortalPaths::app_dir(...)`.
- Do not introduce async runtimes or microservices unless explicitly requested.

Canonical context:
- Before substantial Portal changes, read `PORTAL_CONTEXT.md`.
- Before adding an internal app, also read `docs/APP_DEVELOPMENT_GUIDE.md`.
- Source code remains authoritative when documentation disagrees.
- When a change materially alters architecture, app inventory, storage, capabilities, worker contracts, external integrations, or security boundaries, update `PORTAL_CONTEXT.md` in the same pull request.

Educational comments:
- Comment non-trivial Rust code extensively.
- Explain WHY code exists, not only WHAT it does.
- Explain ownership and borrowing where relevant.
- Explain Option and Result.
- Explain Rc, RefCell, Weak, closures, callbacks, async/await, traits, generics, and lifetimes when introduced.
- Explain the Rust <-> Slint boundary.
- Comments should teach Rust to someone reading the source.

Workflow:
- Inspect the existing code before modifying it.
- Make the smallest coherent change needed.
- Preserve existing behavior unless explicitly changing it.
- Run cargo fmt.
- Run cargo check.
- Fix compiler errors before finishing.
- Summarize changed files and important design decisions.

Git workflow:
- Before starting a task, inspect `git status`.
- Do not start work on top of unrelated uncommitted changes.
- For normal feature work:
  1. Switch to `main`.
  2. Pull the latest state with `git pull --ff-only origin main`.
  3. Create a descriptive feature branch such as `codex/<task-name>`.
- Never develop directly on `main` unless the user explicitly requests a tiny documentation/configuration-only change.
- After implementation:
  - Run the required checks.
  - Inspect `git diff`.
  - Run `git diff --check`.
  - Commit all intended source changes.
  - Push the branch to `origin`.
- If `gh` is available and authenticated, create a pull request against `main`.
- Never merge the pull request automatically unless explicitly requested.
- Never leave completed work only in the local working tree.
- At the end of every completed task, report:
  - Branch name.
  - Commit SHA.
  - Whether the push succeeded.
  - Pull request URL, if created.
- Never commit:
  - `portaldata/`.
  - AI model files.
  - Secrets.
  - `.env` files.
  - Generated build output.
- If push or pull request creation fails, clearly report that instead of claiming the work is synchronized.
- If a task makes no file changes, do not create an empty commit.

Portal-specific:
- portal-app owns the shell, compile-time app catalog, UI, Project -> Shot data, Nexus SQLite/media, generation jobs, and provider orchestration.
- Portal internal apps are explicit source-controlled modules and pages, not dynamically loaded plugins.
- portal-comfy-worker is a dumb compute worker.
- portal-llm-worker is a scene/prompt compiler, not the source of truth.
- portaldata contains local runtime data and models and is not committed to Git.
