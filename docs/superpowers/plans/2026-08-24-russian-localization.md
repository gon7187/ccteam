# Russian Localization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make all ccteam-authored web, Telegram, and CLI copy Russian by default without changing command, flag, API, vendor, or filesystem contracts.

**Architecture:** Extend the existing web `Lang`/`I18N` seam to a complete `ru | zh | en` table and let every web formatter/control consume that language. Keep IM and CLI protocol-free: translate only their static ccteam-authored text in place, leaving command tokens, interpolation values, JSON, and forwarded vendor output byte-for-byte intact.

**Tech Stack:** React 19 + TypeScript + Vitest + Vite; Rust + Clap + anyhow + Tokio; Cargo workspace; embedded Vite bundle.

## Global Constraints

- Fresh browser settings default to `ru`; saved `zh` and `en` remain valid.
- Telegram command names and argument grammar stay ASCII and unchanged.
- CLI subcommands, flags, JSON schema, API fields, paths, vendor/model ids, snippets, and external/vendor text are not translated.
- No new localization dependency, daemon endpoint, configuration key, or per-chat language feature.
- Web output uses `ru-RU`; `zh-CN` and `en-US` behavior stays intact.
- Work stays on `codex/russian-localization`, based on `origin/dev`; never push directly to `main`.
- Every code task is test-first; do not run a second pytest-equivalent test job in parallel.

---

## File Structure

| Path | Responsibility |
| --- | --- |
| `crates/ccteam-web/web/src/lib/i18n.ts` | Canonical three-language UI dictionary, inline/parameterized text, and browser locale mapping. |
| `crates/ccteam-web/web/src/hooks/useWebSettings.ts` | Persisted `Lang` value and Russian fresh-profile default. |
| `crates/ccteam-web/web/src/{components,pages,lib}/**/*.{ts,tsx}` | Existing hard-coded shell copy, accessibility labels, and direct two-language branches consume the canonical locale. |
| `crates/ccteam-web/web/src/**/*.{test.ts,test.tsx}` | Unit/SSR proof of Russian dictionary parity, selected controls, time formatting, and no regression of zh/en. |
| `crates/ccteam-im/src/gateway.rs` | Telegram menu/help/receipts/errors that the gateway owns. |
| `crates/ccteam-im/src/{onboarding,outbound_format,progress,scheduled,transport/providers/telegram}.rs` | Other ccteam-owned Telegram-facing notices and menu registration copy. |
| `crates/ccteam-cli/src/{main,commands,daemon_cli,doctor,legacy_takeover,update,clipboard}.rs` | Clap help and ccteam-authored terminal output/errors. |
| `crates/ccteam-web/web/src/lib/i18n.test.ts`, `crates/ccteam-im/src/gateway.rs`, `crates/ccteam-cli/src/main.rs` | Focused regression assertions for the language contract and stable command grammar. |

### Task 1: Establish the Russian web-language contract

**Files:**

- Modify: `crates/ccteam-web/web/src/lib/i18n.ts:8-695`
- Modify: `crates/ccteam-web/web/src/hooks/useWebSettings.ts:5-43`
- Modify: `crates/ccteam-web/web/src/lib/i18n.test.ts:1-68`
- Modify: `crates/ccteam-web/web/src/pages/SettingsView.test.tsx:203-224`
- Modify: `crates/ccteam-web/web/src/components/AvatarMenu.test.tsx:27-94`

**Interfaces:**

- Consumes: existing `Lang`, `I18N`, `t`, `makeT`, `tr`, and `navLabel` callers.
- Produces: `Lang = "ru" | "zh" | "en"`; `WEB_LOCALE: Record<Lang, "ru-RU" | "zh-CN" | "en-US">`; a complete `I18N.ru`; a default persisted language of `"ru"`.

- [ ] **Step 1: Write the failing web-language tests.**

  Extend `i18n.test.ts` before changing production code:

  ```ts
  it("keeps Russian, Chinese, and English dictionaries in lockstep", () => {
    const keys = Object.keys(I18N.ru).sort();
    expect(keys).toEqual(Object.keys(I18N.zh).sort());
    expect(keys).toEqual(Object.keys(I18N.en).sort());
  });

  it("resolves Russian text and locale", () => {
    expect(t("ru", "homeTitle")).toBe("За работу!");
    expect(WEB_LOCALE.ru).toBe("ru-RU");
    expect(tStopped("ru", "s9")).toContain("s9");
  });
  ```

  Change the pure `GeneralPanel` and `AvatarPopover` fixtures to accept
  `Lang`, render `ru`, and assert `Русский`, Russian settings copy, and the
  active `data-testid="lang-ru"` control.

