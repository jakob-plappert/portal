# Portal Internal App Development Guide

Read `PORTAL_CONTEXT.md`, `AGENTS.md`, this guide, and the relevant source
before changing the app inventory. This guide describes compile-time internal
applications, not third-party plugins.

## What counts as a Portal internal app

A Portal internal app is a product area deliberately compiled into
`portal-app`. It has a stable `PortalAppId`, one descriptor in
`app_catalog.rs`, an explicit Slint page, concrete Rust state/callback code, and
tests appropriate to its behavior. Portal owns its lifecycle and can review all
code that crosses storage, network, and security boundaries.

Home, Nexus, Compute, and Settings are the current internal apps. Compute and
Settings are shell surfaces over Nexus-owned state and therefore do not own
empty storage directories.

## What not to build

Do not turn an app request into:

- a dynamic plugin loader or marketplace;
- runtime manifests that register arbitrary code;
- shared-library loading, script execution, or JSON command dispatch;
- a dependency-injection framework, event bus, or microservice;
- an app-specific database when an ordinary file or the existing explicit
  storage approach is sufficient;
- an LLM tool with arbitrary filesystem or shell access.

Prefer one enum variant, one descriptor, one concrete state module, and one
explicit UI branch.

## Choosing a stable app ID

Choose a lowercase identifier made only of ASCII letters, digits, `-`, or `_`.
It must be unique and should survive display-name changes. Examples:
`nexus`, `example-tool`, `robotics-lab`.

Add a `PortalAppId` variant and its `stable_id()` and navigation mapping in
`portal-app/src/app_catalog.rs`. Add exactly one `PortalAppDescriptor` in
`PORTAL_APPS`. Catalog validation runs at startup and tests detect duplicate
identities, navigation indices, storage IDs, or invalid path characters.

## Display name versus storage ID

`display_name` is presentation and may change. The stable app ID identifies
the product area. `storage_id` is an optional durable directory name. It should
normally equal the stable app ID, but it is declared separately so a future
display rename never moves user data accidentally.

Use `storage_id: None` when an app has no durable state. Do not create an empty
directory just because the app appears in navigation.

## App-scoped storage

Persistent apps should resolve their root with:

```rust
let app_dir = paths.app_dir("example-tool")?;
```

Do not construct `portaldata/apps/...` by string concatenation. Create only the
subdirectories actually required by the app, during that app's explicit
initialization. Keep large media and user-inspectable artifacts as ordinary
files; use SQLite only when structured querying and durable relationships
justify it. Do not introduce another storage engine without an explicit need.

The shared root (`PortalPaths::shared_dir`) is opt-in. Before using it, define
which app owns writes, which apps may read, and how schema/version changes are
coordinated. App-private data should remain private by default.

## Rust state ownership

Portal/Rust owns application state. Add a concrete state type in a focused
module when the app has meaningful domain behavior. Add it to `AppState` only
when it must share the window lifecycle or existing callback setup. Avoid a
generic `App` trait solely to make unlike apps look uniform.

The UI thread currently shares `AppState` through `Rc<RefCell<AppState>>`:

- `Rc` gives multiple callbacks ownership of one UI-thread state allocation.
- `RefCell` checks otherwise-static borrowing rules at runtime.
- keep `borrow()` and `borrow_mut()` scopes short and never hold them while
  invoking code that may re-enter another callback.

If the new app is independent, a dedicated concrete state value and setup
function can be clearer than expanding unrelated Nexus code.

## Slint UI integration

Add the smallest explicit page branch to `portal-app/ui/main.slint`. Keep
domain data in Rust and expose only small UI-facing properties or model rows.
For lists, declare a Slint struct, convert Rust values into a `VecModel`, and
install a `ModelRc` as existing project and Nexus code do.

Do not put filesystem paths, SQL, provider requests, or business rules in
Slint expressions. Slint renders state and emits callbacks; Rust validates and
acts.

## Navigation integration

The sidebar model is populated from `PORTAL_APPS`. To add an app:

1. add its `PortalAppId` variant;
2. give it a stable ID and navigation index;
3. add its descriptor in display order;
4. extend `from_navigation_index`;
5. add the matching explicit `active-app` page branch in Slint;
6. update the stable-mapping tests.

The integer exists only at the Rust/Slint boundary. Rust code should use
`PortalAppId::navigation_index()` instead of repeating numeric literals.

## Callbacks

Declare narrow, typed Slint callbacks for user actions. Register them in a
concrete Rust setup function, usually beside the app's state/domain code.
Convert Slint strings into owned Rust `String`s when they must outlive the
callback argument. Capture a weak window handle in callbacks retained by the
window to avoid reference cycles.

Return actionable errors through a dedicated status property or result model.
Do not infer control state from user-facing error strings.

## Persistence

Define which data is durable before adding persistence. Keep serialization
schemas small and versioned. Use ordinary files for natural file artifacts and
reuse `NexusStore` for Nexus-owned relational data. A separate app may use its
own app-scoped SQLite database when its data truly requires one, but should not
share Nexus tables merely for convenience.

Never store secrets, media BLOBs, transient UI selection, or arbitrary chat
context without a clear product requirement.

## Background work

Network or expensive filesystem work must not block Slint's UI thread. The
existing code uses `std::thread` and channels, then
`upgrade_in_event_loop` to return owned results to Slint. Follow that concrete
pattern when it fits; do not add an async runtime solely for one task.

