# Security

fuckmemory stores your prompts on disk and exposes a memory API to your coding
agents. It is local-first by design: no data leaves the machine unless you
export it yourself.

## Reporting a vulnerability

Do **not** open a public issue for a vulnerability. Report it privately:

- GitHub security advisories: open a private report at
  https://github.com/dalsori/fuckmemory/security/advisories/new
- or email the maintainer: dalsori@clainev.com

Include the affected version, a description of the issue, and — if you have one
— a minimal reproduction. You should receive a response within 5 business days.

## Scope

What this project considers in scope:

- **Credential leakage to disk.** Autosave (`src/hook.rs`) redacts tokens,
  assignments to secret-ish keys and opaque blobs before storing a prompt.
  The redaction is heuristic — it is a safety net, not a guarantee, and the
  README says so. An improvement to the redactor is a security contribution.
- **Prompt injection through stored memory.** Recalled memories are injected
  into an agent's context; a fact that tricks the agent into harmful actions is
  a vulnerability.
- **Privilege issues in `install`/`uninstall`** that modify or destroy agent
  configs beyond what they advertise.

Out of scope: a physical attacker with access to your account and your
`FUCKMEMORY_HOME` directory, who can already read the SQLite store directly.

## Supported versions

Only the latest release is supported.