- [ ] **Step 2: Run the focused tests and observe RED.**

  Run:

  ```bash
  cd crates/ccteam-web/web && npm run test:unit -- src/lib/i18n.test.ts src/pages/SettingsView.test.tsx src/components/AvatarMenu.test.tsx
  ```

  Expected: TypeScript/Vitest fails because `ru`, `I18N.ru`, `WEB_LOCALE`, and
  `lang-ru` do not exist.

- [ ] **Step 3: Add the minimum complete three-language implementation.**

  In `i18n.ts`, make the language value explicit and centralize browser locale
  selection:

  ```ts
  export type Lang = "ru" | "zh" | "en";

  export const WEB_LOCALE: Record<Lang, string> = {
    ru: "ru-RU",
    zh: "zh-CN",
    en: "en-US",
  };
  ```

  Add `ru: { ... }` with every key that exists in `zh` and `en`. Translate the
  user action, security, scheduling, DSH, team, access, marketplace, and
  charter copy in natural Russian; preserve every `--flag`, `/command`,
  `{placeholder}`, identifier, and vendor name inside a translated sentence.
  Convert `NAV_LABELS` to `Record<string, Record<Lang, string>>` and replace
  its binary resolver with an explicit three-language lookup. Keep the current
  two-string `tr` signature temporarily: its callers are converted atomically
  with their Russian values in Task 2, so no intermediate build is broken. In
  `useWebSettings.ts`, widen
  `WebSettings.language` to `Lang` and set only `getDefaults().language` to
  `"ru"`; do not rewrite an existing localStorage value.

- [ ] **Step 4: Run the focused tests and observe GREEN.**

  Run the command from Step 2. Expected: all selected Vitest files pass and
  both legacy language controls still render.

- [ ] **Step 5: Commit the self-contained contract change.**

  ```bash
  git add crates/ccteam-web/web/src/lib/i18n.ts \
    crates/ccteam-web/web/src/hooks/useWebSettings.ts \
    crates/ccteam-web/web/src/lib/i18n.test.ts \
    crates/ccteam-web/web/src/pages/SettingsView.test.tsx \
    crates/ccteam-web/web/src/components/AvatarMenu.test.tsx
  git commit -m "feat: add Russian web locale"
  ```

### Task 2: Route every remaining web control and formatter through `Lang`

**Files:**

- Modify: `crates/ccteam-web/web/src/lib/i18n.ts`
- Modify: `crates/ccteam-web/web/src/pages/{HomeView,SessionView,ChatConsole,WorkflowView,SettingsView,MarketplaceView}.tsx`
- Modify: `crates/ccteam-web/web/src/pages/{railHelpers,railHelpers.test,SessionView.test}.ts*`
- Modify: `crates/ccteam-web/web/src/lib/{quotaBars,quotaBars.test}.ts`
- Modify: `crates/ccteam-web/web/src/components/{BackToLiveButton,MobileTerminalToolbar,TerminalView,Toasts,TokenEntryPage,AvatarMenu}.tsx`
- Modify: `crates/ccteam-web/web/src/components/ui/{dialog,table,combobox}.tsx`
- Modify: the co-located `*.test.tsx` files for each changed page/component.

**Interfaces:**

- Consumes: `Lang`, `makeT`, `tr`, and `WEB_LOCALE` from Task 1.
- Produces: all ccteam-owned text rendered by these components respects the
  active language; formatting functions accept `Lang`, not `"zh" | "en"`.

- [ ] **Step 1: Add RED tests for Russian tail surfaces.**

  Extend existing tests rather than creating a parallel test framework:

  ```ts
  expect(relativeTime("ru", secondsAgo(5 * 60))).toBe("5 мин назад");
  expect(resetHint("2026-08-17T15:12:00Z", NOW, "ru")).toBe("сброс через 3 ч 12 мин");
  expect(renderToString(<GeneralPanel lang="ru" theme="light" onLang={noop} onTheme={noop} />))
    .toContain("Русский");
  ```

  Add one SSR assertion per changed generic control: Russian dismiss/close,
  terminal reconnect, mobile-toolbar accessibility, empty combobox, and sort
  labels. Add a `SessionView` assertion that formatted time uses `ru-RU` and
  Russian "load earlier" / "back to latest" text.