Persist durable external IDs before long polling so restart recovery is
possible. Model truthful states and terminal failures. Do not invent progress
percentages that a provider does not expose.

## Secrets

Store API keys and tokens in the OS keyring. Environment variables may be a
documented non-persistent development fallback. Never write secrets to app
directories, SQLite, logs, diagnostics, docs, Slint properties, fixtures, or
Git. Types containing secrets should not derive `Debug`.

## Cross-app shared data

Prefer an explicit typed read/copy boundary over direct access to another
app's private directory. If shared durable data is genuinely required, define
ownership and use `portaldata/shared/` intentionally. A navigation surface may
read another app's state—as Compute reads Nexus jobs—without pretending it
owns a separate database.

## Tests

At minimum, test:

- stable and unique catalog/navigation IDs;
- storage ID validation and expected path resolution;
- domain validation and persistence round trips;
- failure behavior and secret exclusion;
- Rust-to-Slint model conversion when it contains real logic;
- restart/recovery behavior for durable background work.

Avoid brittle screenshots. Tests must not create billable external resources.
Use temporary directories and local HTTP fixtures where needed.

## Documentation

Update `PORTAL_CONTEXT.md` in the same pull request when the app inventory,
architecture, storage, capabilities, worker contracts, integrations, or
security boundaries change. Add focused domain documentation under `docs/`
when operational detail would overwhelm the context file. Keep status language
honest: modeled or visible is not the same as implemented or live-verified.

## Complete documentation-only example: ExampleTool

This example explains the integration points. **Do not copy it into production
unless ExampleTool is actually requested and implemented.**

### 1. Add compile-time identity

Add `ExampleTool` to `PortalAppId`, then extend both mappings:

```rust
Self::ExampleTool => 4,             // navigation_index
Self::ExampleTool => "example-tool", // stable_id
4 => Some(Self::ExampleTool),       // from_navigation_index
```

Append a descriptor:

```rust
PortalAppDescriptor {
    id: PortalAppId::ExampleTool,
    display_name: "ExampleTool",
    subtitle: "A focused example utility",
    storage_id: Some("example-tool"),
},
```

Update the catalog's expected stable mapping test. The existing uniqueness and
storage-validation tests should continue to pass.

### 2. Define app-owned state

Create `portal-app/src/example_tool.rs` with concrete domain types and, if
needed, an `ExampleToolState`. Keep its operations explicit—for example,
`load_items`, `save_item`, and `setup_callbacks`—instead of defining a generic
plugin trait.

If the state shares the main window lifecycle, add:

```rust
mod example_tool;
```

and one concrete field to `AppState`. If it does not need shared state, let its
setup function own the minimal handles it needs.

### 3. Initialize storage only when needed

Resolve the descriptor's storage ID through `PortalPaths`:

```rust
let storage_id = app_catalog::descriptor(PortalAppId::ExampleTool)
    .storage_id
    .expect("ExampleTool owns app-scoped storage");
let root = paths.app_dir(storage_id)?;
std::fs::create_dir_all(&root)
    .map_err(|error| format!("Could not create ExampleTool storage: {error}"))?;
```

Add only required children and decide a migration strategy before changing an
existing path. Do not place ExampleTool state in Nexus's database or directory.

### 4. Add the explicit page

Add an `active-app == 4` branch in `main.slint`. Declare small
`ExampleToolListItem` rows and callbacks such as `save-example-item(string)`.
The sidebar button appears automatically because Rust supplies the catalog
model; the page body remains explicit and reviewable.

### 5. Wire callbacks

Create `example_tool::setup(&window, ...)` in `main.rs`. Validate callback
input in Rust, write through the app's concrete store, and refresh a Slint
model. Use a weak window handle for retained closures and return clear errors
through an ExampleTool status property.

### 6. Handle background work if required

For slow work, start one named concrete worker using `std::thread` and a
channel. Send owned result data back with `upgrade_in_event_loop`. Define
cancel/restart behavior before adding durable jobs. Do not add a general task
bus or async runtime.

### 7. Finish tests and documentation

Test the new stable mapping, `portaldata/apps/example-tool/` resolution,
persistence, invalid inputs, and any background state transitions. Add
ExampleTool to the context status and internal-app tables with an exact
IMPLEMENTED/PARTIAL/PLANNED status, then run the repository validation suite.

## Checklist

- [ ] Read `PORTAL_CONTEXT.md`, `AGENTS.md`, this guide, and relevant source.
- [ ] Choose a unique stable ID and optional validated storage ID.
- [ ] Add one enum variant and one descriptor to `app_catalog.rs`.
- [ ] Preserve explicit navigation mapping and update its tests.
- [ ] Add a concrete Rust module/state owner only as needed.
- [ ] Add one explicit Slint page and narrow callbacks.
- [ ] Keep business rules, filesystem, SQL, and network work in Rust.
- [ ] Create only required app-scoped storage.
- [ ] Put secrets in the keyring, never local data or diagnostics.
- [ ] Define cross-app ownership before using shared data.
- [ ] Test paths, persistence, failures, and model conversion.
- [ ] Update `PORTAL_CONTEXT.md` and focused docs in the same PR.
- [ ] Run fmt, check, tests, clippy, build, and `git diff --check`.
