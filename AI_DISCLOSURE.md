# AI disclosure

This file says how AI tools were used to make LiveDeck. It follows the categories of the
[W3C AI Content Disclosure Community Group](https://www.w3.org/community/ai-content-disclosure).
That group's work is a draft: it is not a W3C Recommendation, and its disclosure values are
still under discussion. This file therefore uses the group's descriptive categories in plain
words rather than any machine-readable value.

Last updated: 2026-11-08

## Level

**AI-authored, with human review.** An AI model wrote most of the code, tests and
documentation edits in this repository. A human drove the architecture: the crate layout, the
design of each feature and the decisions about what the app should do. A human also directed
each change, reviewed every change before it was committed, and ran the tests.

The categories the group describes, from most to least human involvement, are:

1. Entirely human-authored.
2. AI-assisted: a human wrote the content and used AI for parts of it.
3. AI-authored, with human review (this project).
4. Entirely automatically created, with no human oversight.

## AI tools

- **Claude Haiku 5.5** (Anthropic, model ID `claude-haiku-5-5`), used through Claude Code in
  the terminal.

If another model or tool was used for part of the history, this file should name it. The
maintainer is responsible for keeping it accurate.

## Human-driven workflow

1. The maintainer drives the architecture: the crate layout, the module boundaries and the
   design of each feature. The maintainer also chooses what to change and how it should work.
   Requests are written by a human, and design decisions are made by a human.
2. The AI makes the edits in the working tree and reports what it changed and what it could
   not verify.
3. The maintainer reviews the diff before anything is committed.
4. Tests must pass before a change is committed. The project runs `cargo test --workspace`
   and `cargo fmt --all -- --check`.
5. Commits are made by the maintainer, under the maintainer's name, with a message the
   maintainer approves. No AI attribution is added to commits.
6. The AI does not push, tag, publish a release or merge. The maintainer does those.

## Responsibility

The maintainer is responsible for everything in this repository, including code, tests,
documentation and release notes that were written with AI help. Problems found in AI-written
code are fixed the same way as any other bug.