- [ ] **Step 2: Run the selected tests and observe RED.**

  Run:

  ```bash
  cd crates/ccteam-web/web && npm run test:unit -- \
    src/pages/railHelpers.test.ts src/lib/quotaBars.test.ts \
    src/pages/SessionView.test.tsx src/components/AvatarMenu.test.tsx \
    src/components/Sidebar.test.tsx src/components/ui/ui.test.tsx
  ```

  Expected: the Russian literal and `ru` function arguments are unsupported.

- [ ] **Step 3: Make the smallest locale plumbing changes.**

  Change `tr` to require `tr(lang, zh, en, ru)` and update every existing
  caller in the same change. Then replace every `lang === "en" ? ... : ...`
  and `"zh-CN"` fallback in the
  listed user-facing source files with `t(...)`, `tr(..., ru)`, or
  `WEB_LOCALE[lang]`. Preserve timestamps and numeric values; only pass the
  locale argument to `Intl`:

  ```ts
  when.toLocaleString(WEB_LOCALE[lang], { hour12: false, ...parts })
  ```

  Add a required `lang: Lang` prop only to controls that need an active label
  (`TerminalView`, `MobileTerminalToolbar`, `BackToLiveButton`, `Toasts`,
  `Dialog`, `SortableHeader`, and `Combobox`) and thread it from the existing
  parent that already knows `lang`. Do not add a global context or a new
  dependency. Preserve the existing test ids, keyboard behavior, ARIA roles,
  and `onClose`/`onChange` interfaces.

  For static login text before the settings shell loads, use the Russian
  ccteam-authored copy in `TokenEntryPage`; commands such as `ccteam status`
  remain literal code. Review every hit from this audit after edits:

  ```bash
  rg -n 'lang\s*===\s*"en"|lang\s*!==\s*"en"|"zh-CN"|"en-US"|aria-label="(Dismiss|关闭|排序|Arrow|Back to live)' crates/ccteam-web/web/src
  ```

  Each remaining hit must be a deliberate non-human token or a three-language
  branch; otherwise move it to `i18n.ts` before proceeding.

- [ ] **Step 4: Run the selected tests and observe GREEN.**

  Re-run Step 2. Then run the full web static gate:

  ```bash
  cd crates/ccteam-web/web && npm run lint && npm run build && npm run test:unit
  ```

  Expected: ESLint, `tsc -b`, Vite, and all unit tests exit 0.

- [ ] **Step 5: Commit the web-tail change.**

  ```bash
  git add crates/ccteam-web/web/src
  git commit -m "feat: localize web controls in Russian"
  ```

### Task 3: Translate the Telegram control surface without changing commands

**Files:**

- Modify: `crates/ccteam-im/src/gateway.rs:1715-1867,3295-3874,4000-9200,14092-14105`
- Modify: `crates/ccteam-im/src/{onboarding,outbound_format,progress,scheduled}.rs`
- Modify: `crates/ccteam-im/src/transport/providers/{telegram,mod}.rs`
- Test: `crates/ccteam-im/src/gateway.rs:14320-24030`
- Test: `crates/ccteam-im/src/transport/providers/telegram.rs:874-930`

**Interfaces:**

- Consumes: `GATEWAY_COMMANDS`, `menu_command_specs()`, `render_help()`, and
  existing command handlers.
- Produces: the same command names/argument hints and reply structure, with
  Russian ccteam-owned descriptions, receipts, next hints, validation errors,
  status/project/session headings, and scheduled-message messages.

- [ ] **Step 1: Add RED assertions at the command-table seam.**

  In gateway unit tests, assert both the immutable grammar and translated
  menu/help text:

  ```rust
  #[test]
  fn telegram_menu_keeps_tokens_and_uses_russian_copy() {
      let specs = menu_command_specs();
      assert!(specs.iter().any(|spec| spec.name == "/projects"));
      assert!(specs.iter().any(|spec| spec.name == "/new"));
      assert!(specs.iter().all(|spec| !spec.description.contains("list projects")));
      assert!(render_help().contains("Команды шлюза:"));
  }
  ```

  Convert selected existing expected replies (`/help`, `/projects`, `/sessions`,
  `/use`, a malformed `/new`, and a scheduled inbox receipt) to Russian while
  retaining the exact `/command`, `sid`, slug, and model tokens.

