# Russian localization design

## Goal

Make every ccteam-owned, human-facing surface available in Russian. Russian is
the default for fresh web profiles; existing saved Chinese and English choices
continue to work.

## Scope

Included:

- the web shell, pages, labels, accessibility text, notifications, relative
  time, and date/number locale;
- Telegram command-menu descriptions, `/help`, command receipts, and ccteam
  validation errors;
- CLI help and ccteam-authored diagnostics.

Excluded:

- slash-command names, CLI subcommands and flags, API/JSON keys and values,
  file paths, vendor/model identifiers, and code snippets;
- raw terminal output, vendor responses, user-entered text, and upstream
  errors that ccteam only forwards.

## Design

Extend the existing web `Lang` union and dictionary from `zh | en` to
`ru | zh | en`. Keep one key set for all three dictionaries and make a missing
key fall back to Chinese only as a visible development fault. Replace the
remaining two-language ternaries and hard-coded strings with the same locale
source, including `ru-RU` formatting. The settings page and avatar menu expose
all three languages. New browser settings default to `ru`; a saved `zh` or `en`
value is preserved unchanged.

Telegram remains a single Russian operator bot rather than gaining per-chat
language configuration. Its command tokens and argument grammar stay ASCII and
stable; only their descriptions and all ccteam-owned replies are translated.
CLI command grammar likewise remains stable while Clap metadata and ccteam
diagnostics become Russian.

## Verification

- Add a red-first web test that proves the Russian dictionary has the same key
  set as Chinese and English, default settings select `ru`, and representative
  formatter output uses Russian.
- Extend existing web component tests to prove Russian language controls and
  representative user-visible UI copy.
- Extend Telegram gateway tests to prove command names stay unchanged while
  menu/help text is Russian.
- Add focused CLI assertions for Russian help/diagnostic copy where tests
  already cover the command.
- Run the affected Vitest and Rust tests, type/lint checks, formatter, then
  rebuild the local binary and restart the loopback daemon only after those
  gates pass.

## Delivery

Work is made on `dev` through branch `codex/russian-localization`. The local
deployment is updated only from the verified commit. No external release,
Telegram credential change, or API contract change is part of this work.
