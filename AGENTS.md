Portal is a Rust + Slint learning and production project.

Priorities:
- Keep the architecture simple and explicit.
- Avoid unnecessary abstractions, frameworks, traits, generics, or indirection.
- Prefer readable concrete Rust code.
- Preserve the existing Project -> Shot architecture.
- Do not introduce SQLite, async runtimes, plugin systems, or microservices unless explicitly requested.

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

Portal-specific:
- portal-app owns UI, project data, canon, shots, and orchestration.
- portal-comfy-worker is a dumb compute worker.
- portal-llm-worker is a scene/prompt compiler, not the source of truth.
- portaldata contains local runtime data and models and is not committed to Git.