- [ ] **Step 2: Run the focused Rust tests and observe RED.**

  Run:

  ```bash
  cargo test -p ccteam-im gateway::tests::telegram_menu_keeps_tokens_and_uses_russian_copy -- --exact
  cargo test -p ccteam-im gateway::tests::new_command_help_advertises_model_and_effort -- --exact
  ```

  Expected: the new assertion fails on English menu/help copy; existing test
  remains a grammar guard.

- [ ] **Step 3: Translate only ccteam-authored IM literals.**

  Translate the `help` values in `GATEWAY_COMMANDS`, `NEXT_HINT_*`,
  `render_help()` title, all command receipts/validation `anyhow!` strings,
  project/session/status render headings, onboarding notices, progress labels,
  scheduled-message text, and Telegram API command descriptions. Leave:

  ```rust
  "/new", "/projects", "/sessions", "/use", "/model", "model=<id>",
  "effort=<level>", sid values, project slugs, callback payloads, and vendor output
  ```

  unchanged. Do not add a chat language preference: this operator bot is
  Russian by design. Review all real sender paths, not comments/tests only:

  ```bash
  rg -n -P '(anyhow!\(|bail!\(|format!\(|String::from\(|help:|NEXT_HINT)' crates/ccteam-im/src
  ```

- [ ] **Step 4: Run focused and package tests.**

  Run:

  ```bash
  cargo test -p ccteam-im gateway::tests --lib -- --nocapture
  cargo test -p ccteam-im transport::providers::telegram::tests --lib -- --nocapture
  ```

  Expected: all targeted tests pass; no test expectation changes a slash token
  or callback payload.

- [ ] **Step 5: Commit the Telegram localization.**

  ```bash
  git add crates/ccteam-im/src
  git commit -m "feat: localize Telegram control responses"
  ```

### Task 4: Translate CLI help and ccteam-authored diagnostics

**Files:**

- Modify: `crates/ccteam-cli/src/main.rs:49-630,2531-2561`
- Modify: `crates/ccteam-cli/src/{commands,daemon_cli,doctor,legacy_takeover,update,clipboard}.rs`
- Test: `crates/ccteam-cli/src/main.rs:2531-2561`
- Test: existing unit modules in `commands.rs:3642`, `doctor.rs:1022`, and
  `daemon_cli.rs:559` whose expected output changes.

**Interfaces:**

- Consumes: Clap derive metadata and existing `Result<String>` command output.
- Produces: Russian `ccteam --help`, subcommand/argument descriptions, setup,
  doctor, daemon, hub, skill, and project diagnostics while retaining every
  parseable token and `OutputFormat::Json` payload.

- [ ] **Step 1: Write RED help and JSON-boundary tests.**

  Inside `main.rs`'s existing test module, add:

  ```rust
  #[test]
  fn cli_help_is_russian_but_flags_remain_stable() {
      use clap::CommandFactory;
      let help = Cli::command().render_help().to_string();
      assert!(help.contains("Командный мост"));
      assert!(help.contains("--home"));
      assert!(help.contains("init"));
  }
  ```

  In an existing `commands.rs` test, assert a translated human report retains
  an interpolated project slug and add a sibling assertion that
  `OutputFormat::Json` remains parsed as JSON with its existing field names.

- [ ] **Step 2: Run the selected tests and observe RED.**

  Run:

  ```bash
  cargo test -p ccteam-cli cli_help_is_russian_but_flags_remain_stable --bin ccteam -- --exact
  ```

  Expected: help contains the present English `about` string and the Russian
  assertion fails.

- [ ] **Step 3: Translate the owned CLI presentation layer.**

  Translate every Clap doc comment and `#[command(about = ...)]` text in
  `main.rs`, plus static ccteam strings passed to `println!`, `eprintln!`,
  `format!`, `anyhow!`, `bail!`, and `with_context` in the listed CLI files.
  Retain command examples, flag spellings, placeholders such as `<slug>`,
  filesystem paths, URLs, error values (`{err}`), and all JSON branch strings.
  Never translate output delegated from a spawned program or vendor stderr.

  Verify the intended boundary with both a human and machine invocation:

  ```bash
  cargo run -p ccteam-cli -- --help | rg 'Командный мост|--home|init'
  cargo run -p ccteam-cli -- doctor --verify-mcp --json | jq type
  ```

- [ ] **Step 4: Run the CLI package tests and formatter.**

  Run:

  ```bash
  cargo test -p ccteam-cli --bin ccteam -- --nocapture
  cargo fmt --all -- --check
  ```

  Expected: all CLI tests pass, JSON is valid, and Rust formatting is clean.

- [ ] **Step 5: Commit the CLI localization.**

  ```bash
  git add crates/ccteam-cli/src
  git commit -m "feat: localize CLI copy in Russian"
  ```

### Task 5: Perform the full language audit, release-quality gate, and local deployment

**Files:**

- Modify only if a missed ccteam-owned literal is found: files enumerated in
  Tasks 1-4.
- Test: all changed web, IM, and CLI tests.

**Interfaces:**

- Consumes: the commits from Tasks 1-4 and the existing `make gate`/
  `make install` workflow.
- Produces: a verified source-built `/home/gon71/.local/bin/ccteam` and a
  restarted loopback daemon serving the embedded Russian web bundle and the
  Russian Telegram menu.

- [ ] **Step 1: Run a final owned-copy audit before the full gate.**

  Inspect every remaining static presentation-string producer and classify it
  as translated ccteam copy, an allowed protocol token, a test fixture, a
  comment, or forwarded external text:

  ```bash
  rg -n -P '(anyhow!\(|bail!\(|println!\(|eprintln!\(|format!\(|String::from\(|about\s*=|aria-label=|title=)' \
    crates/ccteam-web/web/src crates/ccteam-im/src crates/ccteam-cli/src
  rg -n 'lang\s*===\s*"en"|lang\s*!==\s*"en"|"zh-CN"|"en-US"' crates/ccteam-web/web/src
  ```

  Translate any missed ccteam-owned user text immediately and add a focused
  assertion beside its existing component/command test before rerunning that
  test. Do not change external/vendor payloads merely because they are English.

- [ ] **Step 2: Run the complete source gates serially.**

  Run:

  ```bash
  make web-check
  make test-baseline
  make test-web
  cargo clippy --workspace --all-targets --locked -- -D warnings
  cargo fmt --all -- --check
  ```

  Expected: every command exits 0. If a registered WSL PTY flake recurs, save
  the exact failing test and compare it to `.loop/verify/README.md`; do not
  relabel a new localization failure as environment noise.

- [ ] **Step 3: Commit the audit-only fixes and prepare deployment.**

  ```bash
  git status --short
  git diff --check
  if ! git diff --quiet; then
    git add crates/ccteam-web/web/src crates/ccteam-im/src crates/ccteam-cli/src
    git commit -m "fix: complete Russian localization"
  fi
  ```

  Before changing the live binary, record its current version and verify the
  source worktree is clean:

  ```bash
  /home/gon71/.local/bin/ccteam --version
  git status --short
  ```

- [ ] **Step 4: Build, atomically install, restart, and health-check.**

  Run the repository-owned installer only after Step 2 is green:

  ```bash
  make install
  /home/gon71/.local/bin/ccteam daemon status
  curl --fail --silent --show-error http://127.0.0.1:7331/api/v1/config/im >/dev/null
  /home/gon71/.local/bin/ccteam --help | rg 'Командный мост|--home|init'
  ```

  Expected: `make install` creates the release bundle, replaces the binary via
  its atomic temp-file move, restarts the managed daemon, and the loopback
  health/config endpoint and Russian CLI help succeed. Do not print the IM
  response or any credential.

- [ ] **Step 5: Deliver the branch through the required Git path.**

  Fetch once more, integrate the tested feature branch into a clean `dev`
  worktree only if its remote base did not move, then push `dev`:

  ```bash
  git fetch --prune origin
  git merge-base --is-ancestor origin/dev codex/russian-localization
  git worktree add -b russian-localization-integrate /tmp/ccteam-ru-integrate origin/dev
  git -C /tmp/ccteam-ru-integrate merge --no-ff codex/russian-localization -m "Merge Russian localization"
  git -C /tmp/ccteam-ru-integrate push origin HEAD:dev
  git worktree remove /tmp/ccteam-ru-integrate
  ```

  If the ancestry check fails, stop and merge the new `origin/dev` into the
  feature branch, then rerun the focused tests before integration. Query an
  existing `dev → main` pull request with `gh pr list --base main --head dev`;
  create a draft only if none exists. Report the feature SHA, deployed SHA, and
  PR state. Never force-push or merge `dev` into `main`: main integration is
  owner-controlled.
